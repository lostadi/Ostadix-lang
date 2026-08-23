//! One-time passcode pairing for mutually authenticated LAN nodes.
//!
//! The pairing channel carries only public identity material, CSRs, and
//! destination-issued client certificates. Private client keys remain local.
//! A SPAKE2 exchange turns the short, single-use passcode into session keys;
//! explicit directional HMAC confirmations bind both node identities, both
//! public bundles, both SPAKE messages, and both issued certificates.

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

pub const LAN_PAIRING_SCHEMA_V1: &str = "ostadix.lan-pairing/v1";
pub const LAN_PAIRING_SUITE_V1: &str = "spake2-ed25519+hkdf-sha256+hmac-sha256";
pub const DEFAULT_LAN_PAIRING_PORT: u16 = 7340;
pub const MAX_LAN_PAIRING_FRAME_BYTES: usize = 512 * 1024;
pub const DEFAULT_LAN_PAIRING_TIMEOUT: Duration = Duration::from_secs(60);

const PAIRING_AUTHENTICATION_FAILURE: &str = "pairing authentication failed";
const PAIRING_HKDF_DOMAIN_V1: &[u8] = b"OSTADIX/LAN-PAIRING/HKDF/V1\0";
const PAIRING_CONFIRMATION_DOMAIN_V1: &[u8] = b"OSTADIX/LAN-PAIRING/CONFIRM/V1\0";
const PRIVATE_KEY_MARKER: &[u8] = b"PRIVATE KEY";
const MAX_PUBLIC_FIELD_BYTES: usize = 256 * 1024;
const PAIRING_CODE_MODULUS: u64 = 10_000_000_000;

type HmacSha256 = Hmac<Sha256>;

/// Public material bound into a one-time pairing transcript.
///
/// `server_ca_pem` authenticates this node when the peer later connects to it.
/// `client_issuer_ca_pem` authenticates the client certificate this node issues
/// for the peer's locally held private key. The two CAs are deliberately
/// distinct protocol coordinates even if an operator provisions equal values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingPublicIdentityV1 {
    pub node_id: String,
    pub server_name: String,
    pub service_port: u16,
    pub supports_v2: bool,
    pub server_ca_pem: String,
    pub client_issuer_ca_pem: String,
    pub client_csr_pem: String,
    pub node_receipt_public_key: String,
}

impl PairingPublicIdentityV1 {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("node_id", &self.node_id)?;
        validate_server_name(&self.server_name)?;
        if self.service_port == 0 {
            bail!("pairing service port must be nonzero");
        }
        if !self.supports_v2 {
            bail!("paired nodes must expose a durable V2 receipt identity");
        }
        require_public_pem("server CA", &self.server_ca_pem, "CERTIFICATE")?;
        require_public_pem(
            "client issuer CA",
            &self.client_issuer_ca_pem,
            "CERTIFICATE",
        )?;
        require_csr_pem(&self.client_csr_pem)?;
        validate_lower_hex("node receipt public key", &self.node_receipt_public_key, 64)
    }
}

/// A pairing identity whose client private key never implements serialization
/// and is zeroized when the value is dropped.
pub struct PairingLocalIdentityV1 {
    public: PairingPublicIdentityV1,
    private_client_key_pem: Zeroizing<Vec<u8>>,
}

impl PairingLocalIdentityV1 {
    pub fn new(public: PairingPublicIdentityV1, private_client_key_pem: Vec<u8>) -> Result<Self> {
        public.validate()?;
        validate_private_client_key(&private_client_key_pem)?;
        Ok(Self {
            public,
            private_client_key_pem: Zeroizing::new(private_client_key_pem),
        })
    }

    pub fn public(&self) -> &PairingPublicIdentityV1 {
        &self.public
    }

    pub fn private_client_key_pem(&self) -> &[u8] {
        self.private_client_key_pem.as_slice()
    }
}

impl fmt::Debug for PairingLocalIdentityV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingLocalIdentityV1")
            .field("public", &self.public)
            .field("private_client_key_pem", &"[redacted]")
            .finish()
    }
}

/// Fully confirmed material to persist for automatic future connections.
pub struct PairingResultV1 {
    pub peer: PairingPublicIdentityV1,
    pub local_issued_client_cert_pem: String,
    local_private_client_key_pem: Zeroizing<Vec<u8>>,
}

impl PairingResultV1 {
    fn new(
        peer: PairingPublicIdentityV1,
        local_issued_client_cert_pem: String,
        local_identity: PairingLocalIdentityV1,
    ) -> Self {
        let PairingLocalIdentityV1 {
            public: _,
            private_client_key_pem,
        } = local_identity;
        Self {
            peer,
            local_issued_client_cert_pem,
            local_private_client_key_pem: private_client_key_pem,
        }
    }

