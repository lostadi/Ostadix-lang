use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::placement::{SemanticDigestV1, StateCapacityObservationV2};
use crate::registry::registry_public_key_id;

use super::super::protocol::{canonical_hosted_bytes, sha256_hex};
use super::protocol::{
    validate_identifier_v2, validate_sha256_v2, HostedCommandBindingV2, HostedPlacementAuthorityV2,
    HostedPlacementEvidenceV2, JournalEntryV2, SignedJournalEntryV2, SignedPlacementLeaseV2,
    HOSTED_JOURNAL_ENTRY_SCHEMA_V2, HOSTED_PLACEMENT_LEASE_SCHEMA_V2,
    HOSTED_SIGNED_ENTRY_SCHEMA_V2,
};

const NODE_KEY_MAGIC_V2: &[u8] = b"OSTADIX-HOSTED-NODE-KEY-V2\0";
const PLACEMENT_KEY_MAGIC_V2: &[u8] = b"OSTADIX-HOSTED-PLACEMENT-KEY-V2\0";
const NODE_SIGNING_DOMAIN_V2: &[u8] = b"OSTADIX/HOSTED-JOURNAL/V2\0";
const PLACEMENT_SIGNING_DOMAIN_V2: &[u8] = b"OSTADIX/HOSTED-PLACEMENT-LEASE/V2\0";
const NODE_KEY_ID_DOMAIN_V2: &[u8] = b"OSTADIX/HOSTED-NODE-KEY-ID/V2\0";
const JOURNAL_ENTRY_DIGEST_DOMAIN_V2: &[u8] = b"OSTADIX/HOSTED-JOURNAL-ENTRY/V2\0";

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PlacementEnvelopeBodyV2<'a> {
    schema: &'a str,
    authority: &'a HostedPlacementAuthorityV2,
    command: &'a HostedCommandBindingV2,
    evidence: &'a HostedPlacementEvidenceV2,
    state_capacity_observation: &'a Option<StateCapacityObservationV2>,
}

#[derive(Clone)]
pub struct HostedNodeSignerV2 {
    signing_key: SigningKey,
}

impl std::fmt::Debug for HostedNodeSignerV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostedNodeSignerV2([redacted])")
    }
}

impl HostedNodeSignerV2 {
    pub fn generate() -> Result<Self> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).context("failed to obtain entropy for hosted node key")?;
        Ok(Self::from_secret_bytes(secret))
    }

    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key())
    }

    pub fn key_id(&self) -> String {
        key_id(NODE_KEY_ID_DOMAIN_V2, &self.public_key())
    }

    pub fn issue_journal_entry(&self, entry: JournalEntryV2) -> Result<SignedJournalEntryV2> {
        if entry.schema != HOSTED_JOURNAL_ENTRY_SCHEMA_V2 {
            bail!(
                "cannot sign unsupported hosted journal schema `{}`",
                entry.schema
            );
        }
        validate_identifier_v2("session_id", &entry.session_id)?;
        if entry.sequence == 0 {
            bail!("hosted journal sequence must start at one");
        }
        if let Some(previous) = &entry.previous_entry_sha256 {
            validate_sha256_v2("previous_entry_sha256", previous)?;
        }
        let body = canonical_hosted_bytes(&entry)?;
        let entry_sha256 = domain_sha256(JOURNAL_ENTRY_DIGEST_DOMAIN_V2, &body);
        let signature = self
            .signing_key
            .sign(&signing_preimage(NODE_SIGNING_DOMAIN_V2, &body)?);
        Ok(SignedJournalEntryV2 {
            schema: HOSTED_SIGNED_ENTRY_SCHEMA_V2.to_owned(),
            entry,
            signer_public_key: self.public_key_hex(),
            signer_key_id: self.key_id(),
            entry_sha256,
            signature: hex::encode(signature.to_bytes()),
        })
    }
}

