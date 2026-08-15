use serde::{Deserialize, Deserializer, Serialize};

use crate::world::ArtifactId;

use super::digest::{validate_fresh, validate_token, validate_window};
use super::{
    CanonicalPlacementRecordV1, GenerationV1, PlacementValidationError, SemanticDigestV1,
    TargetDescriptorV1, UnixMillisV1,
};

pub const MAX_NODE_PROFILE_LIFETIME_MS: u64 = 60_000;
pub const MAX_CAPACITY_OBSERVATION_LIFETIME_MS: u64 = 5_000;
pub const MAX_PLACEMENT_LEASE_LIFETIME_MS: u64 = 30_000;

/// Authentication query handed to a transport- or registry-owned verifier.
/// The core deliberately never treats a signer name or key digest as proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordAuthenticationV1 {
    record_kind: &'static str,
    issuer_key: SemanticDigestV1,
    record_digest: SemanticDigestV1,
}

impl RecordAuthenticationV1 {
    pub(crate) fn new(
        record_kind: &'static str,
        issuer_key: SemanticDigestV1,
        record_digest: SemanticDigestV1,
    ) -> Self {
        Self {
            record_kind,
            issuer_key,
            record_digest,
        }
    }

    pub fn record_kind(&self) -> &'static str {
        self.record_kind
    }

    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        &self.issuer_key
    }

    pub fn record_digest(&self) -> &SemanticDigestV1 {
        &self.record_digest
    }
}

/// Signature-independent hook.  A later transport can verify an Ed25519
/// envelope, a pinned registry record, or another authenticated carrier and
/// answer this exact digest query without changing placement semantics.
pub trait RecordAuthenticatorV1 {
    fn authenticate(&self, record: &RecordAuthenticationV1) -> bool;
}

fn require_authenticated(
    record_kind: &'static str,
    issuer_key: &SemanticDigestV1,
    record_digest: SemanticDigestV1,
    authenticator: &impl RecordAuthenticatorV1,
) -> Result<(), PlacementValidationError> {
    let authentication =
        RecordAuthenticationV1::new(record_kind, issuer_key.clone(), record_digest);
    if authenticator.authenticate(&authentication) {
        Ok(())
    } else {
        Err(PlacementValidationError::Unauthenticated {
            record: record_kind,
        })
    }
}