    pub fn local_private_client_key_pem(&self) -> &[u8] {
        self.local_private_client_key_pem.as_slice()
    }
}

impl fmt::Debug for PairingResultV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingResultV1")
            .field("peer", &self.peer)
            .field(
                "local_issued_client_cert_pem",
                &self.local_issued_client_cert_pem,
            )
            .field("local_private_client_key_pem", &"[redacted]")
            .finish()
    }
}

/// Generate ten uniformly distributed decimal digits, grouped for transcription.
pub fn generate_pairing_passcode() -> Result<String> {
    // Accept a whole number of 10^10-sized buckets. This avoids modulo bias
    // while retaining all ten decimal digits, including leading zeroes.
    let acceptance_limit = u64::MAX - (u64::MAX % PAIRING_CODE_MODULUS);
    loop {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).context("failed to obtain pairing passcode entropy")?;
        let candidate = u64::from_le_bytes(bytes);
        if candidate >= acceptance_limit {
            continue;
        }
        let value = candidate % PAIRING_CODE_MODULUS;
        return Ok(format!("{:05}-{:05}", value / 100_000, value % 100_000));
    }
}

/// Parse exactly ten digits, optionally separated by one middle hyphen.
pub fn parse_pairing_passcode(value: &str) -> Result<Zeroizing<String>> {
    let bytes = value.as_bytes();
    let valid_plain = bytes.len() == 10 && bytes.iter().all(u8::is_ascii_digit);
    let valid_grouped = bytes.len() == 11
        && bytes[5] == b'-'
        && bytes[..5].iter().all(u8::is_ascii_digit)
        && bytes[6..].iter().all(u8::is_ascii_digit);
    if !valid_plain && !valid_grouped {
        bail!("pairing passcode must be exactly ten digits, optionally formatted 00000-00000");
    }
    let normalized = value
        .bytes()
        .filter(|byte| *byte != b'-')
        .collect::<Vec<_>>();
    Ok(Zeroizing::new(
        String::from_utf8(normalized).expect("validated passcode is ASCII"),
    ))
}