impl SignedJournalEntryV2 {
    pub fn verify(&self) -> Result<()> {
        if self.schema != HOSTED_SIGNED_ENTRY_SCHEMA_V2 {
            bail!("unsupported signed hosted entry schema `{}`", self.schema);
        }
        if self.entry.schema != HOSTED_JOURNAL_ENTRY_SCHEMA_V2 {
            bail!(
                "unsupported hosted journal entry schema `{}`",
                self.entry.schema
            );
        }
        validate_identifier_v2("session_id", &self.entry.session_id)?;
        validate_sha256_v2("entry_sha256", &self.entry_sha256)?;
        let public = decode_fixed_hex::<32>("signer_public_key", &self.signer_public_key)?;
        let expected_key_id = key_id(NODE_KEY_ID_DOMAIN_V2, &public);
        if !constant_time_eq(expected_key_id.as_bytes(), self.signer_key_id.as_bytes()) {
            bail!("hosted journal signer key identifier mismatch");
        }
        let body = canonical_hosted_bytes(&self.entry)?;
        let expected_digest = domain_sha256(JOURNAL_ENTRY_DIGEST_DOMAIN_V2, &body);
        if !constant_time_eq(expected_digest.as_bytes(), self.entry_sha256.as_bytes()) {
            bail!("hosted journal entry digest mismatch");
        }
        let signature = decode_fixed_hex::<64>("signature", &self.signature)?;
        let verifying = VerifyingKey::from_bytes(&public)
            .context("hosted journal signer public key is invalid")?;
        verifying
            .verify_strict(
                &signing_preimage(NODE_SIGNING_DOMAIN_V2, &body)?,
                &Signature::from_bytes(&signature),
            )
            .context("hosted journal signature is invalid")
    }
}

/// Placement authority used by a scheduler or registry adapter.  This type is
/// intentionally not loaded by the hosted node as a secret: production nodes
/// receive only its pinned public key.
#[derive(Clone)]
pub struct PlacementLeaseSignerV2 {
    signing_key: SigningKey,
}

impl std::fmt::Debug for PlacementLeaseSignerV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PlacementLeaseSignerV2([redacted])")
    }
}

impl PlacementLeaseSignerV2 {
    pub fn generate() -> Result<Self> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).context("failed to obtain entropy for placement authority")?;
        Ok(Self::from_secret_bytes(secret))
    }

    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key())
    }

    /// Canonical placement issuer identity for this registry-compatible key.
    pub fn issuer_key(&self) -> SemanticDigestV1 {
        SemanticDigestV1::from_sha256(hex::encode(registry_public_key_id(&self.public_key())))
            .expect("registry Ed25519 key identifiers are SHA-256 values")
    }

    pub fn sign(
        &self,
        authority: HostedPlacementAuthorityV2,
        command: HostedCommandBindingV2,
        evidence: HostedPlacementEvidenceV2,
        state_capacity_observation: Option<StateCapacityObservationV2>,
    ) -> Result<SignedPlacementLeaseV2> {
        command.validate()?;
        evidence.validate_shape()?;
        if authority.issuer_key() != &self.issuer_key() {
            bail!("canonical hosted authority issuer does not match signing key");
        }
        let command_digest = command.semantic_digest()?;
        if authority.hosted_command_binding() != &command_digest {
            bail!("canonical hosted authority does not bind the hosted command digest");
        }
        let schema = HOSTED_PLACEMENT_LEASE_SCHEMA_V2.to_owned();
        let encoded = canonical_hosted_bytes(&PlacementEnvelopeBodyV2 {
            schema: &schema,
            authority: &authority,
            command: &command,
            evidence: &evidence,
            state_capacity_observation: &state_capacity_observation,
        })?;
        let signature = self
            .signing_key
            .sign(&signing_preimage(PLACEMENT_SIGNING_DOMAIN_V2, &encoded)?);
        Ok(SignedPlacementLeaseV2 {
            schema,
            authority,
            command,
            evidence,
            state_capacity_observation,
            signer_public_key: self.public_key_hex(),
            signer_key_id: self.issuer_key().to_string(),
            signature: hex::encode(signature.to_bytes()),
        })
    }
}

