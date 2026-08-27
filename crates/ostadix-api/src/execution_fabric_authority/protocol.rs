use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::execution_contract::Policy;
use crate::execution_fabric::{
    decode_execution_candidate_v1, decode_execution_capsule_v1, execution_capsule_sha256_v1,
    AttemptIdV1, CandidateOutcomeV1, ExecutionCandidateV1, ExecutionCapsuleV1, Sha256DigestV1,
    MAX_EXECUTION_CANDIDATE_BYTES, MAX_EXECUTION_CAPSULE_BYTES,
};
use crate::placement_protocol::{GenerationV1, SemanticDigestV1, UnixMillisV1};

pub const FABRIC_REQUEST_SCHEMA_V1: &str = "ostadix.execution-fabric-request/v1";
pub const FABRIC_RESPONSE_SCHEMA_V1: &str = "ostadix.execution-fabric-response/v1";
pub const FABRIC_SUBMISSION_SCHEMA_V1: &str = "ostadix.execution-fabric-submission/v1";
pub const FABRIC_SOURCE_CLOSURE_SCHEMA_V1: &str = "ostadix.execution-source-closure/v1";
pub const FABRIC_SOURCE_CLOSURE_DIALECT_V1: &str = "ostadix-source-closure/v1";
pub const FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1: u32 = 0;
pub const FABRIC_PLACEMENT_LEASE_SCHEMA_V3: &str = "ostadix.execution-placement-lease/v3";
pub const FABRIC_SIGNED_LEASE_SCHEMA_V3: &str = "ostadix.signed-execution-lease/v3";
pub const FABRIC_TERMINAL_RECEIPT_SCHEMA_V1: &str = "ostadix.execution-fabric-terminal-receipt/v1";
pub const FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1: &str =
    "ostadix.signed-execution-fabric-terminal-receipt/v1";
pub const MAX_FABRIC_HEADER_BYTES: usize = 48 * 1024;
pub const FABRIC_CLOCK_SKEW_TOLERANCE_MS: u64 = 2_000;
pub const MAX_FABRIC_LEASE_LIFETIME_MS: u64 = 30_000;

const MAX_NODE_ID_BYTES: usize = 128;
const MAX_REASON_CODE_BYTES: usize = 64;
const MAX_REASON_MESSAGE_BYTES: usize = 1024;
const MAX_SOURCE_FRAGMENT_BYTES: usize = 16 * 1024;
const SUBMISSION_BINDING_DOMAIN_V1: &[u8] = b"ostadix/execution-fabric/submission-binding/v1";
const SOURCE_CLOSURE_DIGEST_DOMAIN_V1: &[u8] = b"ostadix/execution-fabric/source-closure/v1";
const TERMINAL_RECEIPT_DIGEST_DOMAIN_V1: &[u8] = b"ostadix/execution-fabric/terminal-receipt/v1";

#[derive(Debug, Error)]
pub enum FabricAuthorityError {
    #[error("invalid Fabric authority record: {0}")]
    Invalid(String),
    #[error("Fabric {kind} record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("Fabric {kind} record is not canonical CBOR")]
    NonCanonical { kind: &'static str },
    #[error("Fabric codec error: {0}")]
    Codec(String),
    #[error("Fabric signature error: {0}")]
    Signature(String),
    #[error("untrusted Fabric signer `{0}`")]
    UntrustedSigner(String),
}

pub(crate) fn invalid(message: impl Into<String>) -> FabricAuthorityError {
    FabricAuthorityError::Invalid(message.into())
}

pub(crate) fn raw_sha256(bytes: &[u8]) -> Sha256DigestV1 {
    Sha256::digest(bytes).into()
}

pub(crate) fn domain_sha256(domain: &[u8], bytes: &[u8]) -> Sha256DigestV1 {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    digest.finalize().into()
}

pub(crate) fn validate_node_id(node_id: &str) -> Result<(), FabricAuthorityError> {
    if node_id.is_empty()
        || node_id.len() > MAX_NODE_ID_BYTES
        || !node_id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        })
    {
        return Err(invalid("target node identity is not a bounded ASCII token"));
    }
    Ok(())
}