/// Accept one passcode-authenticated pairing exchange as role B (the offerer).
///
/// The local-identity callback runs only after the joiner's public identity is
/// known, allowing a fresh per-peer private key and CSR. The signing callback
/// must issue a client certificate for the CSR in its argument using this
/// offerer's client-issuer CA.
pub fn accept_pairing_once<LocalIdentity, SignPeerCsr>(
    mut stream: TcpStream,
    passcode: &str,
    offer_node_id: &str,
    timeout: Duration,
    local_identity_for_peer: LocalIdentity,
    sign_peer_csr: SignPeerCsr,
) -> Result<PairingResultV1>
where
    LocalIdentity: FnOnce(&PairingPublicIdentityV1) -> Result<PairingLocalIdentityV1>,
    SignPeerCsr: FnOnce(&PairingPublicIdentityV1) -> Result<String>,
{
    validate_identifier("offer_node_id", offer_node_id)?;
    let passcode = parse_pairing_passcode(passcode)?;
    configure_pairing_stream(&stream, timeout)?;

    let offer_nonce = fresh_offer_nonce()?;
    write_frame(
        &mut stream,
        &PairingFrameV1::ServerHello {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: offer_nonce.clone(),
            offer_node_id: offer_node_id.to_owned(),
        },
    )?;

    let (joiner, pake_a) = match read_frame(&mut stream)? {
        PairingFrameV1::JoinHello {
            schema,
            suite,
            offer_nonce: received_nonce,
            joiner,
            pake_a,
        } => {
            validate_header(&schema, &suite, &received_nonce, &offer_nonce)?;
            joiner.validate()?;
            (joiner, decode_pake_message(&pake_a)?)
        }
        _ => bail!("pairing peer sent an unexpected frame before join hello"),
    };
    if joiner.node_id == offer_node_id {
        return Err(authentication_failure());
    }

    let local_identity = local_identity_for_peer(&joiner)
        .context("failed to prepare the offerer's per-peer identity")?;
    if local_identity.public().node_id != offer_node_id {
        bail!("pairing local identity does not match offer_node_id");
    }
    local_identity.public().validate()?;

    let (id_a, id_b) = spake_identities(&offer_nonce, &joiner.node_id, offer_node_id);
    let (spake, pake_b) = Spake2::<Ed25519Group>::start_b(
        &Password::new(passcode.as_bytes()),
        &Identity::new(&id_a),
        &Identity::new(&id_b),
    );
    let shared = Zeroizing::new(
        spake
            .finish(&pake_a)
            .map_err(|_| authentication_failure())?,
    );
    let pake_a_b64 = STANDARD_NO_PAD.encode(&pake_a);
    let pake_b_b64 = STANDARD_NO_PAD.encode(&pake_b);
    let transcript = canonical_transcript(
        &offer_nonce,
        &joiner,
        local_identity.public(),
        &pake_a_b64,
        &pake_b_b64,
    )?;
    let keys = derive_confirmation_keys(&shared, &offer_nonce, &transcript)?;
    let offer_confirmation = confirmation_tag(
        keys.offer_to_join.as_slice(),
        "offer-auth",
        &transcript,
        None,
        None,
    )?;
    write_frame(
        &mut stream,
        &PairingFrameV1::OfferAuth {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: offer_nonce.clone(),
            offer: local_identity.public().clone(),
            pake_b: pake_b_b64,
            confirmation: offer_confirmation,
        },
    )?;

    let certificate_for_offer = match read_frame(&mut stream)? {
        PairingFrameV1::JoinAuth {
            schema,
            suite,
            offer_nonce: received_nonce,
            certificate_for_offer,
            confirmation,
        } => {
            validate_header(&schema, &suite, &received_nonce, &offer_nonce)?;
            require_public_pem(
                "client certificate for offerer",
                &certificate_for_offer,
                "CERTIFICATE",
            )?;
            verify_confirmation(
                keys.join_to_offer.as_slice(),
                "join-auth",
                &transcript,
                Some(&certificate_for_offer),
                None,
                &confirmation,
            )?;
            certificate_for_offer
        }
        _ => bail!("pairing peer sent an unexpected frame before join authentication"),
    };

    let certificate_for_joiner =
        sign_peer_csr(&joiner).context("failed to issue the joiner's paired client certificate")?;
    require_public_pem(
        "client certificate for joiner",
        &certificate_for_joiner,
        "CERTIFICATE",
    )?;
    let commit_confirmation = confirmation_tag(
        keys.offer_to_join.as_slice(),
        "commit",
        &transcript,
        Some(&certificate_for_offer),
        Some(&certificate_for_joiner),
    )?;
    write_frame(
        &mut stream,
        &PairingFrameV1::Commit {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: offer_nonce.clone(),
            certificate_for_joiner: certificate_for_joiner.clone(),
            confirmation: commit_confirmation,
        },
    )?;

    match read_frame(&mut stream)? {
        PairingFrameV1::Ack {
            schema,
            suite,
            offer_nonce: received_nonce,
            confirmation,
        } => {
            validate_header(&schema, &suite, &received_nonce, &offer_nonce)?;
            verify_confirmation(
                keys.join_to_offer.as_slice(),
                "ack",
                &transcript,
                Some(&certificate_for_offer),
                Some(&certificate_for_joiner),
                &confirmation,
            )?;
        }
        _ => bail!("pairing peer sent an unexpected frame before commit acknowledgement"),
    }

    let done_confirmation = confirmation_tag(
        keys.offer_to_join.as_slice(),
        "done",
        &transcript,
        Some(&certificate_for_offer),
        Some(&certificate_for_joiner),
    )?;
    write_frame(
        &mut stream,
        &PairingFrameV1::Done {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce,
            confirmation: done_confirmation,
        },
    )?;

    Ok(PairingResultV1::new(
        joiner,
        certificate_for_offer,
        local_identity,
    ))
}