pub fn verify_placement_lease_signature_v2(lease: &SignedPlacementLeaseV2) -> Result<[u8; 32]> {
    if lease.schema != HOSTED_PLACEMENT_LEASE_SCHEMA_V2 {
        bail!(
            "unsupported hosted placement envelope schema `{}`",
            lease.schema
        );
    }
    lease.command.validate()?;
    lease.evidence.validate_shape()?;
    if lease.command.semantic_digest()? != *lease.authority.hosted_command_binding() {
        bail!("hosted command digest does not match canonical placement lease");
    }
    let public = decode_fixed_hex::<32>("signer_public_key", &lease.signer_public_key)?;
    let expected_key_id = hex::encode(registry_public_key_id(&public));
    if !constant_time_eq(expected_key_id.as_bytes(), lease.signer_key_id.as_bytes()) {
        bail!("hosted placement signer key identifier mismatch");
    }
    if lease.authority.issuer_key().as_sha256() != expected_key_id {
        bail!("canonical placement lease issuer does not match envelope signer");
    }
    let signature = decode_fixed_hex::<64>("signature", &lease.signature)?;
    let body = canonical_hosted_bytes(&PlacementEnvelopeBodyV2 {
        schema: &lease.schema,
        authority: &lease.authority,
        command: &lease.command,
        evidence: &lease.evidence,
        state_capacity_observation: &lease.state_capacity_observation,
    })?;
    let verifying = VerifyingKey::from_bytes(&public)
        .context("hosted placement signer public key is invalid")?;
    verifying
        .verify_strict(
            &signing_preimage(PLACEMENT_SIGNING_DOMAIN_V2, &body)?,
            &Signature::from_bytes(&signature),
        )
        .context("hosted placement lease signature is invalid")?;
    Ok(public)
}

pub fn write_new_node_signing_key_v2(
    path: impl AsRef<Path>,
    signer: &HostedNodeSignerV2,
) -> Result<()> {
    let path = path.as_ref();
    let parent = usable_parent(path);
    ensure_private_directory_v2(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("refusing to overwrite hosted node key `{}`", path.display()))?;
    file.write_all(NODE_KEY_MAGIC_V2)?;
    file.write_all(&signer.signing_key.to_bytes())?;
    file.sync_all()?;
    sync_directory(parent)?;
    Ok(())
}