fn require_nonzero_digest(
    field: &str,
    digest: &Sha256DigestV1,
) -> Result<(), FabricAuthorityError> {
    if digest.iter().all(|byte| *byte == 0) {
        return Err(invalid(format!("{field} must not be the all-zero digest")));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ExecutionCellIncarnationV1(u64);

impl ExecutionCellIncarnationV1 {
    pub fn new(value: u64) -> Result<Self, FabricAuthorityError> {
        if value == 0 {
            return Err(invalid("execution-cell incarnation must be nonzero"));
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ExecutionCellIncarnationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Exact provider and placement facts selected by the Fabric authority.
///
/// The digest-typed fields are identities of the existing canonical placement
/// records. This record does not duplicate their contents or reinterpret their
/// authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricTargetBindingV1 {
    tls_client_principal_sha256: SemanticDigestV1,
    node_id: String,
    node_generation: GenerationV1,
    execution_cell_incarnation: ExecutionCellIncarnationV1,
    target_descriptor_sha256: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    capacity_observation_sha256: SemanticDigestV1,
    candidate_eligibility_sha256: SemanticDigestV1,
    requirement_footprint_sha256: SemanticDigestV1,
    warrant_discharge_sha256: SemanticDigestV1,
    trust_policy_sha256: SemanticDigestV1,
    reservation_sha256: SemanticDigestV1,
    realization_pipeline_sha256: SemanticDigestV1,
}

impl FabricTargetBindingV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tls_client_principal_sha256: SemanticDigestV1,
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        execution_cell_incarnation: ExecutionCellIncarnationV1,
        target_descriptor_sha256: SemanticDigestV1,
        profile_generation: GenerationV1,
        capacity_generation: GenerationV1,
        capacity_observation_sha256: SemanticDigestV1,
        candidate_eligibility_sha256: SemanticDigestV1,
        requirement_footprint_sha256: SemanticDigestV1,
        warrant_discharge_sha256: SemanticDigestV1,
        trust_policy_sha256: SemanticDigestV1,
        reservation_sha256: SemanticDigestV1,
        realization_pipeline_sha256: SemanticDigestV1,
    ) -> Result<Self, FabricAuthorityError> {
        let value = Self {
            tls_client_principal_sha256,
            node_id: node_id.into(),
            node_generation,
            execution_cell_incarnation,
            target_descriptor_sha256,
            profile_generation,
            capacity_generation,
            capacity_observation_sha256,
            candidate_eligibility_sha256,
            requirement_footprint_sha256,
            warrant_discharge_sha256,
            trust_policy_sha256,
            reservation_sha256,
            realization_pipeline_sha256,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn tls_client_principal_sha256(&self) -> &SemanticDigestV1 {
        &self.tls_client_principal_sha256
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn execution_cell_incarnation(&self) -> ExecutionCellIncarnationV1 {
        self.execution_cell_incarnation
    }

    pub fn target_descriptor_sha256(&self) -> &SemanticDigestV1 {
        &self.target_descriptor_sha256
    }

    pub fn profile_generation(&self) -> GenerationV1 {
        self.profile_generation
    }

    pub fn capacity_generation(&self) -> GenerationV1 {
        self.capacity_generation
    }

    pub fn capacity_observation_sha256(&self) -> &SemanticDigestV1 {
        &self.capacity_observation_sha256
    }

    pub fn candidate_eligibility_sha256(&self) -> &SemanticDigestV1 {
        &self.candidate_eligibility_sha256
    }

    pub fn requirement_footprint_sha256(&self) -> &SemanticDigestV1 {
        &self.requirement_footprint_sha256
    }

    pub fn warrant_discharge_sha256(&self) -> &SemanticDigestV1 {
        &self.warrant_discharge_sha256
    }

    pub fn trust_policy_sha256(&self) -> &SemanticDigestV1 {
        &self.trust_policy_sha256
    }

    pub fn reservation_sha256(&self) -> &SemanticDigestV1 {
        &self.reservation_sha256
    }

    pub fn realization_pipeline_sha256(&self) -> &SemanticDigestV1 {
        &self.realization_pipeline_sha256
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        validate_node_id(&self.node_id)
    }
}

#[derive(Serialize)]
struct SourceClosureDigestMaterial<'a> {
    dialect: &'a str,
    source_utf8: &'a str,
    source_sha256: Sha256DigestV1,
    root_operation: u32,
    base_policy: &'a str,
    intent_sha256: Sha256DigestV1,
    operation_oir_sha256: Sha256DigestV1,
    execution_plan_sha256: Sha256DigestV1,
}

/// Exact bounded, parseable material retained before coordinator dispatch.
/// The provider must parse and lower this fragment independently; the digest
/// fields are expected results, not permission to skip reproduction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricSourceClosureV1 {
    schema: String,
    dialect: String,
    source_utf8: String,
    source_sha256: Sha256DigestV1,
    root_operation: u32,
    base_policy: String,
    intent_sha256: Sha256DigestV1,
    operation_oir_sha256: Sha256DigestV1,
    execution_plan_sha256: Sha256DigestV1,
    closure_sha256: Sha256DigestV1,
}

impl FabricSourceClosureV1 {
    pub fn new(
        dialect: impl Into<String>,
        source_utf8: impl Into<String>,
        root_operation: u32,
        base_policy: impl Into<String>,
        intent_sha256: Sha256DigestV1,
        operation_oir_sha256: Sha256DigestV1,
        execution_plan_sha256: Sha256DigestV1,
    ) -> Result<Self, FabricAuthorityError> {
        let dialect = dialect.into();
        let source_utf8 = source_utf8.into();
        let base_policy = base_policy.into();
        let source_sha256 = raw_sha256(source_utf8.as_bytes());
        let material = SourceClosureDigestMaterial {
            dialect: &dialect,
            source_utf8: &source_utf8,
            source_sha256,
            root_operation,
            base_policy: &base_policy,
            intent_sha256,
            operation_oir_sha256,
            execution_plan_sha256,
        };
        let bytes = crate::canonical_cbor::encode(&material)
            .map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
        let value = Self {
            schema: FABRIC_SOURCE_CLOSURE_SCHEMA_V1.to_string(),
            dialect,
            source_utf8,
            source_sha256,
            root_operation,
            base_policy,
            intent_sha256,
            operation_oir_sha256,
            execution_plan_sha256,
            closure_sha256: domain_sha256(SOURCE_CLOSURE_DIGEST_DOMAIN_V1, &bytes),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn dialect(&self) -> &str {
        &self.dialect
    }

    pub fn source_utf8(&self) -> &str {
        &self.source_utf8
    }

    pub fn source_sha256(&self) -> &Sha256DigestV1 {
        &self.source_sha256
    }

    pub fn root_operation(&self) -> u32 {
        self.root_operation
    }

    pub fn base_policy(&self) -> &str {
        &self.base_policy
    }

    pub fn intent_sha256(&self) -> &Sha256DigestV1 {
        &self.intent_sha256
    }

    pub fn operation_oir_sha256(&self) -> &Sha256DigestV1 {
        &self.operation_oir_sha256
    }

    pub fn execution_plan_sha256(&self) -> &Sha256DigestV1 {
        &self.execution_plan_sha256
    }

    pub fn closure_sha256(&self) -> &Sha256DigestV1 {
        &self.closure_sha256
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        if self.schema != FABRIC_SOURCE_CLOSURE_SCHEMA_V1 {
            return Err(invalid("unsupported Fabric source-closure schema"));
        }
        if self.dialect != FABRIC_SOURCE_CLOSURE_DIALECT_V1 {
            return Err(invalid("unsupported Fabric source-closure dialect"));
        }
        if self.source_utf8.is_empty() || self.source_utf8.len() > MAX_SOURCE_FRAGMENT_BYTES {
            return Err(invalid("Fabric source fragment is outside V1 byte bounds"));
        }
        if self.root_operation != FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1 {
            return Err(invalid(
                "Fabric V1 source closure requires root operation zero",
            ));
        }
        if Policy::from_name(&self.base_policy).is_none() {
            return Err(invalid("unsupported Fabric source-closure base policy"));
        }
        if self.source_sha256 != raw_sha256(self.source_utf8.as_bytes()) {
            return Err(invalid("Fabric source fragment digest mismatch"));
        }
        for (field, digest) in [
            ("source digest", &self.source_sha256),
            ("intent digest", &self.intent_sha256),
            ("source OIR digest", &self.operation_oir_sha256),
            ("source plan digest", &self.execution_plan_sha256),
        ] {
            require_nonzero_digest(field, digest)?;
        }
        let material = SourceClosureDigestMaterial {
            dialect: &self.dialect,
            source_utf8: &self.source_utf8,
            source_sha256: self.source_sha256,
            root_operation: self.root_operation,
            base_policy: &self.base_policy,
            intent_sha256: self.intent_sha256,
            operation_oir_sha256: self.operation_oir_sha256,
            execution_plan_sha256: self.execution_plan_sha256,
        };
        let bytes = crate::canonical_cbor::encode(&material)
            .map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
        if self.closure_sha256 != domain_sha256(SOURCE_CLOSURE_DIGEST_DOMAIN_V1, &bytes) {
            return Err(invalid("Fabric source-closure digest mismatch"));
        }
        Ok(())
    }
}

/// Additive Fabric execution authority. PlacementLeaseV2 remains frozen and
/// continues to authorize only its Hosted command/state semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementLeaseV3 {
    schema: String,
    issuer_key_id: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    attempt: AttemptIdV1,
    target: FabricTargetBindingV1,
    capsule_sha256: Sha256DigestV1,
    renderer_source_sha256: Sha256DigestV1,
    source_closure_sha256: Sha256DigestV1,
    intent_sha256: Sha256DigestV1,
    operation_oir_sha256: Sha256DigestV1,
    execution_plan_sha256: Sha256DigestV1,
    input_manifest_sha256: Sha256DigestV1,
    output_contract_sha256: Sha256DigestV1,
    backend_catalog_sha256: Sha256DigestV1,
    backend_implementation_sha256: Sha256DigestV1,
    admission_sha256: Sha256DigestV1,
    maximum_runtime_ms: u64,
    one_use: bool,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl PlacementLeaseV3 {
    pub fn new(
        issuer_key_id: SemanticDigestV1,
        lease_nonce: SemanticDigestV1,
        target: FabricTargetBindingV1,
        source_closure: &FabricSourceClosureV1,
        capsule: &ExecutionCapsuleV1,
        issued_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, FabricAuthorityError> {
        let value = Self {
            schema: FABRIC_PLACEMENT_LEASE_SCHEMA_V3.to_string(),
            issuer_key_id,
            lease_nonce,
            attempt: capsule.attempt().clone(),
            target,
            capsule_sha256: execution_capsule_sha256_v1(capsule)
                .map_err(|error| invalid(error.to_string()))?,
            renderer_source_sha256: *capsule.region().source_sha256(),
            source_closure_sha256: *source_closure.closure_sha256(),
            intent_sha256: *source_closure.intent_sha256(),
            operation_oir_sha256: *capsule.region().expected_oir_sha256(),
            execution_plan_sha256: *capsule.region().expected_plan_sha256(),
            input_manifest_sha256: *capsule.inputs().manifest_sha256(),
            output_contract_sha256: *capsule.output().contract_sha256(),
            backend_catalog_sha256: *capsule.region().backend_catalog_sha256(),
            backend_implementation_sha256: *capsule.region().backend_implementation_sha256(),
            admission_sha256: *capsule.admission_sha256(),
            maximum_runtime_ms: capsule.limits().max_runtime_ms(),
            one_use: true,
            issued_at,
            expires_at,
        };
        value.validate()?;
        value.validate_against_submission(source_closure, capsule)?;
        Ok(value)
    }

    pub fn issuer_key_id(&self) -> &SemanticDigestV1 {
        &self.issuer_key_id
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
    }

    pub fn attempt(&self) -> &AttemptIdV1 {
        &self.attempt
    }

    pub fn target(&self) -> &FabricTargetBindingV1 {
        &self.target
    }

    pub fn capsule_sha256(&self) -> &Sha256DigestV1 {
        &self.capsule_sha256
    }

    pub fn source_closure_sha256(&self) -> &Sha256DigestV1 {
        &self.source_closure_sha256
    }

    pub fn renderer_source_sha256(&self) -> &Sha256DigestV1 {
        &self.renderer_source_sha256
    }

    pub fn intent_sha256(&self) -> &Sha256DigestV1 {
        &self.intent_sha256
    }

    pub fn operation_oir_sha256(&self) -> &Sha256DigestV1 {
        &self.operation_oir_sha256
    }

    pub fn execution_plan_sha256(&self) -> &Sha256DigestV1 {
        &self.execution_plan_sha256
    }

    pub fn input_manifest_sha256(&self) -> &Sha256DigestV1 {
        &self.input_manifest_sha256
    }

    pub fn output_contract_sha256(&self) -> &Sha256DigestV1 {
        &self.output_contract_sha256
    }

    pub fn backend_catalog_sha256(&self) -> &Sha256DigestV1 {
        &self.backend_catalog_sha256
    }

    pub fn backend_implementation_sha256(&self) -> &Sha256DigestV1 {
        &self.backend_implementation_sha256
    }

    pub fn admission_sha256(&self) -> &Sha256DigestV1 {
        &self.admission_sha256
    }

    pub fn maximum_runtime_ms(&self) -> u64 {
        self.maximum_runtime_ms
    }

    pub fn one_use(&self) -> bool {
        self.one_use
    }

    pub fn issued_at(&self) -> UnixMillisV1 {
        self.issued_at
    }

    pub fn expires_at(&self) -> UnixMillisV1 {
        self.expires_at
    }

    pub fn validate_at(&self, now: UnixMillisV1) -> Result<(), FabricAuthorityError> {
        self.validate()?;
        if now.get().saturating_add(FABRIC_CLOCK_SKEW_TOLERANCE_MS) < self.issued_at.get() {
            return Err(invalid(
                "execution lease is not yet valid under clock-skew policy",
            ));
        }
        if now.get()
            > self
                .expires_at
                .get()
                .saturating_add(FABRIC_CLOCK_SKEW_TOLERANCE_MS)
        {
            return Err(invalid(
                "execution lease is expired under clock-skew policy",
            ));
        }
        Ok(())
    }

    pub fn validate_against_submission(
        &self,
        source_closure: &FabricSourceClosureV1,
        capsule: &ExecutionCapsuleV1,
    ) -> Result<(), FabricAuthorityError> {
        self.validate()?;
        source_closure.validate()?;
        let actual_capsule =
            execution_capsule_sha256_v1(capsule).map_err(|error| invalid(error.to_string()))?;
        let exact = self.attempt == *capsule.attempt()
            && self.capsule_sha256 == actual_capsule
            && self.renderer_source_sha256 == *capsule.region().source_sha256()
            && self.source_closure_sha256 == *source_closure.closure_sha256()
            && self.intent_sha256 == *source_closure.intent_sha256()
            && self.operation_oir_sha256 == *source_closure.operation_oir_sha256()
            && self.execution_plan_sha256 == *source_closure.execution_plan_sha256()
            && self.operation_oir_sha256 == *capsule.region().expected_oir_sha256()
            && self.execution_plan_sha256 == *capsule.region().expected_plan_sha256()
            && self.input_manifest_sha256 == *capsule.inputs().manifest_sha256()
            && self.output_contract_sha256 == *capsule.output().contract_sha256()
            && self.backend_catalog_sha256 == *capsule.region().backend_catalog_sha256()
            && self.backend_implementation_sha256
                == *capsule.region().backend_implementation_sha256()
            && self.admission_sha256 == *capsule.admission_sha256()
            && self.maximum_runtime_ms == capsule.limits().max_runtime_ms();
        if !exact {
            return Err(invalid(
                "execution lease does not bind the exact M2 capsule",
            ));
        }
        if self.expires_at.get() > capsule.deadline_unix_ms() {
            return Err(invalid(
                "execution lease expiry exceeds the capsule deadline",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        if self.schema != FABRIC_PLACEMENT_LEASE_SCHEMA_V3 {
            return Err(invalid("unsupported execution placement lease schema"));
        }
        AttemptIdV1::new(self.attempt.task().clone(), self.attempt.generation())
            .map_err(|error| invalid(format!("Fabric lease attempt: {error}")))?;
        self.target.validate()?;
        if !self.one_use {
            return Err(invalid("Fabric execution lease must be one-use"));
        }
        if self.issued_at.get() == 0 || self.expires_at <= self.issued_at {
            return Err(invalid("Fabric execution lease validity window is invalid"));
        }
        if self.expires_at.get() - self.issued_at.get() > MAX_FABRIC_LEASE_LIFETIME_MS {
            return Err(invalid("Fabric execution lease lifetime exceeds V3 bounds"));
        }
        if self.maximum_runtime_ms == 0 {
            return Err(invalid("Fabric execution maximum runtime must be nonzero"));
        }
        for (field, digest) in [
            ("capsule digest", &self.capsule_sha256),
            ("renderer-source digest", &self.renderer_source_sha256),
            ("source-closure digest", &self.source_closure_sha256),
            ("intent digest", &self.intent_sha256),
            ("OIR digest", &self.operation_oir_sha256),
            ("execution-plan digest", &self.execution_plan_sha256),
            ("input-manifest digest", &self.input_manifest_sha256),
            ("output-contract digest", &self.output_contract_sha256),
            ("backend-catalog digest", &self.backend_catalog_sha256),
            (
                "backend-implementation digest",
                &self.backend_implementation_sha256,
            ),
            ("admission digest", &self.admission_sha256),
        ] {
            require_nonzero_digest(field, digest)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedExecutionLeaseV3 {
    pub(crate) schema: String,
    pub(crate) lease: PlacementLeaseV3,
    pub(crate) signer_public_key: String,
    pub(crate) signer_key_id: String,
    pub(crate) signature: String,
}

impl SignedExecutionLeaseV3 {
    pub fn lease(&self) -> &PlacementLeaseV3 {
        &self.lease
    }

    pub fn signer_public_key(&self) -> &str {
        &self.signer_public_key
    }

    pub fn signer_key_id(&self) -> &str {
        &self.signer_key_id
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub(crate) fn validate_shape(&self) -> Result<(), FabricAuthorityError> {
        self.validate_envelope_shape()?;
        self.lease.validate()
    }

    pub(crate) fn validate_envelope_shape(&self) -> Result<(), FabricAuthorityError> {
        if self.schema != FABRIC_SIGNED_LEASE_SCHEMA_V3 {
            return Err(invalid("unsupported signed execution lease schema"));
        }
        validate_lower_hex("lease signer public key", &self.signer_public_key, 32)?;
        validate_lower_hex("lease signer key id", &self.signer_key_id, 32)?;
        validate_lower_hex("lease signature", &self.signature, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricExactPayloadV1 {
    byte_length: u32,
    sha256: Sha256DigestV1,
}

impl FabricExactPayloadV1 {
    pub fn new(
        bytes: &[u8],
        maximum: usize,
        kind: &'static str,
    ) -> Result<Self, FabricAuthorityError> {
        if bytes.len() > maximum {
            return Err(FabricAuthorityError::RecordTooLarge {
                kind,
                actual: bytes.len(),
                maximum,
            });
        }
        let byte_length = u32::try_from(bytes.len())
            .map_err(|_| invalid(format!("{kind} length does not fit u32")))?;
        Ok(Self {
            byte_length,
            sha256: raw_sha256(bytes),
        })
    }

    pub fn byte_length(&self) -> u32 {
        self.byte_length
    }

    pub fn sha256(&self) -> &Sha256DigestV1 {
        &self.sha256
    }

    pub fn validate_bytes(
        &self,
        bytes: &[u8],
        maximum: usize,
        kind: &'static str,
    ) -> Result<(), FabricAuthorityError> {
        let actual = Self::new(bytes, maximum, kind)?;
        if actual != *self {
            return Err(invalid(format!("{kind} payload length/digest mismatch")));
        }
        Ok(())
    }

    pub(crate) fn validate(&self, maximum: usize) -> Result<(), FabricAuthorityError> {
        if self.byte_length as usize > maximum {
            return Err(FabricAuthorityError::RecordTooLarge {
                kind: "declared payload",
                actual: self.byte_length as usize,
                maximum,
            });
        }
        require_nonzero_digest("exact payload", &self.sha256)
    }
}

#[derive(Serialize)]
struct SubmissionBindingMaterial<'a> {
    lease: &'a SignedExecutionLeaseV3,
    source_closure: &'a FabricSourceClosureV1,
    capsule: &'a FabricExactPayloadV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricSubmissionHeaderV1 {
    schema: String,
    lease: SignedExecutionLeaseV3,
    source_closure: FabricSourceClosureV1,
    capsule: FabricExactPayloadV1,
    submission_binding_sha256: Sha256DigestV1,
}

impl FabricSubmissionHeaderV1 {
    pub fn lease(&self) -> &SignedExecutionLeaseV3 {
        &self.lease
    }

    pub fn capsule(&self) -> &FabricExactPayloadV1 {
        &self.capsule
    }

    pub fn source_closure(&self) -> &FabricSourceClosureV1 {
        &self.source_closure
    }

    pub fn submission_binding_sha256(&self) -> &Sha256DigestV1 {
        &self.submission_binding_sha256
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        self.validate_transport_shape()?;
        self.lease.validate_shape()?;
        self.source_closure.validate()
    }

    pub(crate) fn validate_transport_shape(&self) -> Result<(), FabricAuthorityError> {
        if self.schema != FABRIC_SUBMISSION_SCHEMA_V1 {
            return Err(invalid("unsupported Fabric submission schema"));
        }
        self.lease.validate_envelope_shape()?;
        self.capsule.validate(MAX_EXECUTION_CAPSULE_BYTES)?;
        let expected = crate::canonical_cbor::encode(&SubmissionBindingMaterial {
            lease: &self.lease,
            source_closure: &self.source_closure,
            capsule: &self.capsule,
        })
        .map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
        if self.submission_binding_sha256 != domain_sha256(SUBMISSION_BINDING_DOMAIN_V1, &expected)
        {
            return Err(invalid("Fabric submission binding digest mismatch"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricSubmissionV1 {
    header: FabricSubmissionHeaderV1,
    capsule_bytes: Vec<u8>,
}

impl FabricSubmissionV1 {
    pub fn new(
        lease: SignedExecutionLeaseV3,
        source_closure: FabricSourceClosureV1,
        capsule_bytes: Vec<u8>,
    ) -> Result<Self, FabricAuthorityError> {
        let capsule = decode_execution_capsule_v1(&capsule_bytes)
            .map_err(|error| invalid(format!("submission capsule: {error}")))?;
        lease
            .lease()
            .validate_against_submission(&source_closure, &capsule)
            .map_err(|error| invalid(format!("submission lease/capsule: {error}")))?;
        let payload =
            FabricExactPayloadV1::new(&capsule_bytes, MAX_EXECUTION_CAPSULE_BYTES, "capsule")?;
        let binding_bytes = crate::canonical_cbor::encode(&SubmissionBindingMaterial {
            lease: &lease,
            source_closure: &source_closure,
            capsule: &payload,
        })
        .map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
        let header = FabricSubmissionHeaderV1 {
            schema: FABRIC_SUBMISSION_SCHEMA_V1.to_string(),
            lease,
            source_closure,
            capsule: payload,
            submission_binding_sha256: domain_sha256(SUBMISSION_BINDING_DOMAIN_V1, &binding_bytes),
        };
        let value = Self {
            header,
            capsule_bytes,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn from_wire(
        header: FabricSubmissionHeaderV1,
        capsule_bytes: Vec<u8>,
    ) -> Result<Self, FabricAuthorityError> {
        let value = Self {
            header,
            capsule_bytes,
        };
        value.validate_transport_shape()?;
        Ok(value)
    }

    pub fn header(&self) -> &FabricSubmissionHeaderV1 {
        &self.header
    }

    pub fn capsule_bytes(&self) -> &[u8] {
        &self.capsule_bytes
    }

    pub fn decoded_capsule(&self) -> Result<ExecutionCapsuleV1, FabricAuthorityError> {
        decode_execution_capsule_v1(&self.capsule_bytes)
            .map_err(|error| invalid(format!("submission capsule: {error}")))
    }

    pub fn validate(&self) -> Result<(), FabricAuthorityError> {
        self.validate_transport_shape()?;
        self.header.validate()?;
        let capsule = self.decoded_capsule()?;
        self.header
            .lease
            .lease
            .validate_against_submission(&self.header.source_closure, &capsule)
    }

    pub(crate) fn validate_transport_shape(&self) -> Result<(), FabricAuthorityError> {
        self.header.validate_transport_shape()?;
        self.header.capsule.validate_bytes(
            &self.capsule_bytes,
            MAX_EXECUTION_CAPSULE_BYTES,
            "capsule",
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricAttemptQueryV1 {
    issuer_key_id: SemanticDigestV1,
    attempt: AttemptIdV1,
    lease_nonce: SemanticDigestV1,
    submission_binding_sha256: Sha256DigestV1,
}

impl FabricAttemptQueryV1 {
    pub fn from_submission(submission: &FabricSubmissionV1) -> Self {
        Self {
            issuer_key_id: submission.header.lease.lease.issuer_key_id.clone(),
            attempt: submission.header.lease.lease.attempt.clone(),
            lease_nonce: submission.header.lease.lease.lease_nonce.clone(),
            submission_binding_sha256: submission.header.submission_binding_sha256,
        }
    }

    pub fn issuer_key_id(&self) -> &SemanticDigestV1 {
        &self.issuer_key_id
    }

    pub fn attempt(&self) -> &AttemptIdV1 {
        &self.attempt
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
    }

    pub fn submission_binding_sha256(&self) -> &Sha256DigestV1 {
        &self.submission_binding_sha256
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        AttemptIdV1::new(self.attempt.task().clone(), self.attempt.generation())
            .map_err(|error| invalid(format!("Fabric query attempt: {error}")))?;
        require_nonzero_digest("query submission binding", &self.submission_binding_sha256)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FabricRequestV1 {
    SubmitPureAttempt(FabricSubmissionV1),
    QueryAttempt(FabricAttemptQueryV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricAttemptStatusV1 {
    issuer_key_id: SemanticDigestV1,
    attempt: AttemptIdV1,
    lease_nonce: SemanticDigestV1,
    submission_binding_sha256: Sha256DigestV1,
    node_id: String,
    node_generation: GenerationV1,
    execution_cell_incarnation: ExecutionCellIncarnationV1,
}

impl FabricAttemptStatusV1 {
    pub fn from_submission(submission: &FabricSubmissionV1) -> Self {
        let lease = submission.header.lease.lease();
        Self {
            issuer_key_id: lease.issuer_key_id.clone(),
            attempt: lease.attempt.clone(),
            lease_nonce: lease.lease_nonce.clone(),
            submission_binding_sha256: submission.header.submission_binding_sha256,
            node_id: lease.target.node_id.clone(),
            node_generation: lease.target.node_generation,
            execution_cell_incarnation: lease.target.execution_cell_incarnation,
        }
    }

    /// Build provider-scoped status coordinates for a query that has no
    /// retained ledger record. This does not authorize or advance an attempt;
    /// it only permits a bounded rejection to echo the queried binding on the
    /// authenticated Fabric route.
    pub fn from_query(
        query: &FabricAttemptQueryV1,
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        execution_cell_incarnation: ExecutionCellIncarnationV1,
    ) -> Result<Self, FabricAuthorityError> {
        query.validate()?;
        let value = Self {
            issuer_key_id: query.issuer_key_id.clone(),
            attempt: query.attempt.clone(),
            lease_nonce: query.lease_nonce.clone(),
            submission_binding_sha256: query.submission_binding_sha256,
            node_id: node_id.into(),
            node_generation,
            execution_cell_incarnation,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn issuer_key_id(&self) -> &SemanticDigestV1 {
        &self.issuer_key_id
    }

    pub fn attempt(&self) -> &AttemptIdV1 {
        &self.attempt
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
    }

    pub fn submission_binding_sha256(&self) -> &Sha256DigestV1 {
        &self.submission_binding_sha256
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn execution_cell_incarnation(&self) -> ExecutionCellIncarnationV1 {
        self.execution_cell_incarnation
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        AttemptIdV1::new(self.attempt.task().clone(), self.attempt.generation())
            .map_err(|error| invalid(format!("Fabric status attempt: {error}")))?;
        validate_node_id(&self.node_id)?;
        require_nonzero_digest("attempt status binding", &self.submission_binding_sha256)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricRejectionV1 {
    status: FabricAttemptStatusV1,
    reason_code: String,
    message: String,
}

impl FabricRejectionV1 {
    pub fn new(
        status: FabricAttemptStatusV1,
        reason_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, FabricAuthorityError> {
        let value = Self {
            status,
            reason_code: reason_code.into(),
            message: message.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn status(&self) -> &FabricAttemptStatusV1 {
        &self.status
    }

    pub fn reason_code(&self) -> &str {
        &self.reason_code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        self.status.validate()?;
        validate_reason("rejection", &self.reason_code, &self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricAbandonmentV1 {
    status: FabricAttemptStatusV1,
    reason_code: String,
    message: String,
}

impl FabricAbandonmentV1 {
    pub fn new(
        status: FabricAttemptStatusV1,
        reason_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, FabricAuthorityError> {
        let value = Self {
            status,
            reason_code: reason_code.into(),
            message: message.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn status(&self) -> &FabricAttemptStatusV1 {
        &self.status
    }

    pub fn reason_code(&self) -> &str {
        &self.reason_code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        self.status.validate()?;
        validate_reason("abandonment", &self.reason_code, &self.message)
    }
}

fn validate_reason(kind: &str, code: &str, message: &str) -> Result<(), FabricAuthorityError> {
    if code.is_empty()
        || code.len() > MAX_REASON_CODE_BYTES
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid(format!("Fabric {kind} code is invalid")));
    }
    if message.is_empty()
        || message.len() > MAX_REASON_MESSAGE_BYTES
        || message.chars().any(|value| value.is_control())
    {
        return Err(invalid(format!("Fabric {kind} message is invalid")));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FabricTerminalStatusV1 {
    TerminalCandidate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalCandidateReceiptV1 {
    schema: String,
    node_id: String,
    node_generation: GenerationV1,
    execution_cell_incarnation: ExecutionCellIncarnationV1,
    attempt: AttemptIdV1,
    issuer_key_id: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    lease_sha256: Sha256DigestV1,
    submission_binding_sha256: Sha256DigestV1,
    capsule_sha256: Sha256DigestV1,
    source_closure_sha256: Sha256DigestV1,
    input_manifest_sha256: Sha256DigestV1,
    output_contract_sha256: Sha256DigestV1,
    backend_catalog_sha256: Sha256DigestV1,
    backend_implementation_sha256: Sha256DigestV1,
    output_content_sha256: Sha256DigestV1,
    candidate_payload: FabricExactPayloadV1,
    runtime_observation_ms: u64,
    provider_completed_unix_ms: u64,
    terminal_status: FabricTerminalStatusV1,
}

impl TerminalCandidateReceiptV1 {
    pub(crate) fn new(
        submission: &FabricSubmissionV1,
        lease_sha256: Sha256DigestV1,
        candidate: &ExecutionCandidateV1,
        candidate_bytes: &[u8],
        runtime_observation_ms: u64,
    ) -> Result<Self, FabricAuthorityError> {
        let capsule = submission.decoded_capsule()?;
        candidate
            .validate_against(&capsule)
            .map_err(|error| invalid(format!("terminal candidate: {error}")))?;
        let CandidateOutcomeV1::Succeeded { output } = candidate.outcome() else {
            return Err(invalid(
                "the admitted trusted renderer cannot return a failed candidate",
            ));
        };
        let lease = submission.header.lease.lease();
        let value = Self {
            schema: FABRIC_TERMINAL_RECEIPT_SCHEMA_V1.to_string(),
            node_id: lease.target.node_id.clone(),
            node_generation: lease.target.node_generation,
            execution_cell_incarnation: lease.target.execution_cell_incarnation,
            attempt: lease.attempt.clone(),
            issuer_key_id: lease.issuer_key_id.clone(),
            lease_nonce: lease.lease_nonce.clone(),
            lease_sha256,
            submission_binding_sha256: submission.header.submission_binding_sha256,
            capsule_sha256: lease.capsule_sha256,
            source_closure_sha256: lease.source_closure_sha256,
            input_manifest_sha256: lease.input_manifest_sha256,
            output_contract_sha256: lease.output_contract_sha256,
            backend_catalog_sha256: lease.backend_catalog_sha256,
            backend_implementation_sha256: lease.backend_implementation_sha256,
            output_content_sha256: *output.value().content_sha256(),
            candidate_payload: FabricExactPayloadV1::new(
                candidate_bytes,
                MAX_EXECUTION_CANDIDATE_BYTES,
                "candidate",
            )?,
            runtime_observation_ms,
            provider_completed_unix_ms: candidate.completed_unix_ms(),
            terminal_status: FabricTerminalStatusV1::TerminalCandidate,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn execution_cell_incarnation(&self) -> ExecutionCellIncarnationV1 {
        self.execution_cell_incarnation
    }

    pub fn attempt(&self) -> &AttemptIdV1 {
        &self.attempt
    }

    pub fn issuer_key_id(&self) -> &SemanticDigestV1 {
        &self.issuer_key_id
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
    }

    pub fn lease_sha256(&self) -> &Sha256DigestV1 {
        &self.lease_sha256
    }

    pub fn submission_binding_sha256(&self) -> &Sha256DigestV1 {
        &self.submission_binding_sha256
    }

    pub fn capsule_sha256(&self) -> &Sha256DigestV1 {
        &self.capsule_sha256
    }

    pub fn source_closure_sha256(&self) -> &Sha256DigestV1 {
        &self.source_closure_sha256
    }

    pub fn input_manifest_sha256(&self) -> &Sha256DigestV1 {
        &self.input_manifest_sha256
    }

    pub fn output_contract_sha256(&self) -> &Sha256DigestV1 {
        &self.output_contract_sha256
    }

    pub fn backend_catalog_sha256(&self) -> &Sha256DigestV1 {
        &self.backend_catalog_sha256
    }

    pub fn backend_implementation_sha256(&self) -> &Sha256DigestV1 {
        &self.backend_implementation_sha256
    }

    pub fn output_content_sha256(&self) -> &Sha256DigestV1 {
        &self.output_content_sha256
    }

    pub fn candidate_payload(&self) -> &FabricExactPayloadV1 {
        &self.candidate_payload
    }

    pub fn runtime_observation_ms(&self) -> u64 {
        self.runtime_observation_ms
    }

    pub fn provider_completed_unix_ms(&self) -> u64 {
        self.provider_completed_unix_ms
    }

    pub fn terminal_status(&self) -> FabricTerminalStatusV1 {
        self.terminal_status
    }

    pub fn semantic_sha256(&self) -> Result<Sha256DigestV1, FabricAuthorityError> {
        self.validate()?;
        let bytes = crate::canonical_cbor::encode(self)
            .map_err(|error| FabricAuthorityError::Codec(format!("{error:#}")))?;
        Ok(domain_sha256(TERMINAL_RECEIPT_DIGEST_DOMAIN_V1, &bytes))
    }

    pub(crate) fn validate(&self) -> Result<(), FabricAuthorityError> {
        self.validate_representation_shape()?;
        AttemptIdV1::new(self.attempt.task().clone(), self.attempt.generation())
            .map_err(|error| invalid(format!("terminal receipt attempt: {error}")))?;
        validate_node_id(&self.node_id)?;
        if self.provider_completed_unix_ms == 0 {
            return Err(invalid(
                "terminal receipt provider completion time must be nonzero",
            ));
        }
        self.candidate_payload
            .validate(MAX_EXECUTION_CANDIDATE_BYTES)?;
        for (field, digest) in [
            ("receipt lease", &self.lease_sha256),
            ("receipt submission", &self.submission_binding_sha256),
            ("receipt capsule", &self.capsule_sha256),
            ("receipt source closure", &self.source_closure_sha256),
            ("receipt input manifest", &self.input_manifest_sha256),
            ("receipt output contract", &self.output_contract_sha256),
            ("receipt backend catalog", &self.backend_catalog_sha256),
            (
                "receipt backend implementation",
                &self.backend_implementation_sha256,
            ),
            ("receipt output content", &self.output_content_sha256),
        ] {
            require_nonzero_digest(field, digest)?;
        }
        Ok(())
    }

    /// Validate only fields that define the canonical receipt representation.
    /// Semantic bindings are deliberately left to the coordinator's ordered
    /// gates after the node signature has been authenticated.
    pub(crate) fn validate_representation_shape(&self) -> Result<(), FabricAuthorityError> {
        if self.schema != FABRIC_TERMINAL_RECEIPT_SCHEMA_V1 {
            return Err(invalid("unsupported terminal-candidate receipt schema"));
        }
        self.candidate_payload
            .validate(MAX_EXECUTION_CANDIDATE_BYTES)
    }

    pub fn validate_candidate_bytes(
        &self,
        candidate_bytes: &[u8],
    ) -> Result<ExecutionCandidateV1, FabricAuthorityError> {
        self.candidate_payload.validate_bytes(
            candidate_bytes,
            MAX_EXECUTION_CANDIDATE_BYTES,
            "candidate",
        )?;
        let candidate = decode_execution_candidate_v1(candidate_bytes)
            .map_err(|error| invalid(format!("terminal candidate: {error}")))?;
        if candidate.attempt() != &self.attempt
            || candidate.capsule_sha256() != &self.capsule_sha256
            || candidate.input_manifest_sha256() != &self.input_manifest_sha256
            || candidate.output_contract_sha256() != &self.output_contract_sha256
            || candidate.completed_unix_ms() != self.provider_completed_unix_ms
        {
            return Err(invalid(
                "terminal receipt does not bind the exact M2 candidate",
            ));
        }
        let CandidateOutcomeV1::Succeeded { output } = candidate.outcome() else {
            return Err(invalid("terminal receipt carried a failed M2 candidate"));
        };
        if output.value().content_sha256() != &self.output_content_sha256 {
            return Err(invalid("terminal receipt output-content digest mismatch"));
        }
        Ok(candidate)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedTerminalCandidateReceiptV1 {
    pub(crate) schema: String,
    pub(crate) receipt: TerminalCandidateReceiptV1,
    pub(crate) signer_public_key: String,
    pub(crate) signer_key_id: String,
    pub(crate) signature: String,
}

impl SignedTerminalCandidateReceiptV1 {
    pub fn receipt(&self) -> &TerminalCandidateReceiptV1 {
        &self.receipt
    }

    pub fn signer_public_key(&self) -> &str {
        &self.signer_public_key
    }

    pub fn signer_key_id(&self) -> &str {
        &self.signer_key_id
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub(crate) fn validate_shape(&self) -> Result<(), FabricAuthorityError> {
        self.validate_envelope_shape()?;
        self.receipt.validate()
    }

    pub(crate) fn validate_representation_shape(&self) -> Result<(), FabricAuthorityError> {
        self.validate_envelope_shape()?;
        self.receipt.validate_representation_shape()
    }

    pub(crate) fn validate_envelope_shape(&self) -> Result<(), FabricAuthorityError> {
        if self.schema != FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1 {
            return Err(invalid("unsupported signed terminal receipt schema"));
        }
        validate_lower_hex("receipt signer public key", &self.signer_public_key, 32)?;
        validate_lower_hex("receipt signer key id", &self.signer_key_id, 32)?;
        validate_lower_hex("receipt signature", &self.signature, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricTerminalCandidateV1 {
    signed_receipt: SignedTerminalCandidateReceiptV1,
    candidate_bytes: Vec<u8>,
}

impl FabricTerminalCandidateV1 {
    pub(crate) fn from_wire(
        signed_receipt: SignedTerminalCandidateReceiptV1,
        candidate_bytes: Vec<u8>,
    ) -> Result<Self, FabricAuthorityError> {
        let value = Self {
            signed_receipt,
            candidate_bytes,
        };
        value.validate_transport_shape()?;
        Ok(value)
    }

    pub fn signed_receipt(&self) -> &SignedTerminalCandidateReceiptV1 {
        &self.signed_receipt
    }

    pub fn candidate_bytes(&self) -> &[u8] {
        &self.candidate_bytes
    }

    pub fn decoded_candidate(&self) -> Result<ExecutionCandidateV1, FabricAuthorityError> {
        self.signed_receipt
            .receipt
            .validate_candidate_bytes(&self.candidate_bytes)
    }

    pub(crate) fn decoded_candidate_representation(
        &self,
    ) -> Result<ExecutionCandidateV1, FabricAuthorityError> {
        crate::execution_fabric::decode_execution_candidate_representation_v1(&self.candidate_bytes)
            .map_err(|error| invalid(format!("terminal candidate: {error}")))
    }

    pub fn validate(&self) -> Result<(), FabricAuthorityError> {
        self.validate_transport_shape()?;
        self.signed_receipt.validate_shape()?;
        self.decoded_candidate().map(|_| ())
    }

    pub(crate) fn validate_transport_shape(&self) -> Result<(), FabricAuthorityError> {
        self.signed_receipt.validate_representation_shape()?;
        self.decoded_candidate_representation().map(|_| ())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FabricResponseV1 {
    Accepted(FabricAttemptStatusV1),
    Running(FabricAttemptStatusV1),
    TerminalCandidate(FabricTerminalCandidateV1),
    Rejected(FabricRejectionV1),
    Abandoned(FabricAbandonmentV1),
}

pub(crate) fn validate_lower_hex(
    field: &str,
    value: &str,
    expected_bytes: usize,
) -> Result<(), FabricAuthorityError> {
    if value.len() != expected_bytes * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(format!(
            "{field} must be exactly {expected_bytes} lowercase hexadecimal bytes"
        )));
    }
    Ok(())
}