/// Join one passcode-authenticated pairing exchange as role A.
///
/// The signing callback must issue a client certificate for the offerer's CSR
/// using the joiner's local client-issuer CA.
pub fn join_pairing_once<SignPeerCsr>(
    mut stream: TcpStream,
    passcode: &str,
    expected_offer_node_id: &str,
    timeout: Duration,
    local_identity: PairingLocalIdentityV1,
    sign_peer_csr: SignPeerCsr,
) -> Result<PairingResultV1>
where
    SignPeerCsr: FnOnce(&PairingPublicIdentityV1) -> Result<String>,
{
    validate_identifier("expected_offer_node_id", expected_offer_node_id)?;
    local_identity.public().validate()?;
    if local_identity.public().node_id == expected_offer_node_id {
        return Err(authentication_failure());
    }
    let passcode = parse_pairing_passcode(passcode)?;
    configure_pairing_stream(&stream, timeout)?;

    let offer_nonce = match read_frame(&mut stream)? {
        PairingFrameV1::ServerHello {
            schema,
            suite,
            offer_nonce,
            offer_node_id,
        } => {
            validate_header(&schema, &suite, &offer_nonce, &offer_nonce)?;
            validate_lower_hex("offer nonce", &offer_nonce, 32)?;
            if offer_node_id != expected_offer_node_id {
                return Err(authentication_failure());
            }
            offer_nonce
        }
        _ => bail!("pairing peer sent an unexpected initial frame"),
    };

    let (id_a, id_b) = spake_identities(
        &offer_nonce,
        &local_identity.public().node_id,
        expected_offer_node_id,
    );
    let (spake, pake_a) = Spake2::<Ed25519Group>::start_a(
        &Password::new(passcode.as_bytes()),
        &Identity::new(&id_a),
        &Identity::new(&id_b),
    );
    let pake_a_b64 = STANDARD_NO_PAD.encode(&pake_a);
    write_frame(
        &mut stream,
        &PairingFrameV1::JoinHello {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: offer_nonce.clone(),
            joiner: local_identity.public().clone(),
            pake_a: pake_a_b64.clone(),
        },
    )?;

    let (offer, pake_b, offer_confirmation) = match read_frame(&mut stream)? {
        PairingFrameV1::OfferAuth {
            schema,
            suite,
            offer_nonce: received_nonce,
            offer,
            pake_b,
            confirmation,
        } => {
            validate_header(&schema, &suite, &received_nonce, &offer_nonce)?;
            offer.validate()?;
            if offer.node_id != expected_offer_node_id {
                return Err(authentication_failure());
            }
            (offer, decode_pake_message(&pake_b)?, confirmation)
        }
        _ => bail!("pairing peer sent an unexpected frame before offer authentication"),
    };
    let shared = Zeroizing::new(
        spake
            .finish(&pake_b)
            .map_err(|_| authentication_failure())?,
    );
    let pake_b_b64 = STANDARD_NO_PAD.encode(&pake_b);
    let transcript = canonical_transcript(
        &offer_nonce,
        local_identity.public(),
        &offer,
        &pake_a_b64,
        &pake_b_b64,
    )?;
    let keys = derive_confirmation_keys(&shared, &offer_nonce, &transcript)?;
    verify_confirmation(
        keys.offer_to_join.as_slice(),
        "offer-auth",
        &transcript,
        None,
        None,
        &offer_confirmation,
    )?;

    let certificate_for_offer =
        sign_peer_csr(&offer).context("failed to issue the offerer's paired client certificate")?;
    require_public_pem(
        "client certificate for offerer",
        &certificate_for_offer,
        "CERTIFICATE",
    )?;
    let join_confirmation = confirmation_tag(
        keys.join_to_offer.as_slice(),
        "join-auth",
        &transcript,
        Some(&certificate_for_offer),
        None,
    )?;
    write_frame(
        &mut stream,
        &PairingFrameV1::JoinAuth {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: offer_nonce.clone(),
            certificate_for_offer: certificate_for_offer.clone(),
            confirmation: join_confirmation,
        },
    )?;

    let certificate_for_joiner = match read_frame(&mut stream)? {
        PairingFrameV1::Commit {
            schema,
            suite,
            offer_nonce: received_nonce,
            certificate_for_joiner,
            confirmation,
        } => {
            validate_header(&schema, &suite, &received_nonce, &offer_nonce)?;
            require_public_pem(
                "client certificate for joiner",
                &certificate_for_joiner,
                "CERTIFICATE",
            )?;
            verify_confirmation(
                keys.offer_to_join.as_slice(),
                "commit",
                &transcript,
                Some(&certificate_for_offer),
                Some(&certificate_for_joiner),
                &confirmation,
            )?;
            certificate_for_joiner
        }
        _ => bail!("pairing peer sent an unexpected frame before commit"),
    };

    let ack_confirmation = confirmation_tag(
        keys.join_to_offer.as_slice(),
        "ack",
        &transcript,
        Some(&certificate_for_offer),
        Some(&certificate_for_joiner),
    )?;
    write_frame(
        &mut stream,
        &PairingFrameV1::Ack {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: offer_nonce.clone(),
            confirmation: ack_confirmation,
        },
    )?;

    match read_frame(&mut stream)? {
        PairingFrameV1::Done {
            schema,
            suite,
            offer_nonce: received_nonce,
            confirmation,
        } => {
            validate_header(&schema, &suite, &received_nonce, &offer_nonce)?;
            verify_confirmation(
                keys.offer_to_join.as_slice(),
                "done",
                &transcript,
                Some(&certificate_for_offer),
                Some(&certificate_for_joiner),
                &confirmation,
            )?;
        }
        _ => bail!("pairing peer sent an unexpected frame before completion"),
    }

    Ok(PairingResultV1::new(
        offer,
        certificate_for_joiner,
        local_identity,
    ))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "kebab-case", deny_unknown_fields)]