/// Short-lived signed projection of one stable target descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeProfileV1 {
    issuer_key: SemanticDigestV1,
    descriptor: TargetDescriptorV1,
    profile_generation: GenerationV1,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl NodeProfileV1 {
    pub fn new(
        issuer_key: SemanticDigestV1,
        descriptor: TargetDescriptorV1,
        profile_generation: GenerationV1,
        issued_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        validate_window(
            "node profile",
            issued_at,
            expires_at,
            MAX_NODE_PROFILE_LIFETIME_MS,
        )?;
        Ok(Self {
            issuer_key,
            descriptor,
            profile_generation,
            issued_at,
            expires_at,
        })
    }

    pub fn descriptor(&self) -> &TargetDescriptorV1 {
        &self.descriptor
    }

    pub fn descriptor_digest(&self) -> Result<SemanticDigestV1, PlacementValidationError> {
        self.descriptor.semantic_digest()
    }

    pub fn profile_generation(&self) -> GenerationV1 {
        self.profile_generation
    }

    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        &self.issuer_key
    }

    pub fn issued_at(&self) -> UnixMillisV1 {
        self.issued_at
    }

    pub fn expires_at(&self) -> UnixMillisV1 {
        self.expires_at
    }

    pub fn validate_at(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        validate_fresh("node profile", self.issued_at, self.expires_at, now)?;
        require_authenticated(
            "node profile",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

impl CanonicalPlacementRecordV1 for NodeProfileV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/node-profile/v1";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeProfileWireV1 {
    issuer_key: SemanticDigestV1,
    descriptor: TargetDescriptorV1,
    profile_generation: GenerationV1,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for NodeProfileV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = NodeProfileWireV1::deserialize(deserializer)?;
        Self::new(
            wire.issuer_key,
            wire.descriptor,
            wire.profile_generation,
            wire.issued_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Fast-changing resource availability, intentionally separated from the
/// stable target descriptor and artifact cache key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityObservationV1 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    total_cpu_slots: u32,
    free_cpu_slots: u32,
    total_memory_bytes: u64,
    free_memory_bytes: u64,
    total_scratch_bytes: u64,
    free_scratch_bytes: u64,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl CapacityObservationV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer_key: SemanticDigestV1,
        node_id: impl Into<String>,
        target_descriptor: SemanticDigestV1,
        profile_generation: GenerationV1,
        capacity_generation: GenerationV1,
        total_cpu_slots: u32,
        free_cpu_slots: u32,
        total_memory_bytes: u64,
        free_memory_bytes: u64,
        total_scratch_bytes: u64,
        free_scratch_bytes: u64,
        issued_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("capacity node identity", &node_id)?;
        validate_window(
            "capacity observation",
            issued_at,
            expires_at,
            MAX_CAPACITY_OBSERVATION_LIFETIME_MS,
        )?;
        if total_cpu_slots == 0
            || free_cpu_slots > total_cpu_slots
            || free_memory_bytes > total_memory_bytes
            || free_scratch_bytes > total_scratch_bytes
        {
            return Err(PlacementValidationError::InsufficientCapacity);
        }
        Ok(Self {
            issuer_key,
            node_id,
            target_descriptor,
            profile_generation,
            capacity_generation,
            total_cpu_slots,
            free_cpu_slots,
            total_memory_bytes,
            free_memory_bytes,
            total_scratch_bytes,
            free_scratch_bytes,
            issued_at,
            expires_at,
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn target_descriptor(&self) -> &SemanticDigestV1 {
        &self.target_descriptor
    }

    pub fn profile_generation(&self) -> GenerationV1 {
        self.profile_generation
    }

    pub fn capacity_generation(&self) -> GenerationV1 {
        self.capacity_generation
    }

    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        &self.issuer_key
    }

    pub fn fits(&self, reservation: &PlacementReservationV1) -> bool {
        self.free_cpu_slots >= reservation.cpu_slots
            && self.free_memory_bytes >= reservation.memory_bytes
            && self.free_scratch_bytes >= reservation.scratch_bytes
    }

    pub fn validate_for_profile(
        &self,
        profile: &NodeProfileV1,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        validate_fresh("capacity observation", self.issued_at, self.expires_at, now)?;
        require_equal(
            "capacity node identity",
            profile.descriptor().node_id(),
            &self.node_id,
        )?;
        require_equal(
            "capacity target descriptor",
            &profile.descriptor_digest()?.to_string(),
            &self.target_descriptor.to_string(),
        )?;
        require_equal(
            "capacity profile generation",
            &profile.profile_generation().get().to_string(),
            &self.profile_generation.get().to_string(),
        )?;
        require_authenticated(
            "capacity observation",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

impl CanonicalPlacementRecordV1 for CapacityObservationV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/capacity-observation/v1";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityObservationWireV1 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    total_cpu_slots: u32,
    free_cpu_slots: u32,
    total_memory_bytes: u64,
    free_memory_bytes: u64,
    total_scratch_bytes: u64,
    free_scratch_bytes: u64,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for CapacityObservationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CapacityObservationWireV1::deserialize(deserializer)?;
        Self::new(
            wire.issuer_key,
            wire.node_id,
            wire.target_descriptor,
            wire.profile_generation,
            wire.capacity_generation,
            wire.total_cpu_slots,
            wire.free_cpu_slots,
            wire.total_memory_bytes,
            wire.free_memory_bytes,
            wire.total_scratch_bytes,
            wire.free_scratch_bytes,
            wire.issued_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementReservationV1 {
    cpu_slots: u32,
    memory_bytes: u64,
    scratch_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementReservationWireV1 {
    cpu_slots: u32,
    memory_bytes: u64,
    scratch_bytes: u64,
}

impl<'de> Deserialize<'de> for PlacementReservationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PlacementReservationWireV1::deserialize(deserializer)?;
        Self::new(wire.cpu_slots, wire.memory_bytes, wire.scratch_bytes)
            .map_err(serde::de::Error::custom)
    }
}

impl PlacementReservationV1 {
    pub fn new(
        cpu_slots: u32,
        memory_bytes: u64,
        scratch_bytes: u64,
    ) -> Result<Self, PlacementValidationError> {
        if cpu_slots == 0 {
            return Err(PlacementValidationError::Zero {
                field: "reserved CPU slots",
            });
        }
        Ok(Self {
            cpu_slots,
            memory_bytes,
            scratch_bytes,
        })
    }

    pub fn cpu_slots(&self) -> u32 {
        self.cpu_slots
    }

    pub fn memory_bytes(&self) -> u64 {
        self.memory_bytes
    }

    pub fn scratch_bytes(&self) -> u64 {
        self.scratch_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAttemptIdV1 {
    task: SemanticDigestV1,
    attempt: GenerationV1,
}

impl TaskAttemptIdV1 {
    pub fn new(task: SemanticDigestV1, attempt: GenerationV1) -> Self {
        Self { task, attempt }
    }

    pub fn task(&self) -> &SemanticDigestV1 {
        &self.task
    }

    pub fn attempt(&self) -> GenerationV1 {
        self.attempt
    }
}

/// One-use, short-lived reservation authority for an exact prepared attempt.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementLeaseV1 {
    issuer_key: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    operation_oir: ArtifactId,
    requirement_footprint: SemanticDigestV1,
    admission: SemanticDigestV1,
    task_attempt: TaskAttemptIdV1,
    backend_implementation: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    one_use: bool,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl PlacementLeaseV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer_key: SemanticDigestV1,
        lease_nonce: SemanticDigestV1,
        expectation: LeaseExpectationV1,
        issued_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        validate_window(
            "placement lease",
            issued_at,
            expires_at,
            MAX_PLACEMENT_LEASE_LIFETIME_MS,
        )?;
        Ok(Self {
            issuer_key,
            lease_nonce,
            node_id: expectation.node_id,
            target_descriptor: expectation.target_descriptor,
            profile_generation: expectation.profile_generation,
            capacity_generation: expectation.capacity_generation,
            operation_oir: expectation.operation_oir,
            requirement_footprint: expectation.requirement_footprint,
            admission: expectation.admission,
            task_attempt: expectation.task_attempt,
            backend_implementation: expectation.backend_implementation,
            trust_policy: expectation.trust_policy,
            reservation: expectation.reservation,
            one_use: true,
            issued_at,
            expires_at,
        })
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
    }

    pub fn reservation(&self) -> &PlacementReservationV1 {
        &self.reservation
    }

    pub fn validate_for(
        &self,
        expected: &LeaseExpectationV1,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        validate_fresh("placement lease", self.issued_at, self.expires_at, now)?;
        if !self.one_use {
            return Err(PlacementValidationError::InvalidToken {
                field: "placement lease use policy",
                value: "reusable".to_owned(),
            });
        }
        macro_rules! exact {
            ($field:literal, $actual:expr, $expected:expr) => {
                require_equal($field, &$expected.to_string(), &$actual.to_string())?
            };
        }
        require_equal("lease node", &expected.node_id, &self.node_id)?;
        exact!(
            "lease target descriptor",
            self.target_descriptor,
            expected.target_descriptor
        );
        exact!(
            "lease profile generation",
            self.profile_generation.get(),
            expected.profile_generation.get()
        );
        exact!(
            "lease capacity generation",
            self.capacity_generation.get(),
            expected.capacity_generation.get()
        );
        require_equal(
            "lease operation",
            expected.operation_oir.as_sha256(),
            self.operation_oir.as_sha256(),
        )?;
        exact!(
            "lease requirement footprint",
            self.requirement_footprint,
            expected.requirement_footprint
        );
        exact!("lease admission", self.admission, expected.admission);
        if self.task_attempt != expected.task_attempt {
            return Err(scope_mismatch(
                "lease task attempt",
                format!(
                    "{}@{}",
                    expected.task_attempt.task(),
                    expected.task_attempt.attempt().get()
                ),
                format!(
                    "{}@{}",
                    self.task_attempt.task(),
                    self.task_attempt.attempt().get()
                ),
            ));
        }
        exact!(
            "lease backend implementation",
            self.backend_implementation,
            expected.backend_implementation
        );
        exact!(
            "lease trust policy",
            self.trust_policy,
            expected.trust_policy
        );
        if self.reservation != expected.reservation {
            return Err(scope_mismatch(
                "lease reservation",
                format!("{:?}", expected.reservation),
                format!("{:?}", self.reservation),
            ));
        }
        require_authenticated(
            "placement lease",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

impl CanonicalPlacementRecordV1 for PlacementLeaseV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/lease/v1";
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseExpectationV1 {
    pub node_id: String,
    pub target_descriptor: SemanticDigestV1,
    pub profile_generation: GenerationV1,
    pub capacity_generation: GenerationV1,
    pub operation_oir: ArtifactId,
    pub requirement_footprint: SemanticDigestV1,
    pub admission: SemanticDigestV1,
    pub task_attempt: TaskAttemptIdV1,
    pub backend_implementation: SemanticDigestV1,
    pub trust_policy: SemanticDigestV1,
    pub reservation: PlacementReservationV1,
}

impl LeaseExpectationV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: impl Into<String>,
        target_descriptor: SemanticDigestV1,
        profile_generation: GenerationV1,
        capacity_generation: GenerationV1,
        operation_oir: ArtifactId,
        requirement_footprint: SemanticDigestV1,
        admission: SemanticDigestV1,
        task_attempt: TaskAttemptIdV1,
        backend_implementation: SemanticDigestV1,
        trust_policy: SemanticDigestV1,
        reservation: PlacementReservationV1,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("lease expectation node", &node_id)?;
        Ok(Self {
            node_id,
            target_descriptor,
            profile_generation,
            capacity_generation,
            operation_oir,
            requirement_footprint,
            admission,
            task_attempt,
            backend_implementation,
            trust_policy,
            reservation,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementLeaseWireV1 {
    issuer_key: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    operation_oir: ArtifactId,
    requirement_footprint: SemanticDigestV1,
    admission: SemanticDigestV1,
    task_attempt: TaskAttemptIdV1,
    backend_implementation: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    one_use: bool,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for PlacementLeaseV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PlacementLeaseWireV1::deserialize(deserializer)?;
        if !wire.one_use {
            return Err(serde::de::Error::custom("placement lease must be one-use"));
        }
        let expectation = LeaseExpectationV1::new(
            wire.node_id,
            wire.target_descriptor,
            wire.profile_generation,
            wire.capacity_generation,
            wire.operation_oir,
            wire.requirement_footprint,
            wire.admission,
            wire.task_attempt,
            wire.backend_implementation,
            wire.trust_policy,
            wire.reservation,
        )
        .map_err(serde::de::Error::custom)?;
        Self::new(
            wire.issuer_key,
            wire.lease_nonce,
            expectation,
            wire.issued_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

pub(crate) fn scope_mismatch(
    field: &'static str,
    expected: impl Into<String>,
    got: impl Into<String>,
) -> PlacementValidationError {
    PlacementValidationError::ScopeMismatch {
        field,
        expected: expected.into(),
        got: got.into(),
    }
}

pub(crate) fn require_equal(
    field: &'static str,
    expected: &str,
    got: &str,
) -> Result<(), PlacementValidationError> {
    if expected == got {
        Ok(())
    } else {
        Err(scope_mismatch(field, expected, got))
    }
}