pub fn read_node_signing_key_v2(path: impl AsRef<Path>) -> Result<HostedNodeSignerV2> {
    let path = path.as_ref();
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect hosted node key `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("hosted node key must be a regular, non-symlink file");
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "hosted node key `{}` must not be accessible by group or other users",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(1024).read_to_end(&mut bytes)?;
    if bytes.len() != NODE_KEY_MAGIC_V2.len() + 32 || !bytes.starts_with(NODE_KEY_MAGIC_V2) {
        bail!("hosted node signing key has an invalid V2 encoding");
    }
    let mut secret = [0_u8; 32];
    secret.copy_from_slice(&bytes[NODE_KEY_MAGIC_V2.len()..]);
    Ok(HostedNodeSignerV2::from_secret_bytes(secret))
}

pub fn read_placement_public_key_v2(path: impl AsRef<Path>) -> Result<[u8; 32]> {
    let path = path.as_ref();
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "failed to inspect placement authority key `{}`",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("placement authority public key must be a regular, non-symlink file");
    }
    let text = fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read placement authority key `{}`",
            path.display()
        )
    })?;
    decode_fixed_hex::<32>("placement authority public key", text.trim())
}

pub fn write_new_placement_signing_key_v2(
    path: impl AsRef<Path>,
    signer: &PlacementLeaseSignerV2,
) -> Result<()> {
    let path = path.as_ref();
    let parent = usable_parent(path);
    ensure_private_directory_v2(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).with_context(|| {
        format!(
            "refusing to overwrite placement signing key `{}`",
            path.display()
        )
    })?;
    file.write_all(PLACEMENT_KEY_MAGIC_V2)?;
    file.write_all(&signer.signing_key.to_bytes())?;
    file.sync_all()?;
    sync_directory(parent)
}

pub fn read_placement_signing_key_v2(path: impl AsRef<Path>) -> Result<PlacementLeaseSignerV2> {
    let path = path.as_ref();
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "failed to inspect placement signing key `{}`",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("placement signing key must be a regular, non-symlink file");
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "placement signing key `{}` must not be accessible by group or other users",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(1024).read_to_end(&mut bytes)?;
    if bytes.len() != PLACEMENT_KEY_MAGIC_V2.len() + 32
        || !bytes.starts_with(PLACEMENT_KEY_MAGIC_V2)
    {
        bail!("placement signing key has an invalid V2 encoding");
    }
    let mut secret = [0_u8; 32];
    secret.copy_from_slice(&bytes[PLACEMENT_KEY_MAGIC_V2.len()..]);
    Ok(PlacementLeaseSignerV2::from_secret_bytes(secret))
}

pub fn write_new_placement_public_key_v2(
    path: impl AsRef<Path>,
    public_key: &[u8; 32],
) -> Result<()> {
    let path = path.as_ref();
    let parent = usable_parent(path);
    ensure_private_directory_v2(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).with_context(|| {
        format!(
            "refusing to overwrite placement authority key `{}`",
            path.display()
        )
    })?;
    writeln!(file, "{}", hex::encode(public_key))?;
    file.sync_all()?;
    sync_directory(parent)
}

pub fn write_new_node_public_key_v2(path: impl AsRef<Path>, public_key: &[u8; 32]) -> Result<()> {
    write_new_hex_public_key(path.as_ref(), public_key, "hosted node public key")
}

pub fn read_node_public_key_v2(path: impl AsRef<Path>) -> Result<[u8; 32]> {
    let path = path.as_ref();
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "failed to inspect hosted node public key `{}`",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("hosted node public key must be a regular, non-symlink file");
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read hosted node public key `{}`", path.display()))?;
    decode_fixed_hex::<32>("hosted node public key", text.trim())
}

fn write_new_hex_public_key(path: &Path, public_key: &[u8; 32], label: &str) -> Result<()> {
    let parent = usable_parent(path);
    ensure_private_directory_v2(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("refusing to overwrite {label} `{}`", path.display()))?;
    writeln!(file, "{}", hex::encode(public_key))?;
    file.sync_all()?;
    sync_directory(parent)
}

fn usable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn ensure_private_directory_v2(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "hosted V2 state path `{}` must be a real directory",
                    path.display()
                );
            }
            #[cfg(unix)]
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!(
                    "hosted V2 state directory `{}` must have mode 0700",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(path).with_context(|| {
                format!("failed to create hosted V2 state `{}`", path.display())
            })?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory `{}` for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory `{}`", path.display()))
}

pub(crate) fn key_id(domain: &[u8], public_key: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(public_key);
    hex::encode(hash.finalize())
}

pub(crate) fn domain_sha256(domain: &[u8], body: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update((body.len() as u64).to_be_bytes());
    hash.update(body);
    hex::encode(hash.finalize())
}

pub(crate) fn signing_preimage(domain: &[u8], body: &[u8]) -> Result<Vec<u8>> {
    let len: u64 = body
        .len()
        .try_into()
        .context("signed hosted record is too large")?;
    let mut preimage = Vec::with_capacity(domain.len() + 8 + body.len());
    preimage.extend_from_slice(domain);
    preimage.extend_from_slice(&len.to_be_bytes());
    preimage.extend_from_slice(body);
    Ok(preimage)
}

pub(crate) fn decode_fixed_hex<const N: usize>(field: &str, text: &str) -> Result<[u8; N]> {
    let decoded = hex::decode(text).with_context(|| format!("{field} is not hexadecimal"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{field} must contain exactly {N} bytes"))
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub(crate) fn salted_bearer_hash(salt: &[u8; 32], bearer: &[u8; 32]) -> String {
    const DOMAIN: &[u8] = b"OSTADIX/HOSTED-SESSION-BEARER/V2\0";
    let mut bytes = Vec::with_capacity(DOMAIN.len() + 64);
    bytes.extend_from_slice(DOMAIN);
    bytes.extend_from_slice(salt);
    bytes.extend_from_slice(bearer);
    sha256_hex(&bytes)
}