enum PairingFrameV1 {
    ServerHello {
        schema: String,
        suite: String,
        offer_nonce: String,
        offer_node_id: String,
    },
    JoinHello {
        schema: String,
        suite: String,
        offer_nonce: String,
        joiner: PairingPublicIdentityV1,
        pake_a: String,
    },
    OfferAuth {
        schema: String,
        suite: String,
        offer_nonce: String,
        offer: PairingPublicIdentityV1,
        pake_b: String,
        confirmation: String,
    },
    JoinAuth {
        schema: String,
        suite: String,
        offer_nonce: String,
        certificate_for_offer: String,
        confirmation: String,
    },
    Commit {
        schema: String,
        suite: String,
        offer_nonce: String,
        certificate_for_joiner: String,
        confirmation: String,
    },
    Ack {
        schema: String,
        suite: String,
        offer_nonce: String,
        confirmation: String,
    },
    Done {
        schema: String,
        suite: String,
        offer_nonce: String,
        confirmation: String,
    },
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PairingTranscriptV1<'a> {
    schema: &'static str,
    suite: &'static str,
    offer_nonce: &'a str,
    joiner: &'a PairingPublicIdentityV1,
    offer: &'a PairingPublicIdentityV1,
    pake_a: &'a str,
    pake_b: &'a str,
}

struct ConfirmationKeys {
    offer_to_join: Zeroizing<[u8; 32]>,
    join_to_offer: Zeroizing<[u8; 32]>,
}

fn configure_pairing_stream(stream: &TcpStream, timeout: Duration) -> Result<()> {
    if timeout.is_zero() {
        bail!("pairing timeout must be positive");
    }
    // The CLI accepts from an expiring nonblocking listener. Some Unix
    // implementations propagate O_NONBLOCK to accepted sockets, so restore
    // blocking framed I/O before applying read/write deadlines.
    stream
        .set_nonblocking(false)
        .context("failed to restore blocking pairing stream I/O")?;
    stream
        .set_nodelay(true)
        .context("failed to enable TCP_NODELAY for pairing")?;
    stream
        .set_read_timeout(Some(timeout))
        .context("failed to set pairing read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("failed to set pairing write timeout")
}

fn fresh_offer_nonce() -> Result<String> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).context("failed to obtain pairing offer entropy")?;
    Ok(hex::encode(nonce))
}

fn spake_identities(
    offer_nonce: &str,
    joiner_node_id: &str,
    offer_node_id: &str,
) -> (Vec<u8>, Vec<u8>) {
    let mut identity_a = Vec::new();
    append_identity_field(&mut identity_a, PAIRING_HKDF_DOMAIN_V1);
    append_identity_field(&mut identity_a, LAN_PAIRING_SUITE_V1.as_bytes());
    append_identity_field(&mut identity_a, b"joiner/A");
    append_identity_field(&mut identity_a, offer_nonce.as_bytes());
    append_identity_field(&mut identity_a, joiner_node_id.as_bytes());
    append_identity_field(&mut identity_a, offer_node_id.as_bytes());

    let mut identity_b = Vec::new();
    append_identity_field(&mut identity_b, PAIRING_HKDF_DOMAIN_V1);
    append_identity_field(&mut identity_b, LAN_PAIRING_SUITE_V1.as_bytes());
    append_identity_field(&mut identity_b, b"offer/B");
    append_identity_field(&mut identity_b, offer_nonce.as_bytes());
    append_identity_field(&mut identity_b, joiner_node_id.as_bytes());
    append_identity_field(&mut identity_b, offer_node_id.as_bytes());
    (identity_a, identity_b)
}

fn append_identity_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u32).to_be_bytes());
    output.extend_from_slice(value);
}

fn canonical_transcript(
    offer_nonce: &str,
    joiner: &PairingPublicIdentityV1,
    offer: &PairingPublicIdentityV1,
    pake_a: &str,
    pake_b: &str,
) -> Result<Vec<u8>> {
    joiner.validate()?;
    offer.validate()?;
    serde_json::to_vec(&PairingTranscriptV1 {
        schema: LAN_PAIRING_SCHEMA_V1,
        suite: LAN_PAIRING_SUITE_V1,
        offer_nonce,
        joiner,
        offer,
        pake_a,
        pake_b,
    })
    .context("failed to encode canonical pairing transcript")
}

fn derive_confirmation_keys(
    shared: &[u8],
    offer_nonce: &str,
    transcript: &[u8],
) -> Result<ConfirmationKeys> {
    let mut salt_hasher = Sha256::new();
    salt_hasher.update(PAIRING_HKDF_DOMAIN_V1);
    salt_hasher.update(offer_nonce.as_bytes());
    let salt = salt_hasher.finalize();
    let transcript_sha256 = Sha256::digest(transcript);
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared);

    let mut offer_to_join = Zeroizing::new([0_u8; 32]);
    let mut offer_info = Vec::new();
    append_identity_field(&mut offer_info, PAIRING_HKDF_DOMAIN_V1);
    append_identity_field(&mut offer_info, b"offer-to-join");
    append_identity_field(&mut offer_info, &transcript_sha256);
    hkdf.expand(&offer_info, offer_to_join.as_mut())
        .map_err(|_| anyhow!("failed to derive offer-to-join pairing key"))?;

    let mut join_to_offer = Zeroizing::new([0_u8; 32]);
    let mut join_info = Vec::new();
    append_identity_field(&mut join_info, PAIRING_HKDF_DOMAIN_V1);
    append_identity_field(&mut join_info, b"join-to-offer");
    append_identity_field(&mut join_info, &transcript_sha256);
    hkdf.expand(&join_info, join_to_offer.as_mut())
        .map_err(|_| anyhow!("failed to derive join-to-offer pairing key"))?;
    Ok(ConfirmationKeys {
        offer_to_join,
        join_to_offer,
    })
}

