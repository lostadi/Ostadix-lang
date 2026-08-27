use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::canonical_cbor::{encode, registry_public_key_id, signing_preimage};
use crate::execution_fabric::decode_execution_candidate_v1;
use crate::placement_protocol::{GenerationV1, SemanticDigestV1, UnixMillisV1};

use super::codec::fabric_lease_sha256_v3;
use super::protocol::{
    invalid, validate_lower_hex, validate_node_id, ExecutionCellIncarnationV1,
    FabricAuthorityError, FabricSubmissionV1, FabricTerminalCandidateV1, PlacementLeaseV3,
    SignedExecutionLeaseV3, SignedTerminalCandidateReceiptV1, TerminalCandidateReceiptV1,
    FABRIC_SIGNED_LEASE_SCHEMA_V3, FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1,
};

pub const FABRIC_EXECUTION_LEASE_SIGNING_DOMAIN_V3: &[u8] = b"OSTADIX/EXECUTION-FABRIC/LEASE/V3\0";
pub const FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1: &[u8] =
    b"OSTADIX/EXECUTION-FABRIC/TERMINAL-RECEIPT/V1\0";

#[derive(Clone)]
pub struct FabricSigningKeyV1 {
    signing_key: SigningKey,
}

impl std::fmt::Debug for FabricSigningKeyV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FabricSigningKeyV1([redacted])")
    }
}

impl FabricSigningKeyV1 {
    pub fn generate() -> Result<Self, FabricAuthorityError> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| {
            FabricAuthorityError::Signature(format!("operating-system entropy failed: {error}"))
        })?;
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

    pub fn key_id(&self) -> [u8; 32] {
        registry_public_key_id(&self.public_key())
    }

    pub fn key_id_hex(&self) -> String {
        hex::encode(self.key_id())
    }

    pub fn key_id_digest(&self) -> SemanticDigestV1 {
        SemanticDigestV1::from_sha256(self.key_id_hex())
            .expect("registry key identity is a lowercase SHA-256")
    }

    pub fn sign_execution_lease(
        &self,
        lease: PlacementLeaseV3,
    ) -> Result<SignedExecutionLeaseV3, FabricAuthorityError> {
        lease.validate()?;
        if lease.issuer_key_id() != &self.key_id_digest() {
            return Err(invalid(
                "execution lease issuer key does not identify the signing key",
            ));
        }
        let body =
            encode(&lease).map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
        let preimage = signing_preimage(FABRIC_EXECUTION_LEASE_SIGNING_DOMAIN_V3, &body)
            .map_err(|error| FabricAuthorityError::Signature(format!("{error:#}")))?;
        let signed = SignedExecutionLeaseV3 {
            schema: FABRIC_SIGNED_LEASE_SCHEMA_V3.to_string(),
            lease,
            signer_public_key: self.public_key_hex(),
            signer_key_id: self.key_id_hex(),
            signature: hex::encode(self.signing_key.sign(&preimage).to_bytes()),
        };
        signed.validate_shape()?;
        Ok(signed)
    }

    pub fn sign_terminal_candidate(
        &self,
        submission: &FabricSubmissionV1,
        candidate_bytes: Vec<u8>,
        runtime_observation_ms: u64,
    ) -> Result<FabricTerminalCandidateV1, FabricAuthorityError> {
        submission.validate()?;
        let candidate = decode_execution_candidate_v1(&candidate_bytes)
            .map_err(|error| invalid(format!("terminal candidate: {error}")))?;
        let receipt = TerminalCandidateReceiptV1::new(
            submission,
            fabric_lease_sha256_v3(submission.header().lease().lease())?,
            &candidate,
            &candidate_bytes,
            runtime_observation_ms,
        )?;
        let body =
            encode(&receipt).map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
        let preimage = signing_preimage(FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1, &body)
            .map_err(|error| FabricAuthorityError::Signature(format!("{error:#}")))?;
        let signed = SignedTerminalCandidateReceiptV1 {
            schema: FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1.to_string(),
            receipt,
            signer_public_key: self.public_key_hex(),
            signer_key_id: self.key_id_hex(),
            signature: hex::encode(self.signing_key.sign(&preimage).to_bytes()),
        };
        signed.validate_shape()?;
        FabricTerminalCandidateV1::from_wire(signed, candidate_bytes)
    }
}

/// Explicit allowlist of Fabric execution authorities. A public key embedded
/// in a signed request is never trusted merely because its signature verifies.
#[derive(Clone, Debug, Default)]
pub struct TrustedFabricAuthoritiesV1 {
    keys: BTreeMap<String, [u8; 32]>,
}

impl TrustedFabricAuthoritiesV1 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enroll(&mut self, public_key: [u8; 32]) -> String {
        let key_id = hex::encode(registry_public_key_id(&public_key));
        self.keys.insert(key_id.clone(), public_key);
        key_id
    }

    pub fn contains_key_id(&self, key_id: &str) -> bool {
        self.keys.contains_key(key_id)
    }

    pub fn verify_execution_lease(
        &self,
        signed: &SignedExecutionLeaseV3,
        now: UnixMillisV1,
    ) -> Result<(), FabricAuthorityError> {
        signed.validate_envelope_shape()?;
        let public_key = decode_fixed_hex::<32>(
            "execution lease signer public key",
            signed.signer_public_key(),
        )?;
        let actual_key_id = hex::encode(registry_public_key_id(&public_key));
        if actual_key_id != signed.signer_key_id()
            || actual_key_id != signed.lease().issuer_key_id().as_sha256()
        {
            return Err(invalid("execution lease signer key identity mismatch"));
        }
        let trusted = self
            .keys
            .get(&actual_key_id)
            .ok_or_else(|| FabricAuthorityError::UntrustedSigner(actual_key_id.clone()))?;
        if trusted != &public_key {
            return Err(invalid(
                "trusted Fabric issuer key bytes do not match key id",
            ));
        }
        verify_signature(
            FABRIC_EXECUTION_LEASE_SIGNING_DOMAIN_V3,
            signed.lease(),
            &public_key,
            signed.signature(),
        )?;
        signed.lease().validate_at(now)
    }
}

