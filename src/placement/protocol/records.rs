use serde::{Deserialize, Deserializer, Serialize};

use crate::world::ArtifactId;

use super::digest::{validate_fresh, validate_token, validate_window};
use super::state::{StateReservationV2, StateSessionIdV2};
use super::{
    CanonicalPlacementRecordV1, CurrentBackendCatalogV1, GenerationV1, PlacementValidationError,
    SemanticDigestV1, TargetDescriptorV1, UnixMillisV1,
};

pub const MAX_NODE_PROFILE_LIFETIME_MS: u64 = 60_000;
pub const MAX_CAPACITY_OBSERVATION_LIFETIME_MS: u64 = 5_000;
pub const MAX_PLACEMENT_LEASE_LIFETIME_MS: u64 = 30_000;

/// Authentication query handed to a transport- or registry-owned protocol verifier.
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

pub(super) fn require_authenticated(
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

    pub fn validate_at_with_catalog(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
        catalog: &impl CurrentBackendCatalogV1,
    ) -> Result<(), PlacementValidationError> {
        self.descriptor
            .validate_current_backend_catalog_with(catalog)?;
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

    pub fn expires_at(&self) -> UnixMillisV1 {
        self.expires_at
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

/// Exact state authority carried by a placement lease.
///
/// `None` means the command neither opens nor resumes backend state. `Open`
/// binds the separately authenticated state-capacity observation and the hard
/// reservation admitted against it. `Existing` binds one accepted session;
/// stateful actors carry their exact actor-generation digest, while a
/// stateless session may omit it because no actor state is being resumed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum LeaseStateBindingV2 {
    None,
    Open {
        state_capacity_observation: SemanticDigestV1,
        reservation: StateReservationV2,
    },
    Existing {
        session: StateSessionIdV2,
        actor_generation: Option<SemanticDigestV1>,
    },
}

impl LeaseStateBindingV2 {
    pub fn open(
        state_capacity_observation: SemanticDigestV1,
        reservation: StateReservationV2,
    ) -> Self {
        Self::Open {
            state_capacity_observation,
            reservation,
        }
    }

    pub fn existing(session: StateSessionIdV2, actor_generation: Option<SemanticDigestV1>) -> Self {
        Self::Existing {
            session,
            actor_generation,
        }
    }

    pub fn state_capacity_observation(&self) -> Option<&SemanticDigestV1> {
        match self {
            Self::Open {
                state_capacity_observation,
                ..
            } => Some(state_capacity_observation),
            Self::None | Self::Existing { .. } => None,
        }
    }

    pub fn reservation(&self) -> Option<&StateReservationV2> {
        match self {
            Self::Open { reservation, .. } => Some(reservation),
            Self::None | Self::Existing { .. } => None,
        }
    }

    pub fn session(&self) -> Option<&StateSessionIdV2> {
        match self {
            Self::Existing { session, .. } => Some(session),
            Self::None | Self::Open { .. } => None,
        }
    }

    pub fn actor_generation(&self) -> Option<&SemanticDigestV1> {
        match self {
            Self::Existing {
                actor_generation, ..
            } => actor_generation.as_ref(),
            Self::None | Self::Open { .. } => None,
        }
    }

    fn validate_for_node(&self, node_id: &str) -> Result<(), PlacementValidationError> {
        if let Self::Existing { session, .. } = self {
            require_equal("lease state session node", node_id, session.node_id())?;
        }
        Ok(())
    }
}

/// Complete execution scope expected by a placement-lease V2 consumer.
///
/// This is a canonical record in its own right so scheduler/transport adapters
/// can compare or log the exact expectation without inventing a second field
/// projection. Envelope authority (issuer, nonce, and validity) remains on the
/// lease and is checked by `PlacementLeaseV2::validate_for`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseExpectationV2 {
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    capacity_observation: SemanticDigestV1,
    candidate_eligibility: SemanticDigestV1,
    operation_oir: ArtifactId,
    requirement_footprint: SemanticDigestV1,
    warrant_discharge: SemanticDigestV1,
    admission: SemanticDigestV1,
    task_attempt: TaskAttemptIdV1,
    backend_implementation: SemanticDigestV1,
    realization_pipeline: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    hosted_command_binding: SemanticDigestV1,
    state_binding: LeaseStateBindingV2,
}

impl LeaseExpectationV2 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: impl Into<String>,
        target_descriptor: SemanticDigestV1,
        profile_generation: GenerationV1,
        capacity_generation: GenerationV1,
        capacity_observation: SemanticDigestV1,
        candidate_eligibility: SemanticDigestV1,
        operation_oir: ArtifactId,
        requirement_footprint: SemanticDigestV1,
        warrant_discharge: SemanticDigestV1,
        admission: SemanticDigestV1,
        task_attempt: TaskAttemptIdV1,
        backend_implementation: SemanticDigestV1,
        realization_pipeline: SemanticDigestV1,
        trust_policy: SemanticDigestV1,
        reservation: PlacementReservationV1,
        hosted_command_binding: SemanticDigestV1,
        state_binding: LeaseStateBindingV2,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("lease v2 expectation node", &node_id)?;
        state_binding.validate_for_node(&node_id)?;
        Ok(Self {
            node_id,
            target_descriptor,
            profile_generation,
            capacity_generation,
            capacity_observation,
            candidate_eligibility,
            operation_oir,
            requirement_footprint,
            warrant_discharge,
            admission,
            task_attempt,
            backend_implementation,
            realization_pipeline,
            trust_policy,
            reservation,
            hosted_command_binding,
            state_binding,
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

    pub fn capacity_observation(&self) -> &SemanticDigestV1 {
        &self.capacity_observation
    }

    pub fn candidate_eligibility(&self) -> &SemanticDigestV1 {
        &self.candidate_eligibility
    }

    pub fn operation_oir(&self) -> &ArtifactId {
        &self.operation_oir
    }

    pub fn requirement_footprint(&self) -> &SemanticDigestV1 {
        &self.requirement_footprint
    }

    pub fn warrant_discharge(&self) -> &SemanticDigestV1 {
        &self.warrant_discharge
    }

    pub fn admission(&self) -> &SemanticDigestV1 {
        &self.admission
    }

    pub fn task_attempt(&self) -> &TaskAttemptIdV1 {
        &self.task_attempt
    }

    pub fn backend_implementation(&self) -> &SemanticDigestV1 {
        &self.backend_implementation
    }

    pub fn realization_pipeline(&self) -> &SemanticDigestV1 {
        &self.realization_pipeline
    }

    pub fn trust_policy(&self) -> &SemanticDigestV1 {
        &self.trust_policy
    }

    pub fn reservation(&self) -> &PlacementReservationV1 {
        &self.reservation
    }

    pub fn hosted_command_binding(&self) -> &SemanticDigestV1 {
        &self.hosted_command_binding
    }

    pub fn state_binding(&self) -> &LeaseStateBindingV2 {
        &self.state_binding
    }
}

impl CanonicalPlacementRecordV1 for LeaseExpectationV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/lease-expectation/v2";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseExpectationWireV2 {
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    capacity_observation: SemanticDigestV1,
    candidate_eligibility: SemanticDigestV1,
    operation_oir: ArtifactId,
    requirement_footprint: SemanticDigestV1,
    warrant_discharge: SemanticDigestV1,
    admission: SemanticDigestV1,
    task_attempt: TaskAttemptIdV1,
    backend_implementation: SemanticDigestV1,
    realization_pipeline: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    hosted_command_binding: SemanticDigestV1,
    state_binding: LeaseStateBindingV2,
}

impl<'de> Deserialize<'de> for LeaseExpectationV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = LeaseExpectationWireV2::deserialize(deserializer)?;
        Self::new(
            wire.node_id,
            wire.target_descriptor,
            wire.profile_generation,
            wire.capacity_generation,
            wire.capacity_observation,
            wire.candidate_eligibility,
            wire.operation_oir,
            wire.requirement_footprint,
            wire.warrant_discharge,
            wire.admission,
            wire.task_attempt,
            wire.backend_implementation,
            wire.realization_pipeline,
            wire.trust_policy,
            wire.reservation,
            wire.hosted_command_binding,
            wire.state_binding,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// One-use, short-lived execution authority binding eligibility, warrants,
/// capacity, exact realization, hosted command context, and state.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementLeaseV2 {
    issuer_key: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    capacity_observation: SemanticDigestV1,
    candidate_eligibility: SemanticDigestV1,
    operation_oir: ArtifactId,
    requirement_footprint: SemanticDigestV1,
    warrant_discharge: SemanticDigestV1,
    admission: SemanticDigestV1,
    task_attempt: TaskAttemptIdV1,
    backend_implementation: SemanticDigestV1,
    realization_pipeline: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    hosted_command_binding: SemanticDigestV1,
    state_binding: LeaseStateBindingV2,
    one_use: bool,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl PlacementLeaseV2 {
    pub fn new(
        issuer_key: SemanticDigestV1,
        lease_nonce: SemanticDigestV1,
        expectation: LeaseExpectationV2,
        issued_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        validate_window(
            "placement lease v2",
            issued_at,
            expires_at,
            MAX_PLACEMENT_LEASE_LIFETIME_MS,
        )?;
        expectation
            .state_binding
            .validate_for_node(&expectation.node_id)?;
        Ok(Self {
            issuer_key,
            lease_nonce,
            node_id: expectation.node_id,
            target_descriptor: expectation.target_descriptor,
            profile_generation: expectation.profile_generation,
            capacity_generation: expectation.capacity_generation,
            capacity_observation: expectation.capacity_observation,
            candidate_eligibility: expectation.candidate_eligibility,
            operation_oir: expectation.operation_oir,
            requirement_footprint: expectation.requirement_footprint,
            warrant_discharge: expectation.warrant_discharge,
            admission: expectation.admission,
            task_attempt: expectation.task_attempt,
            backend_implementation: expectation.backend_implementation,
            realization_pipeline: expectation.realization_pipeline,
            trust_policy: expectation.trust_policy,
            reservation: expectation.reservation,
            hosted_command_binding: expectation.hosted_command_binding,
            state_binding: expectation.state_binding,
            one_use: true,
            issued_at,
            expires_at,
        })
    }

    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        &self.issuer_key
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
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

    pub fn capacity_observation(&self) -> &SemanticDigestV1 {
        &self.capacity_observation
    }

    pub fn candidate_eligibility(&self) -> &SemanticDigestV1 {
        &self.candidate_eligibility
    }

    pub fn operation_oir(&self) -> &ArtifactId {
        &self.operation_oir
    }

    pub fn requirement_footprint(&self) -> &SemanticDigestV1 {
        &self.requirement_footprint
    }

    pub fn warrant_discharge(&self) -> &SemanticDigestV1 {
        &self.warrant_discharge
    }

    pub fn admission(&self) -> &SemanticDigestV1 {
        &self.admission
    }

    pub fn task_attempt(&self) -> &TaskAttemptIdV1 {
        &self.task_attempt
    }

    pub fn backend_implementation(&self) -> &SemanticDigestV1 {
        &self.backend_implementation
    }

    pub fn realization_pipeline(&self) -> &SemanticDigestV1 {
        &self.realization_pipeline
    }

    pub fn trust_policy(&self) -> &SemanticDigestV1 {
        &self.trust_policy
    }

    pub fn reservation(&self) -> &PlacementReservationV1 {
        &self.reservation
    }

    pub fn hosted_command_binding(&self) -> &SemanticDigestV1 {
        &self.hosted_command_binding
    }

    pub fn state_binding(&self) -> &LeaseStateBindingV2 {
        &self.state_binding
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

    pub fn validate_for(
        &self,
        expected: &LeaseExpectationV2,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        validate_fresh("placement lease v2", self.issued_at, self.expires_at, now)?;
        if !self.one_use {
            return Err(PlacementValidationError::InvalidToken {
                field: "placement lease v2 use policy",
                value: "reusable".to_owned(),
            });
        }
        self.state_binding.validate_for_node(&self.node_id)?;

        macro_rules! exact {
            ($field:literal, $actual:expr, $expected:expr) => {
                require_equal($field, &$expected.to_string(), &$actual.to_string())?
            };
        }
        require_equal("lease v2 node", &expected.node_id, &self.node_id)?;
        exact!(
            "lease v2 target descriptor",
            self.target_descriptor,
            expected.target_descriptor
        );
        exact!(
            "lease v2 profile generation",
            self.profile_generation.get(),
            expected.profile_generation.get()
        );
        exact!(
            "lease v2 capacity generation",
            self.capacity_generation.get(),
            expected.capacity_generation.get()
        );
        exact!(
            "lease v2 capacity observation",
            self.capacity_observation,
            expected.capacity_observation
        );
        exact!(
            "lease v2 candidate eligibility",
            self.candidate_eligibility,
            expected.candidate_eligibility
        );
        require_equal(
            "lease v2 operation",
            expected.operation_oir.as_sha256(),
            self.operation_oir.as_sha256(),
        )?;
        exact!(
            "lease v2 requirement footprint",
            self.requirement_footprint,
            expected.requirement_footprint
        );
        exact!(
            "lease v2 warrant discharge",
            self.warrant_discharge,
            expected.warrant_discharge
        );
        exact!("lease v2 admission", self.admission, expected.admission);
        require_exact_debug(
            "lease v2 task attempt",
            &expected.task_attempt,
            &self.task_attempt,
        )?;
        exact!(
            "lease v2 backend implementation",
            self.backend_implementation,
            expected.backend_implementation
        );
        exact!(
            "lease v2 realization pipeline",
            self.realization_pipeline,
            expected.realization_pipeline
        );
        exact!(
            "lease v2 trust policy",
            self.trust_policy,
            expected.trust_policy
        );
        require_exact_debug(
            "lease v2 compute reservation",
            &expected.reservation,
            &self.reservation,
        )?;
        exact!(
            "lease v2 hosted command binding",
            self.hosted_command_binding,
            expected.hosted_command_binding
        );
        require_exact_debug(
            "lease v2 state binding",
            &expected.state_binding,
            &self.state_binding,
        )?;
        require_authenticated(
            "placement lease v2",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

impl CanonicalPlacementRecordV1 for PlacementLeaseV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/lease/v2";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementLeaseWireV2 {
    issuer_key: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    capacity_observation: SemanticDigestV1,
    candidate_eligibility: SemanticDigestV1,
    operation_oir: ArtifactId,
    requirement_footprint: SemanticDigestV1,
    warrant_discharge: SemanticDigestV1,
    admission: SemanticDigestV1,
    task_attempt: TaskAttemptIdV1,
    backend_implementation: SemanticDigestV1,
    realization_pipeline: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    hosted_command_binding: SemanticDigestV1,
    state_binding: LeaseStateBindingV2,
    one_use: bool,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for PlacementLeaseV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PlacementLeaseWireV2::deserialize(deserializer)?;
        if !wire.one_use {
            return Err(serde::de::Error::custom(
                "placement lease v2 must be one-use",
            ));
        }
        let expectation = LeaseExpectationV2::new(
            wire.node_id,
            wire.target_descriptor,
            wire.profile_generation,
            wire.capacity_generation,
            wire.capacity_observation,
            wire.candidate_eligibility,
            wire.operation_oir,
            wire.requirement_footprint,
            wire.warrant_discharge,
            wire.admission,
            wire.task_attempt,
            wire.backend_implementation,
            wire.realization_pipeline,
            wire.trust_policy,
            wire.reservation,
            wire.hosted_command_binding,
            wire.state_binding,
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

/// Exact non-execution scope expected by a hosted state-control consumer.
///
/// Open and recovery commands still carry a full recomputable placement proof
/// for their selected node/backend/state binding, but intentionally have no
/// operation OIR, compiler admission, or task-attempt coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateControlExpectationV2 {
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    capacity_observation: SemanticDigestV1,
    candidate_eligibility: SemanticDigestV1,
    requirement_footprint: SemanticDigestV1,
    warrant_discharge: SemanticDigestV1,
    backend_implementation: SemanticDigestV1,
    realization_pipeline: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    hosted_command_binding: SemanticDigestV1,
    state_binding: LeaseStateBindingV2,
}

impl StateControlExpectationV2 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: impl Into<String>,
        target_descriptor: SemanticDigestV1,
        profile_generation: GenerationV1,
        capacity_generation: GenerationV1,
        capacity_observation: SemanticDigestV1,
        candidate_eligibility: SemanticDigestV1,
        requirement_footprint: SemanticDigestV1,
        warrant_discharge: SemanticDigestV1,
        backend_implementation: SemanticDigestV1,
        realization_pipeline: SemanticDigestV1,
        trust_policy: SemanticDigestV1,
        reservation: PlacementReservationV1,
        hosted_command_binding: SemanticDigestV1,
        state_binding: LeaseStateBindingV2,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("state-control expectation node", &node_id)?;
        state_binding.validate_for_node(&node_id)?;
        Ok(Self {
            node_id,
            target_descriptor,
            profile_generation,
            capacity_generation,
            capacity_observation,
            candidate_eligibility,
            requirement_footprint,
            warrant_discharge,
            backend_implementation,
            realization_pipeline,
            trust_policy,
            reservation,
            hosted_command_binding,
            state_binding,
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
    pub fn capacity_observation(&self) -> &SemanticDigestV1 {
        &self.capacity_observation
    }
    pub fn candidate_eligibility(&self) -> &SemanticDigestV1 {
        &self.candidate_eligibility
    }
    pub fn requirement_footprint(&self) -> &SemanticDigestV1 {
        &self.requirement_footprint
    }
    pub fn warrant_discharge(&self) -> &SemanticDigestV1 {
        &self.warrant_discharge
    }
    pub fn backend_implementation(&self) -> &SemanticDigestV1 {
        &self.backend_implementation
    }
    pub fn realization_pipeline(&self) -> &SemanticDigestV1 {
        &self.realization_pipeline
    }
    pub fn trust_policy(&self) -> &SemanticDigestV1 {
        &self.trust_policy
    }
    pub fn reservation(&self) -> &PlacementReservationV1 {
        &self.reservation
    }
    pub fn hosted_command_binding(&self) -> &SemanticDigestV1 {
        &self.hosted_command_binding
    }
    pub fn state_binding(&self) -> &LeaseStateBindingV2 {
        &self.state_binding
    }
}

impl CanonicalPlacementRecordV1 for StateControlExpectationV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-control-expectation/v2";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateControlExpectationWireV2 {
    node_id: String,
    target_descriptor: SemanticDigestV1,
    profile_generation: GenerationV1,
    capacity_generation: GenerationV1,
    capacity_observation: SemanticDigestV1,
    candidate_eligibility: SemanticDigestV1,
    requirement_footprint: SemanticDigestV1,
    warrant_discharge: SemanticDigestV1,
    backend_implementation: SemanticDigestV1,
    realization_pipeline: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
    hosted_command_binding: SemanticDigestV1,
    state_binding: LeaseStateBindingV2,
}

impl<'de> Deserialize<'de> for StateControlExpectationV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateControlExpectationWireV2::deserialize(deserializer)?;
        Self::new(
            wire.node_id,
            wire.target_descriptor,
            wire.profile_generation,
            wire.capacity_generation,
            wire.capacity_observation,
            wire.candidate_eligibility,
            wire.requirement_footprint,
            wire.warrant_discharge,
            wire.backend_implementation,
            wire.realization_pipeline,
            wire.trust_policy,
            wire.reservation,
            wire.hosted_command_binding,
            wire.state_binding,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// One-use, short-lived authority for OpenSession and Recover only.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateControlLeaseV2 {
    issuer_key: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    expectation: StateControlExpectationV2,
    one_use: bool,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl StateControlLeaseV2 {
    pub fn new(
        issuer_key: SemanticDigestV1,
        lease_nonce: SemanticDigestV1,
        expectation: StateControlExpectationV2,
        issued_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        validate_window(
            "state-control lease v2",
            issued_at,
            expires_at,
            MAX_PLACEMENT_LEASE_LIFETIME_MS,
        )?;
        Ok(Self {
            issuer_key,
            lease_nonce,
            expectation,
            one_use: true,
            issued_at,
            expires_at,
        })
    }

    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        &self.issuer_key
    }
    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
    }
    pub fn expectation(&self) -> &StateControlExpectationV2 {
        &self.expectation
    }
    pub fn hosted_command_binding(&self) -> &SemanticDigestV1 {
        self.expectation.hosted_command_binding()
    }
    pub fn state_binding(&self) -> &LeaseStateBindingV2 {
        self.expectation.state_binding()
    }
    pub fn issued_at(&self) -> UnixMillisV1 {
        self.issued_at
    }
    pub fn expires_at(&self) -> UnixMillisV1 {
        self.expires_at
    }

    pub fn validate_for(
        &self,
        expected: &StateControlExpectationV2,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        validate_fresh(
            "state-control lease v2",
            self.issued_at,
            self.expires_at,
            now,
        )?;
        if !self.one_use {
            return Err(PlacementValidationError::InvalidToken {
                field: "state-control lease v2 use policy",
                value: "reusable".to_owned(),
            });
        }
        require_exact_debug(
            "state-control lease expectation",
            expected,
            &self.expectation,
        )?;
        require_authenticated(
            "state-control lease v2",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

impl CanonicalPlacementRecordV1 for StateControlLeaseV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-control-lease/v2";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateControlLeaseWireV2 {
    issuer_key: SemanticDigestV1,
    lease_nonce: SemanticDigestV1,
    expectation: StateControlExpectationV2,
    one_use: bool,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for StateControlLeaseV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateControlLeaseWireV2::deserialize(deserializer)?;
        if !wire.one_use {
            return Err(serde::de::Error::custom(
                "state-control lease v2 must be one-use",
            ));
        }
        Self::new(
            wire.issuer_key,
            wire.lease_nonce,
            wire.expectation,
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

fn require_exact_debug<T: PartialEq + std::fmt::Debug>(
    field: &'static str,
    expected: &T,
    got: &T,
) -> Result<(), PlacementValidationError> {
    if expected == got {
        Ok(())
    } else {
        Err(scope_mismatch(
            field,
            format!("{expected:?}"),
            format!("{got:?}"),
        ))
    }
}