fn confirmation_tag(
    key: &[u8],
    stage: &str,
    transcript: &[u8],
    certificate_for_offer: Option<&str>,
    certificate_for_joiner: Option<&str>,
) -> Result<String> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key)
        .map_err(|_| anyhow!("failed to initialize pairing confirmation"))?;
    update_mac_field(&mut mac, PAIRING_CONFIRMATION_DOMAIN_V1);
    update_mac_field(&mut mac, LAN_PAIRING_SCHEMA_V1.as_bytes());
    update_mac_field(&mut mac, LAN_PAIRING_SUITE_V1.as_bytes());
    update_mac_field(&mut mac, stage.as_bytes());
    update_mac_field(&mut mac, transcript);
    update_mac_field(
        &mut mac,
        certificate_for_offer.unwrap_or_default().as_bytes(),
    );
    update_mac_field(
        &mut mac,
        certificate_for_joiner.unwrap_or_default().as_bytes(),
    );
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn verify_confirmation(
    key: &[u8],
    stage: &str,
    transcript: &[u8],
    certificate_for_offer: Option<&str>,
    certificate_for_joiner: Option<&str>,
    received: &str,
) -> Result<()> {
    let received = decode_fixed_hex::<32>(received).map_err(|_| authentication_failure())?;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).map_err(|_| authentication_failure())?;
    update_mac_field(&mut mac, PAIRING_CONFIRMATION_DOMAIN_V1);
    update_mac_field(&mut mac, LAN_PAIRING_SCHEMA_V1.as_bytes());
    update_mac_field(&mut mac, LAN_PAIRING_SUITE_V1.as_bytes());
    update_mac_field(&mut mac, stage.as_bytes());
    update_mac_field(&mut mac, transcript);
    update_mac_field(
        &mut mac,
        certificate_for_offer.unwrap_or_default().as_bytes(),
    );
    update_mac_field(
        &mut mac,
        certificate_for_joiner.unwrap_or_default().as_bytes(),
    );
    mac.verify_slice(&received)
        .map_err(|_| authentication_failure())
}

fn update_mac_field(mac: &mut HmacSha256, value: &[u8]) {
    mac.update(&(value.len() as u64).to_be_bytes());
    mac.update(value);
}

fn authentication_failure() -> anyhow::Error {
    anyhow!(PAIRING_AUTHENTICATION_FAILURE)
}

fn encode_frame(frame: &PairingFrameV1) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(frame).context("failed to encode pairing frame")?;
    if payload.is_empty() || payload.len() > MAX_LAN_PAIRING_FRAME_BYTES {
        bail!(
            "pairing frame length {} is outside 1..={MAX_LAN_PAIRING_FRAME_BYTES}",
            payload.len()
        );
    }
    reject_private_wire_material(&payload)?;
    let mut encoded = Vec::with_capacity(4 + payload.len());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(&payload);
    Ok(encoded)
}

fn write_frame(stream: &mut TcpStream, frame: &PairingFrameV1) -> Result<()> {
    let encoded = encode_frame(frame)?;
    stream
        .write_all(&encoded)
        .and_then(|_| stream.flush())
        .context("failed to write pairing frame")
}

fn read_frame(stream: &mut TcpStream) -> Result<PairingFrameV1> {
    let mut length = [0_u8; 4];
    stream
        .read_exact(&mut length)
        .context("pairing peer closed before a complete length prefix")?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_LAN_PAIRING_FRAME_BYTES {
        bail!("pairing peer announced invalid frame length {length}");
    }
    let mut payload = vec![0_u8; length];
    stream
        .read_exact(&mut payload)
        .context("pairing peer closed before a complete frame")?;
    reject_private_wire_material(&payload)?;
    serde_json::from_slice(&payload).context("failed to decode pairing frame")
}

fn validate_header(schema: &str, suite: &str, nonce: &str, expected_nonce: &str) -> Result<()> {
    if schema != LAN_PAIRING_SCHEMA_V1 || suite != LAN_PAIRING_SUITE_V1 {
        bail!("unsupported pairing schema or cryptographic suite");
    }
    validate_lower_hex("offer nonce", nonce, 32)?;
    if nonce != expected_nonce {
        return Err(authentication_failure());
    }
    Ok(())
}

fn decode_pake_message(encoded: &str) -> Result<Vec<u8>> {
    if encoded.len() > 256 {
        bail!("pairing PAKE message is oversized");
    }
    STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| authentication_failure())
}