/// Exact node receipt key and generation coordinates retained by a
/// coordinator for one explicitly selected target.
#[derive(Clone, Debug)]
pub struct PinnedFabricNodeKeyV1 {
    node_id: String,
    node_generation: GenerationV1,
    execution_cell_incarnation: ExecutionCellIncarnationV1,
    public_key: [u8; 32],
    key_id: String,
}

impl PinnedFabricNodeKeyV1 {
    pub fn new(
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        execution_cell_incarnation: ExecutionCellIncarnationV1,
        public_key: [u8; 32],
    ) -> Result<Self, FabricAuthorityError> {
        let node_id = node_id.into();
        let key_id = hex::encode(registry_public_key_id(&public_key));
        let value = Self {
            node_id,
            node_generation,
            execution_cell_incarnation,
            public_key,
            key_id,
        };
        validate_node_id(&value.node_id)?;
        Ok(value)
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn verify_terminal_candidate(
        &self,
        terminal: &FabricTerminalCandidateV1,
        submission: &FabricSubmissionV1,
    ) -> Result<(), FabricAuthorityError> {
        terminal.validate_transport_shape()?;
        let signed = terminal.signed_receipt();
        let receipt = signed.receipt();
        if signed.signer_key_id() != self.key_id
            || signed.signer_public_key() != hex::encode(self.public_key)
        {
            return Err(invalid(
                "terminal receipt was not signed by the pinned node key",
            ));
        }
        verify_signature(
            FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1,
            receipt,
            &self.public_key,
            signed.signature(),
        )?;

        receipt.validate()?;
        if receipt.node_id() != self.node_id
            || receipt.node_generation() != self.node_generation
            || receipt.execution_cell_incarnation() != self.execution_cell_incarnation
        {
            return Err(invalid("terminal receipt node/generation binding mismatch"));
        }
        submission.validate()?;
        let lease = submission.header().lease().lease();
        let capsule = submission.decoded_capsule()?;
        let candidate = terminal.decoded_candidate()?;
        candidate
            .validate_against(&capsule)
            .map_err(|error| invalid(format!("terminal candidate: {error}")))?;
        let exact = receipt.attempt() == lease.attempt()
            && receipt.node_id() == lease.target().node_id()
            && receipt.node_generation() == lease.target().node_generation()
            && receipt.execution_cell_incarnation() == lease.target().execution_cell_incarnation()
            && receipt.issuer_key_id() == lease.issuer_key_id()
            && receipt.lease_nonce() == lease.lease_nonce()
            && receipt.lease_sha256() == &fabric_lease_sha256_v3(lease)?
            && receipt.submission_binding_sha256()
                == submission.header().submission_binding_sha256()
            && receipt.capsule_sha256() == lease.capsule_sha256()
            && receipt.source_closure_sha256() == lease.source_closure_sha256()
            && receipt.input_manifest_sha256() == lease.input_manifest_sha256()
            && receipt.output_contract_sha256() == lease.output_contract_sha256()
            && receipt.backend_catalog_sha256() == lease.backend_catalog_sha256()
            && receipt.backend_implementation_sha256() == lease.backend_implementation_sha256()
            && receipt.runtime_observation_ms() <= lease.maximum_runtime_ms()
            && candidate.attempt() == lease.attempt();
        if !exact {
            return Err(invalid(
                "terminal receipt does not bind the retained Fabric submission",
            ));
        }
        Ok(())
    }
}

fn verify_signature<T: serde::Serialize>(
    domain: &[u8],
    body: &T,
    public_key: &[u8; 32],
    signature_hex: &str,
) -> Result<(), FabricAuthorityError> {
    validate_lower_hex("Fabric signature", signature_hex, 64)?;
    let signature = decode_fixed_hex::<64>("Fabric signature", signature_hex)?;
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .map_err(|_| FabricAuthorityError::Signature("invalid Ed25519 public key".to_string()))?;
    let body = encode(body).map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
    let preimage = signing_preimage(domain, &body)
        .map_err(|error| FabricAuthorityError::Signature(format!("{error:#}")))?;
    verifying_key
        .verify_strict(&preimage, &Signature::from_bytes(&signature))
        .map_err(|_| FabricAuthorityError::Signature("Ed25519 verification failed".to_string()))
}

fn decode_fixed_hex<const N: usize>(
    field: &str,
    value: &str,
) -> Result<[u8; N], FabricAuthorityError> {
    validate_lower_hex(field, value, N)?;
    let decoded = hex::decode(value)
        .map_err(|error| FabricAuthorityError::Signature(format!("{field}: {error}")))?;
    decoded
        .try_into()
        .map_err(|_| FabricAuthorityError::Signature(format!("{field} has wrong length")))
}