fn reject_private_wire_material(bytes: &[u8]) -> Result<()> {
    if contains_bytes(bytes, PRIVATE_KEY_MARKER) {
        bail!("pairing wire material must never contain a private key");
    }
    Ok(())
}

fn validate_private_client_key(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_PUBLIC_FIELD_BYTES {
        bail!("pairing private client key has an invalid size");
    }
    if !contains_bytes(bytes, PRIVATE_KEY_MARKER) {
        bail!("pairing private client key is not a supported PEM private key");
    }
    Ok(())
}

fn require_public_pem(field: &str, value: &str, kind: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_PUBLIC_FIELD_BYTES || value.as_bytes().contains(&0) {
        bail!("pairing {field} has an invalid size or encoding");
    }
    reject_private_wire_material(value.as_bytes())?;
    if !value.contains(&format!("-----BEGIN {kind}-----"))
        || !value.contains(&format!("-----END {kind}-----"))
    {
        bail!("pairing {field} is not a PEM {kind}");
    }
    Ok(())
}

fn require_csr_pem(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_PUBLIC_FIELD_BYTES || value.as_bytes().contains(&0) {
        bail!("pairing client CSR has an invalid size or encoding");
    }
    reject_private_wire_material(value.as_bytes())?;
    let ordinary = value.contains("-----BEGIN CERTIFICATE REQUEST-----")
        && value.contains("-----END CERTIFICATE REQUEST-----");
    let legacy = value.contains("-----BEGIN NEW CERTIFICATE REQUEST-----")
        && value.contains("-----END NEW CERTIFICATE REQUEST-----");
    if !ordinary && !legacy {
        bail!("pairing client CSR is not a PEM certificate request");
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 {
        bail!("{field} must contain between 1 and 128 bytes");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{field} contains characters outside [A-Za-z0-9._:-]");
    }
    Ok(())
}

fn validate_server_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 253
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        bail!("server_name must be a non-empty ASCII DNS name or IP address");
    }
    Ok(())
}

fn validate_lower_hex(field: &str, value: &str, length: usize) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{field} must be lowercase hexadecimal with exactly {length} characters");
    }
    Ok(())
}

fn decode_fixed_hex<const N: usize>(value: &str) -> Result<[u8; N]> {
    validate_lower_hex("pairing confirmation", value, N * 2)?;
    let decoded = hex::decode(value)?;
    let mut output = [0_u8; N];
    output.copy_from_slice(&decoded);
    Ok(output)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn public_identity(node_id: &str, key_byte: char) -> PairingPublicIdentityV1 {
        PairingPublicIdentityV1 {
            node_id: node_id.to_owned(),
            server_name: format!("{node_id}.local"),
            service_port: 7337,
            supports_v2: true,
            server_ca_pem: format!(
                "-----BEGIN CERTIFICATE-----\nserver-ca-{node_id}\n-----END CERTIFICATE-----\n"
            ),
            client_issuer_ca_pem: format!(
                "-----BEGIN CERTIFICATE-----\nclient-ca-{node_id}\n-----END CERTIFICATE-----\n"
            ),
            client_csr_pem: format!(
                "-----BEGIN CERTIFICATE REQUEST-----\ncsr-{node_id}\n-----END CERTIFICATE REQUEST-----\n"
            ),
            node_receipt_public_key: key_byte.to_string().repeat(64),
        }
    }

    fn local_identity(node_id: &str, key_byte: char) -> PairingLocalIdentityV1 {
        PairingLocalIdentityV1::new(
            public_identity(node_id, key_byte),
            format!(
                "-----BEGIN PRIVATE KEY-----\nprivate-client-{node_id}\n-----END PRIVATE KEY-----\n"
            )
            .into_bytes(),
        )
        .unwrap()
    }

    fn issued_certificate(peer: &PairingPublicIdentityV1) -> Result<String> {
        Ok(format!(
            "-----BEGIN CERTIFICATE-----\nissued-for-{}\n-----END CERTIFICATE-----\n",
            peer.node_id
        ))
    }

    #[test]
    fn correct_code_mutually_confirms_and_returns_reciprocal_material() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let offer = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            accept_pairing_once(
                stream,
                "01234-56789",
                "offer-node",
                Duration::from_secs(5),
                |_| Ok(local_identity("offer-node", 'b')),
                issued_certificate,
            )
        });

        let joined = join_pairing_once(
            TcpStream::connect(address).unwrap(),
            "0123456789",
            "offer-node",
            Duration::from_secs(5),
            local_identity("join-node", 'a'),
            issued_certificate,
        )
        .unwrap();
        let accepted = offer.join().unwrap().unwrap();

        assert_eq!(joined.peer.node_id, "offer-node");
        assert_eq!(accepted.peer.node_id, "join-node");
        assert!(joined
            .local_issued_client_cert_pem
            .contains("issued-for-join-node"));
        assert!(accepted
            .local_issued_client_cert_pem
            .contains("issued-for-offer-node"));
        assert!(joined
            .local_private_client_key_pem()
            .windows(b"private-client-join-node".len())
            .any(|window| window == b"private-client-join-node"));
        assert!(accepted
            .local_private_client_key_pem()
            .windows(b"private-client-offer-node".len())
            .any(|window| window == b"private-client-offer-node"));
    }

    #[test]
    fn wrong_code_has_one_generic_authentication_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let offer = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            accept_pairing_once(
                stream,
                "11111-11111",
                "offer-node",
                Duration::from_secs(5),
                |_| Ok(local_identity("offer-node", 'b')),
                issued_certificate,
            )
        });
        let error = join_pairing_once(
            TcpStream::connect(address).unwrap(),
            "22222-22222",
            "offer-node",
            Duration::from_secs(5),
            local_identity("join-node", 'a'),
            issued_certificate,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), PAIRING_AUTHENTICATION_FAILURE);
        assert!(offer.join().unwrap().is_err());
    }

    #[test]
    fn confirmation_rejects_any_transcript_tampering() {
        let joiner = public_identity("join-node", 'a');
        let offer = public_identity("offer-node", 'b');
        let transcript = canonical_transcript(
            "00aa00aa00aa00aa00aa00aa00aa00aa",
            &joiner,
            &offer,
            "pake-a",
            "pake-b",
        )
        .unwrap();
        let keys = derive_confirmation_keys(
            b"test shared secret",
            "00aa00aa00aa00aa00aa00aa00aa00aa",
            &transcript,
        )
        .unwrap();
        let confirmation = confirmation_tag(
            keys.offer_to_join.as_slice(),
            "offer-auth",
            &transcript,
            None,
            None,
        )
        .unwrap();
        let mut tampered_offer = offer;
        tampered_offer.server_name = "attacker.local".to_owned();
        let tampered = canonical_transcript(
            "00aa00aa00aa00aa00aa00aa00aa00aa",
            &joiner,
            &tampered_offer,
            "pake-a",
            "pake-b",
        )
        .unwrap();
        let error = verify_confirmation(
            keys.offer_to_join.as_slice(),
            "offer-auth",
            &tampered,
            None,
            None,
            &confirmation,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), PAIRING_AUTHENTICATION_FAILURE);
    }

    #[test]
    fn passcode_and_public_identity_validation_are_strict() {
        assert_eq!(
            parse_pairing_passcode("01234-56789").unwrap().as_str(),
            "0123456789"
        );
        assert_eq!(
            parse_pairing_passcode("0123456789").unwrap().as_str(),
            "0123456789"
        );
        for invalid in [
            "123456789",
            "12345678901",
            "0123-456789",
            "01234 56789",
            "01234-5678x",
            " 0123456789",
        ] {
            assert!(parse_pairing_passcode(invalid).is_err(), "{invalid}");
        }

        let mut invalid = public_identity("join-node", 'a');
        invalid.client_issuer_ca_pem =
            "-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----\n".to_owned();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn private_client_keys_are_redacted_and_cannot_enter_wire_frames() {
        let local = local_identity("join-node", 'a');
        let secret = b"private-client-join-node";
        let debug = format!("{local:?}");
        assert!(!debug
            .as_bytes()
            .windows(secret.len())
            .any(|window| window == secret));

        let frame = PairingFrameV1::JoinHello {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: "00aa00aa00aa00aa00aa00aa00aa00aa".to_owned(),
            joiner: local.public().clone(),
            pake_a: "cHVibGljLXBha2U".to_owned(),
        };
        let encoded = encode_frame(&frame).unwrap();
        assert!(!encoded
            .windows(PRIVATE_KEY_MARKER.len())
            .any(|window| window == PRIVATE_KEY_MARKER));
        assert!(!encoded.windows(secret.len()).any(|window| window == secret));

        let rejected = PairingFrameV1::JoinAuth {
            schema: LAN_PAIRING_SCHEMA_V1.to_owned(),
            suite: LAN_PAIRING_SUITE_V1.to_owned(),
            offer_nonce: "00aa00aa00aa00aa00aa00aa00aa00aa".to_owned(),
            certificate_for_offer:
                "-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----\n".to_owned(),
            confirmation: "00".repeat(32),
        };
        assert!(encode_frame(&rejected).is_err());
    }

    #[test]
    fn generated_passcodes_have_the_exact_transcription_shape() {
        for _ in 0..32 {
            let passcode = generate_pairing_passcode().unwrap();
            assert_eq!(passcode.len(), 11);
            assert_eq!(passcode.as_bytes()[5], b'-');
            assert!(parse_pairing_passcode(&passcode).is_ok());
        }
    }
}
