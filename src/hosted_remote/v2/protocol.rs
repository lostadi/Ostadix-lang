use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::placement::{
    ActorGenerationIdV1, BackendStateSupportV2, CanonicalPlacementRecordV1, CapacityObservationV1,
    GenerationV1, NodeProfileV1, PlacementLeaseV2, PlacementReservationV1, PlacementTrustPolicyV1,
    PlacementWarrantV1, RequirementFootprintV1, SemanticDigestV1, StateCapacityObservationV2,
    StateControlLeaseV2, StateQuotaLimitsV2, StateReservationV2, StateSessionIdV2, TaskAttemptIdV1,
    UnixMillisV1, WarrantDischargeV1,
};
use crate::value::OValue;

use super::super::protocol::{
    canonical_hosted_sha256, sha256_hex, truncate_hosted_error_message, MAX_HOSTED_ID_BYTES,
    MAX_HOSTED_OUTPUT_BYTES, MAX_HOSTED_SOURCE_BYTES,
};

pub const HOSTED_PROTOCOL_V2: &str = "ostadix.hosted-transport/v2";
pub const HOSTED_SESSION_SCHEMA_V2: &str = "ostadix.hosted-session/v2";
pub const HOSTED_OPERATION_SCHEMA_V2: &str = "ostadix.hosted-operation/v2";
pub const HOSTED_JOURNAL_ENTRY_SCHEMA_V2: &str = "ostadix.hosted-journal-entry/v2";
pub const HOSTED_SIGNED_ENTRY_SCHEMA_V2: &str = "ostadix.hosted-signed-entry/v2";
pub const HOSTED_PLACEMENT_LEASE_SCHEMA_V2: &str = "ostadix.hosted-placement-lease/v2";
pub const HOSTED_COMMAND_BINDING_SCHEMA_V2: &str = "ostadix.hosted-command-binding/v2";
pub const HOSTED_OPEN_CAPABILITY_COMMITMENT_SCHEMA_V2: &str =
    "ostadix.hosted-open-capability-commitment/v2";
pub const HOSTED_PLACEMENT_EVIDENCE_SCHEMA_V2: &str = "ostadix.hosted-placement-evidence/v2";
pub const HOSTED_RECOVERY_WARRANT_SCHEMA_V2: &str = "ostadix.hosted-recovery-warrant/v2";

pub const DEFAULT_MAX_OPEN_SESSIONS_V2: u32 = 64;
pub const DEFAULT_MAX_ACTORS_PER_SESSION_V2: u32 = 1;
pub const DEFAULT_MAX_SNAPSHOT_BYTES_PER_ACTOR_V2: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_STATE_BYTES_TOTAL_V2: u64 = 4 * 1024 * 1024 * 1024;

/// A fixed codec-safety bound, not a scheduler capacity policy.  Durable state
/// admission is governed only by canonical `StateQuotaLimitsV2` and the exact
/// reservation carried by the placement lease.
pub const MAX_OPERATIONS_PER_SESSION_V2: usize = 4096;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStateTierV2 {
    /// Every operation receives a fresh evaluator and root scope.
    Stateless,
    /// A backend-specific checkpoint adapter can reconstruct actor state.
    CheckpointRestore,
    /// State can be reconstructed by replay under an exact recovery warrant.
    ReplayReconstructible,
    /// State survives only while this node process retains the actor.
    LiveActorOnly,
}

impl SessionStateTierV2 {
    pub fn needs_live_actor(self) -> bool {
        !matches!(self, Self::Stateless)
    }

    pub fn validate_backend_support(self, support: &BackendStateSupportV2) -> Result<()> {
        match (self, support) {
            (Self::Stateless, BackendStateSupportV2::Stateless)
            | (
                Self::CheckpointRestore,
                BackendStateSupportV2::SemanticSnapshot { .. },
            )
            | (
                Self::LiveActorOnly,
                BackendStateSupportV2::ExternalPinned { .. },
            ) => Ok(()),
            (Self::ReplayReconstructible, _) => bail!(
                "ReplayReconstructible is not authorized by hosted V2: no current catalog tier and automatic replay adapter discharge it"
            ),
            (Self::Stateless, _) => {
                bail!("Stateless session requires a current Stateless backend specification")
            }
            (Self::CheckpointRestore, _) => bail!(
                "CheckpointRestore requires a current SemanticSnapshot backend specification and its exact codec/compatibility contract"
            ),
            (Self::LiveActorOnly, _) => bail!(
                "LiveActorOnly requires a current ExternalPinned backend specification; untracked-state escalation is not implemented"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusV2 {
    Ready,
    Executing,
    RecoveryRequired,
    Quarantined,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementPurposeV2 {
    OpenSession,
    Execute,
    Recover,
}

/// The exact hosted command whose digest is carried by canonical
/// `PlacementLeaseV2::hosted_command_binding`.
///
/// State identity and accounting are intentionally repeated here rather than
/// inferred from a bearer/session filename: the placement authority signs the
/// canonical lease and this binding in one envelope, while the node checks the
/// binding against its local session ledger before consuming the lease nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedCommandBindingV2 {
    pub schema: String,
    pub protocol: String,
    pub node_id: String,
    pub principal_sha256: String,
    pub state_session: StateSessionIdV2,
    pub session_state_tier: SessionStateTierV2,
    pub client_request_id: String,
    pub client_sequence: u64,
    pub purpose: PlacementPurposeV2,
    pub operation_sha256: Option<String>,
    pub recovery_warrant_sha256: Option<String>,
    /// Commitment to the client-precommitted session capability. Required for
    /// OpenSession and absent for every existing-session command.
    pub open_capability_commitment: Option<SemanticDigestV1>,
    pub state_quota_generation: GenerationV1,
    pub state_quota_limits: StateQuotaLimitsV2,
    pub state_reservation: StateReservationV2,
    pub actor_generation: Option<ActorGenerationIdV1>,
}

impl HostedCommandBindingV2 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != HOSTED_COMMAND_BINDING_SCHEMA_V2 {
            bail!(
                "unsupported hosted command binding schema `{}`",
                self.schema
            );
        }
        if self.protocol != HOSTED_PROTOCOL_V2 {
            bail!("unsupported hosted command protocol `{}`", self.protocol);
        }
        validate_identifier_v2("node_id", &self.node_id)?;
        validate_sha256_v2("principal_sha256", &self.principal_sha256)?;
        if self.state_session.node_id() != self.node_id {
            bail!("hosted command state session belongs to a different node");
        }
        validate_identifier_v2("client_request_id", &self.client_request_id)?;
        if let Some(digest) = &self.operation_sha256 {
            validate_sha256_v2("operation_sha256", digest)?;
        }
        if let Some(digest) = &self.recovery_warrant_sha256 {
            validate_sha256_v2("recovery_warrant_sha256", digest)?;
        }
        match self.purpose {
            PlacementPurposeV2::OpenSession => {
                if self.client_sequence != 0
                    || self.operation_sha256.is_some()
                    || self.recovery_warrant_sha256.is_some()
                    || self.open_capability_commitment.is_none()
                    || self.actor_generation.is_some()
                {
                    bail!(
                        "open-session lease is missing its capability commitment or carries execute/recover-only bindings"
                    );
                }
            }
            PlacementPurposeV2::Execute => {
                if self.client_sequence == 0
                    || self.operation_sha256.is_none()
                    || self.recovery_warrant_sha256.is_some()
                    || self.open_capability_commitment.is_some()
                {
                    bail!("execute lease is missing an exact session/sequence/operation binding");
                }
                // A stateful session's first execution establishes its first
                // actor, so it has no prior actor generation to bind. Runtime
                // authorization enforces None exactly in that initial state
                // and requires Some(current generation) thereafter.
            }
            PlacementPurposeV2::Recover => {
                if self.client_sequence == 0
                    || self.operation_sha256.is_some()
                    || self.recovery_warrant_sha256.is_none()
                    || self.open_capability_commitment.is_some()
                    || self.actor_generation.is_none()
                {
                    bail!("recover lease is missing an exact session/sequence/warrant binding");
                }
            }
        }
        if self.session_state_tier == SessionStateTierV2::Stateless
            && self.actor_generation.is_some()
        {
            bail!("stateless hosted command must not bind a retained actor generation");
        }
        Ok(())
    }

    pub fn semantic_digest(&self) -> Result<SemanticDigestV1> {
        self.validate()?;
        CanonicalPlacementRecordV1::semantic_digest(self).map_err(Into::into)
    }
}

impl CanonicalPlacementRecordV1 for HostedCommandBindingV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/hosted/command-binding/v2";
}

/// Full scheduler proof carried under the hosted envelope signature.
///
/// These records are intentionally not reduced to digests before they reach
/// the node: authorization recomputes candidate eligibility against the
/// current backend catalog and the exact local fragment bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedPlacementEvidenceV2 {
    pub schema: String,
    pub node_profile: NodeProfileV1,
    pub capacity_observation: CapacityObservationV1,
    pub requirement_footprint: RequirementFootprintV1,
    pub warrant_discharge: WarrantDischargeV1,
    pub warrants: Vec<PlacementWarrantV1>,
    pub trust_policy: PlacementTrustPolicyV1,
    pub reservation: PlacementReservationV1,
}

impl HostedPlacementEvidenceV2 {
    pub fn validate_shape(&self) -> Result<()> {
        if self.schema != HOSTED_PLACEMENT_EVIDENCE_SCHEMA_V2 {
            bail!(
                "unsupported hosted placement evidence schema `{}`",
                self.schema
            );
        }
        self.requirement_footprint.require_complete()?;
        let mut previous: Option<SemanticDigestV1> = None;
        for warrant in &self.warrants {
            let id = warrant
                .id()
                .map_err(anyhow::Error::from)
                .context("hosted placement warrant has no canonical identity")?;
            if previous.as_ref().is_some_and(|prior| prior >= &id) {
                bail!(
                    "hosted placement warrants must be unique and strictly sorted by canonical identity"
                );
            }
            previous = Some(id);
        }
        Ok(())
    }

    pub fn semantic_digest(&self) -> Result<SemanticDigestV1> {
        self.validate_shape()?;
        CanonicalPlacementRecordV1::semantic_digest(self).map_err(Into::into)
    }
}

impl CanonicalPlacementRecordV1 for HostedPlacementEvidenceV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/hosted/placement-evidence/v2";
}

/// Stable placement coordinates fixed when a session is opened. Fresh
/// capacity observations and operation-scoped warrants may change, but later
/// commands cannot switch target, backend, pipeline, requirement class, trust
/// policy, or compute reservation within the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedPlacementIdentityV2 {
    pub target_descriptor: SemanticDigestV1,
    pub requirement_footprint: SemanticDigestV1,
    pub backend_implementation: SemanticDigestV1,
    pub realization_pipeline: SemanticDigestV1,
    pub trust_policy: SemanticDigestV1,
    pub reservation: PlacementReservationV1,
}

impl HostedPlacementIdentityV2 {
    pub fn semantic_digest(&self) -> Result<SemanticDigestV1> {
        CanonicalPlacementRecordV1::semantic_digest(self).map_err(Into::into)
    }
}

impl CanonicalPlacementRecordV1 for HostedPlacementIdentityV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/hosted/placement-identity/v2";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "authority", content = "lease", rename_all = "kebab-case")]
pub enum HostedPlacementAuthorityV2 {
    Execution(PlacementLeaseV2),
    StateControl(StateControlLeaseV2),
}

impl HostedPlacementAuthorityV2 {
    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        match self {
            Self::Execution(lease) => lease.issuer_key(),
            Self::StateControl(lease) => lease.issuer_key(),
        }
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        match self {
            Self::Execution(lease) => lease.lease_nonce(),
            Self::StateControl(lease) => lease.lease_nonce(),
        }
    }

    pub fn hosted_command_binding(&self) -> &SemanticDigestV1 {
        match self {
            Self::Execution(lease) => lease.hosted_command_binding(),
            Self::StateControl(lease) => lease.hosted_command_binding(),
        }
    }

    pub fn semantic_digest(&self) -> Result<SemanticDigestV1> {
        match self {
            Self::Execution(lease) => Ok(lease.semantic_digest()?),
            Self::StateControl(lease) => Ok(lease.semantic_digest()?),
        }
    }

    pub fn expires_at(&self) -> UnixMillisV1 {
        match self {
            Self::Execution(lease) => lease.expires_at(),
            Self::StateControl(lease) => lease.expires_at(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedPlacementLeaseV2 {
    pub schema: String,
    pub authority: HostedPlacementAuthorityV2,
    pub command: HostedCommandBindingV2,
    pub evidence: HostedPlacementEvidenceV2,
    /// Present only for `LeaseStateBindingV2::Open`; its canonical digest is
    /// carried by the lease and its limits are checked against node policy.
    pub state_capacity_observation: Option<StateCapacityObservationV2>,
    pub signer_public_key: String,
    pub signer_key_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryWarrantV2 {
    pub schema: String,
    pub warrant_id: String,
    pub session_id: String,
    pub trigger: RecoveryTriggerV2,
    pub evidence_sha256: String,
}

impl RecoveryWarrantV2 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != HOSTED_RECOVERY_WARRANT_SCHEMA_V2 {
            bail!("unsupported recovery warrant schema `{}`", self.schema);
        }
        validate_identifier_v2("warrant_id", &self.warrant_id)?;
        validate_sha256_v2("session_id", &self.session_id)?;
        self.trigger.validate()?;
        validate_sha256_v2("evidence_sha256", &self.evidence_sha256)?;
        Ok(())
    }

    pub fn sha256(&self) -> Result<String> {
        self.validate()?;
        canonical_hosted_sha256(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayClassV2 {
    Pure,
    Idempotent,
}

/// Exact state-control fact that authorizes one recovery decision. Ambiguous
/// operation replay policy is deliberately absent from clean actor-loss
/// recovery: restoring a known durable checkpoint is not permission to replay
/// a user operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryTriggerV2 {
    AmbiguousOperation {
        operation_id: String,
        operation_sha256: String,
        replay_class: ReplayClassV2,
        stable_publication_id: Option<String>,
    },
    ActorLost {
        previous_actor_generation: ActorGenerationIdV1,
        checkpoint_sha256: String,
        checkpoint_bytes: u64,
        recovery_required_head_sha256: String,
    },
}

impl RecoveryTriggerV2 {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::AmbiguousOperation {
                operation_id,
                operation_sha256,
                replay_class,
                stable_publication_id,
            } => {
                validate_identifier_v2("ambiguous operation_id", operation_id)?;
                validate_sha256_v2("ambiguous operation_sha256", operation_sha256)?;
                match replay_class {
                    ReplayClassV2::Pure if stable_publication_id.is_some() => {
                        bail!("pure recovery trigger must not carry a publication identity")
                    }
                    ReplayClassV2::Idempotent if stable_publication_id.is_none() => {
                        bail!("idempotent recovery trigger requires a stable publication identity")
                    }
                    _ => {}
                }
                if let Some(identity) = stable_publication_id {
                    validate_identifier_v2("stable_publication_id", identity)?;
                }
            }
            Self::ActorLost {
                previous_actor_generation: _,
                checkpoint_sha256,
                checkpoint_bytes,
                recovery_required_head_sha256,
            } => {
                validate_sha256_v2("actor-loss checkpoint_sha256", checkpoint_sha256)?;
                if *checkpoint_bytes == 0 {
                    bail!("actor-loss recovery trigger requires a nonempty checkpoint");
                }
                validate_sha256_v2(
                    "actor-loss recovery_required_head_sha256",
                    recovery_required_head_sha256,
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCapabilityV2 {
    pub session_id: String,
    pub bearer: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct OpenCapabilityCommitmentRecordV2<'a> {
    schema: &'static str,
    session_id: &'a str,
    bearer: &'a str,
}

impl CanonicalPlacementRecordV1 for OpenCapabilityCommitmentRecordV2<'_> {
    const DIGEST_DOMAIN: &'static str = "ostadix/hosted/open-capability-commitment/v2";
}

/// Canonical, domain-separated commitment to the exact client-generated
/// capability. The server may retain this digest, a fresh salt, and the
/// salted bearer hash; it never needs to persist the bearer itself.
pub fn open_capability_commitment_v2(capability: &SessionCapabilityV2) -> Result<SemanticDigestV1> {
    capability.validate()?;
    Ok(OpenCapabilityCommitmentRecordV2 {
        schema: HOSTED_OPEN_CAPABILITY_COMMITMENT_SCHEMA_V2,
        session_id: &capability.session_id,
        bearer: &capability.bearer,
    }
    .semantic_digest()?)
}

impl SessionCapabilityV2 {
    pub fn validate(&self) -> Result<()> {
        validate_sha256_v2("session_id", &self.session_id)?;
        validate_capability_v2(&self.bearer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCredentialsV2 {
    pub session_id: String,
    pub bearer: String,
}

impl From<SessionCapabilityV2> for SessionCredentialsV2 {
    fn from(capability: SessionCapabilityV2) -> Self {
        Self {
            session_id: capability.session_id,
            bearer: capability.bearer,
        }
    }
}

impl SessionCredentialsV2 {
    pub fn validate(&self) -> Result<()> {
        validate_sha256_v2("session_id", &self.session_id)?;
        validate_capability_v2(&self.bearer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenSessionRequestV2 {
    pub client_request_id: String,
    pub state_tier: SessionStateTierV2,
    /// Generated and durably saved by the client before any network write.
    pub proposed_capability: SessionCapabilityV2,
    pub capability_commitment: SemanticDigestV1,
    pub placement_lease: SignedPlacementLeaseV2,
}

impl OpenSessionRequestV2 {
    pub fn validate(&self) -> Result<()> {
        validate_identifier_v2("client_request_id", &self.client_request_id)?;
        self.proposed_capability.validate()?;
        let command = &self.placement_lease.command;
        command.validate()?;
        if command.purpose != PlacementPurposeV2::OpenSession
            || command.client_request_id != self.client_request_id
            || command.client_sequence != 0
            || command.session_state_tier != self.state_tier
        {
            bail!("open request differs from its signed hosted command binding");
        }
        let expected_session_id = command.state_session.semantic_digest()?.to_string();
        if self.proposed_capability.session_id != expected_session_id {
            bail!("open capability session identity differs from the signed state session");
        }
        let expected_commitment = open_capability_commitment_v2(&self.proposed_capability)?;
        if self.capability_commitment != expected_commitment
            || command.open_capability_commitment.as_ref() != Some(&expected_commitment)
        {
            bail!("open capability commitment differs from the signed hosted command binding");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedOperationV2 {
    pub schema: String,
    pub operation_id: String,
    pub task_attempt: TaskAttemptIdV1,
    pub source_utf8: String,
    pub source_sha256: String,
    pub expected_backend_catalog_sha256: String,
    pub deadline_unix_ms: u64,
    pub output_limit_bytes: u64,
}

impl PreparedOperationV2 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation_id: impl Into<String>,
        task_attempt: TaskAttemptIdV1,
        source_utf8: impl Into<String>,
        expected_backend_catalog_sha256: impl Into<String>,
        deadline_unix_ms: u64,
        output_limit_bytes: u64,
    ) -> Result<Self> {
        let source_utf8 = source_utf8.into();
        let operation = Self {
            schema: HOSTED_OPERATION_SCHEMA_V2.to_owned(),
            operation_id: operation_id.into(),
            task_attempt,
            source_sha256: sha256_hex(source_utf8.as_bytes()),
            source_utf8,
            expected_backend_catalog_sha256: expected_backend_catalog_sha256.into(),
            deadline_unix_ms,
            output_limit_bytes,
        };
        operation.validate()?;
        Ok(operation)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != HOSTED_OPERATION_SCHEMA_V2 {
            bail!("unsupported hosted V2 operation schema `{}`", self.schema);
        }
        validate_identifier_v2("operation_id", &self.operation_id)?;
        if self.source_utf8.len() > MAX_HOSTED_SOURCE_BYTES {
            bail!(
                "prepared source length {} exceeds maximum {}",
                self.source_utf8.len(),
                MAX_HOSTED_SOURCE_BYTES
            );
        }
        validate_sha256_v2("source_sha256", &self.source_sha256)?;
        validate_sha256_v2(
            "expected_backend_catalog_sha256",
            &self.expected_backend_catalog_sha256,
        )?;
        if sha256_hex(self.source_utf8.as_bytes()) != self.source_sha256 {
            bail!("prepared V2 operation source digest mismatch");
        }
        if self.deadline_unix_ms == 0 {
            bail!("prepared V2 operation requires a non-zero deadline");
        }
        if self.output_limit_bytes == 0 || self.output_limit_bytes > MAX_HOSTED_OUTPUT_BYTES as u64
        {
            bail!(
                "output limit {} must be between 1 and {} bytes",
                self.output_limit_bytes,
                MAX_HOSTED_OUTPUT_BYTES
            );
        }
        Ok(())
    }

    pub fn sha256(&self) -> Result<String> {
        self.validate()?;
        canonical_hosted_sha256(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitOperationRequestV2 {
    pub credentials: SessionCredentialsV2,
    pub client_request_id: String,
    pub client_sequence: u64,
    pub operation: PreparedOperationV2,
    pub placement_lease: SignedPlacementLeaseV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMutationRequestV2 {
    pub credentials: SessionCredentialsV2,
    pub client_request_id: String,
    pub client_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverSessionRequestV2 {
    pub credentials: SessionCredentialsV2,
    pub client_request_id: String,
    pub client_sequence: u64,
    pub warrant: RecoveryWarrantV2,
    pub placement_lease: SignedPlacementLeaseV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionQueryV2 {
    pub credentials: SessionCredentialsV2,
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostedRequestV2 {
    OpenSession {
        protocol: String,
        request: OpenSessionRequestV2,
    },
    SubmitOperation {
        protocol: String,
        request: SubmitOperationRequestV2,
    },
    Status {
        protocol: String,
        query: SessionQueryV2,
    },
    Actors {
        protocol: String,
        query: SessionQueryV2,
    },
    ResetSession {
        protocol: String,
        request: SessionMutationRequestV2,
    },
    RecoverSession {
        protocol: String,
        request: RecoverSessionRequestV2,
    },
    CloseSession {
        protocol: String,
        request: SessionMutationRequestV2,
    },
}

impl HostedRequestV2 {
    pub fn validate(&self) -> Result<()> {
        let protocol = match self {
            Self::OpenSession { protocol, .. }
            | Self::SubmitOperation { protocol, .. }
            | Self::Status { protocol, .. }
            | Self::Actors { protocol, .. }
            | Self::ResetSession { protocol, .. }
            | Self::RecoverSession { protocol, .. }
            | Self::CloseSession { protocol, .. } => protocol,
        };
        if protocol != HOSTED_PROTOCOL_V2 {
            bail!("unsupported hosted protocol `{protocol}`");
        }
        match self {
            Self::OpenSession { request, .. } => {
                request.validate()?;
            }
            Self::SubmitOperation { request, .. } => {
                request.credentials.validate()?;
                validate_client_mutation_v2(request.client_sequence, &request.client_request_id)?;
                request.operation.validate()?;
            }
            Self::Status { query, .. } | Self::Actors { query, .. } => {
                query.credentials.validate()?;
                if let Some(operation_id) = &query.operation_id {
                    validate_identifier_v2("operation_id", operation_id)?;
                }
            }
            Self::ResetSession { request, .. } | Self::CloseSession { request, .. } => {
                request.credentials.validate()?;
                validate_client_mutation_v2(request.client_sequence, &request.client_request_id)?;
            }
            Self::RecoverSession { request, .. } => {
                request.credentials.validate()?;
                validate_client_mutation_v2(request.client_sequence, &request.client_request_id)?;
                request.warrant.validate()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatusV2 {
    Accepted,
    Running,
    Succeeded,
    Failed,
    NotStarted,
    Ambiguous,
}

// Preserve direct construction of the versioned V2 outcome schema. Boxing the
// successful OValue would impose heap indirection on every producer/consumer
// while changing this public wire model for an internal layout concern.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OperationOutcomeV2 {
    Succeeded {
        value: OValue,
    },
    Failed {
        stage: OperationFailureStageV2,
        code: String,
        message: String,
    },
}

impl OperationOutcomeV2 {
    pub fn failed(
        stage: OperationFailureStageV2,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let message = truncate_hosted_error_message(message.into());
        Self::Failed {
            stage,
            code: code.into(),
            message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationFailureStageV2 {
    Admission,
    Parse,
    Evaluate,
    Output,
    Deadline,
    Infrastructure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationViewV2 {
    pub operation_id: String,
    pub task_attempt: TaskAttemptIdV1,
    pub operation_sha256: String,
    pub status: OperationStatusV2,
    pub accepted_unix_ms: u64,
    pub started_unix_ms: Option<u64>,
    pub finished_unix_ms: Option<u64>,
    pub outcome: Option<OperationOutcomeV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorObservationV2 {
    pub actor_id: Option<String>,
    pub actor_generation: Option<ActorGenerationIdV1>,
    /// Generation the next locally prepared stateful fragment must establish
    /// when no physical actor currently exists (initial open or reset).
    pub next_actor_generation: GenerationV1,
    pub state_tier: SessionStateTierV2,
    pub retained: bool,
    pub health: ActorHealthV2,
    pub checkpoint_sha256: Option<String>,
    pub checkpoint_bytes: Option<u64>,
    pub observed_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorHealthV2 {
    Ready,
    Busy,
    RecoveryRequired,
    Quarantined,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionViewV2 {
    pub schema: String,
    pub session_id: String,
    pub node_id: String,
    pub principal_sha256: String,
    pub state_tier: SessionStateTierV2,
    pub status: SessionStatusV2,
    pub next_client_sequence: u64,
    pub actor: ActorObservationV2,
    pub operations: BTreeMap<String, OperationViewV2>,
    pub journal_head_sha256: String,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JournalEventV2 {
    SessionOpened {
        request_sha256: String,
        principal_sha256: String,
        bearer_salt: String,
        bearer_hash: String,
        capability_commitment: SemanticDigestV1,
        state_tier: SessionStateTierV2,
        state_session: StateSessionIdV2,
        state_quota_generation: GenerationV1,
        state_quota_limits: StateQuotaLimitsV2,
        state_reservation: StateReservationV2,
        placement_identity: HostedPlacementIdentityV2,
        placement_lease_sha256: String,
        placement_lease_nonce: String,
        client_request_id: String,
    },
    OperationAccepted {
        client_sequence: u64,
        client_request_id: String,
        request_sha256: String,
        operation_id: String,
        task_attempt: TaskAttemptIdV1,
        operation_sha256: String,
        source_sha256: String,
        actor_id: Option<String>,
        actor_generation: Option<ActorGenerationIdV1>,
        placement_lease_sha256: String,
        placement_lease_nonce: String,
    },
    OperationStarted {
        operation_id: String,
        operation_sha256: String,
        actor_generation: Option<ActorGenerationIdV1>,
        started_unix_ms: u64,
    },
    ActorCheckpointed {
        actor_generation: ActorGenerationIdV1,
        snapshot_sha256: String,
        snapshot_bytes: u64,
    },
    ActorCheckpointFailed {
        actor_generation: ActorGenerationIdV1,
        code: String,
        message: String,
    },
    OperationTerminal {
        operation_id: String,
        operation_sha256: String,
        finished_unix_ms: u64,
        outcome: OperationOutcomeV2,
        /// Whether the evaluator's state remains a sound continuation point
        /// after this operation. A false value requires explicit recovery or
        /// reset even though the operation itself has a terminal outcome.
        state_durable: bool,
        /// Whether execution reached a backend that may have created or
        /// mutated actor state. This prevents restart reconstruction from
        /// inventing state for admission failures that never reached a
        /// backend while preserving state already established by earlier
        /// operations.
        actor_state_touched: bool,
    },
    OperationInterrupted {
        operation_id: String,
        operation_sha256: String,
        classification: OperationStatusV2,
        reason: String,
    },
    ActorStateLost {
        previous_actor_generation: ActorGenerationIdV1,
        next_actor_generation: GenerationV1,
        reason: String,
    },
    ActorGenerationRetired {
        previous_actor_generation: ActorGenerationIdV1,
        next_actor_generation: GenerationV1,
        reason: String,
    },
    ActorRestored {
        previous_actor_generation: ActorGenerationIdV1,
        actor_generation: ActorGenerationIdV1,
        actor_id: String,
        snapshot_sha256: String,
        snapshot_bytes: u64,
    },
    SessionReset {
        client_sequence: u64,
        client_request_id: String,
        request_sha256: String,
        previous_actor_generation: Option<ActorGenerationIdV1>,
        next_actor_generation: GenerationV1,
    },
    /// Durable allocation of the unique physical actor generation used by one
    /// recovery handshake. This is not a client commit. If no terminal recovery
    /// record follows, startup deterministically refuses the exact request
    /// before exposing the session again.
    RecoveryAttemptStarted {
        client_sequence: u64,
        client_request_id: String,
        request_sha256: String,
        warrant_sha256: String,
        placement_lease_sha256: String,
        placement_lease_nonce: String,
        trigger: RecoveryTriggerV2,
        previous_actor_generation: ActorGenerationIdV1,
        attempted_actor_generation: ActorGenerationIdV1,
        checkpoint_sha256: String,
        checkpoint_bytes: u64,
    },
    RecoveryCommitted {
        client_sequence: u64,
        client_request_id: String,
        request_sha256: String,
        warrant_sha256: String,
        placement_lease_sha256: String,
        placement_lease_nonce: String,
        recovery_attempt_sha256: String,
        trigger: RecoveryTriggerV2,
        previous_actor_generation: ActorGenerationIdV1,
        actor_generation: ActorGenerationIdV1,
        actor_id: String,
        checkpoint_sha256: Option<String>,
        checkpoint_bytes: Option<u64>,
    },
    RecoveryRefused {
        client_sequence: u64,
        client_request_id: String,
        request_sha256: String,
        warrant_sha256: String,
        placement_lease_sha256: String,
        placement_lease_nonce: String,
        recovery_attempt_sha256: Option<String>,
        attempted_actor_generation: Option<ActorGenerationIdV1>,
        code: String,
        message: String,
    },
    PlacementLeaseRefused {
        state_session_sha256: String,
        placement_lease_sha256: String,
        placement_lease_nonce: String,
        hosted_command_sha256: String,
        code: String,
        message: String,
    },
    ClosedSessionGcAuthorized {
        session_id: String,
        terminal_journal_head_sha256: String,
        expected_reclaimed_bytes: u64,
        retained_journal_sha256: String,
        retained_journal_bytes: u64,
    },
    ClosedSessionGcCompleted {
        session_id: String,
        terminal_journal_head_sha256: String,
        reclaimed_bytes: u64,
    },
    /// Signed authority-journal evidence that startup removed only an
    /// incomplete final frame and retained the exact validated hash-chain
    /// prefix. Complete invalid frames are never represented as repairs.
    JournalTailRepaired {
        journal_id: String,
        old_bytes: u64,
        new_bytes: u64,
        recovered_head_sha256: Option<String>,
    },
    SessionClosed {
        client_sequence: u64,
        client_request_id: String,
        request_sha256: String,
        actor_generation: Option<ActorGenerationIdV1>,
    },
}

impl JournalEventV2 {
    pub fn retired_session_id(&self) -> Option<&str> {
        match self {
            Self::ClosedSessionGcAuthorized { session_id, .. } => Some(session_id),
            _ => None,
        }
    }

    pub fn closed_session_gc_archive(&self) -> Option<(&str, &str, u64)> {
        match self {
            Self::ClosedSessionGcAuthorized {
                session_id,
                retained_journal_sha256,
                retained_journal_bytes,
                ..
            } => Some((session_id, retained_journal_sha256, *retained_journal_bytes)),
            _ => None,
        }
    }

    pub fn placement_lease_sha256(&self) -> Option<&str> {
        match self {
            Self::SessionOpened {
                placement_lease_sha256,
                ..
            }
            | Self::OperationAccepted {
                placement_lease_sha256,
                ..
            }
            | Self::RecoveryAttemptStarted {
                placement_lease_sha256,
                ..
            }
            | Self::RecoveryCommitted {
                placement_lease_sha256,
                ..
            }
            | Self::RecoveryRefused {
                placement_lease_sha256,
                ..
            }
            | Self::PlacementLeaseRefused {
                placement_lease_sha256,
                ..
            } => Some(placement_lease_sha256),
            _ => None,
        }
    }

    pub fn placement_lease_nonce(&self) -> Option<&str> {
        match self {
            Self::SessionOpened {
                placement_lease_nonce,
                ..
            }
            | Self::OperationAccepted {
                placement_lease_nonce,
                ..
            }
            | Self::RecoveryAttemptStarted {
                placement_lease_nonce,
                ..
            }
            | Self::RecoveryCommitted {
                placement_lease_nonce,
                ..
            }
            | Self::RecoveryRefused {
                placement_lease_nonce,
                ..
            }
            | Self::PlacementLeaseRefused {
                placement_lease_nonce,
                ..
            } => Some(placement_lease_nonce),
            _ => None,
        }
    }

    pub fn client_commit(&self) -> Option<(u64, &str, &str)> {
        match self {
            Self::OperationAccepted {
                client_sequence,
                client_request_id,
                request_sha256,
                ..
            }
            | Self::SessionReset {
                client_sequence,
                client_request_id,
                request_sha256,
                ..
            }
            | Self::RecoveryCommitted {
                client_sequence,
                client_request_id,
                request_sha256,
                ..
            }
            | Self::RecoveryRefused {
                client_sequence,
                client_request_id,
                request_sha256,
                ..
            }
            | Self::SessionClosed {
                client_sequence,
                client_request_id,
                request_sha256,
                ..
            } => Some((*client_sequence, client_request_id, request_sha256)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntryV2 {
    pub schema: String,
    pub session_id: String,
    pub sequence: u64,
    pub previous_entry_sha256: Option<String>,
    pub recorded_unix_ms: u64,
    pub event: JournalEventV2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedJournalEntryV2 {
    pub schema: String,
    pub entry: JournalEntryV2,
    pub signer_public_key: String,
    pub signer_key_id: String,
    pub entry_sha256: String,
    pub signature: String,
}

// This public V2 wire enum intentionally keeps receipts and errors directly
// constructible; transport internals must not force a boxed schema revision.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum HostedResponseV2 {
    SessionOpened {
        capability: SessionCapabilityV2,
        receipt: SignedJournalEntryV2,
    },
    Committed {
        receipt: SignedJournalEntryV2,
    },
    Status {
        /// A mutually authenticated convenience view. The receipt attests the
        /// exact session journal head, not every projected field in this view.
        session: SessionViewV2,
        head_receipt: SignedJournalEntryV2,
    },
    Actors {
        /// A mutually authenticated convenience view. The receipt attests the
        /// exact session journal head, not every projected actor field.
        session_id: String,
        actors: Vec<ActorObservationV2>,
        journal_head_sha256: String,
        head_receipt: SignedJournalEntryV2,
    },
    Error {
        error: HostedProtocolErrorV2,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedProtocolErrorV2 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl HostedProtocolErrorV2 {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        let message = truncate_hosted_error_message(message.into());
        Self {
            code: code.into(),
            message,
            retryable,
        }
    }
}

pub(super) fn validate_identifier_v2(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_HOSTED_ID_BYTES {
        bail!("{field} length must be between 1 and {MAX_HOSTED_ID_BYTES} bytes");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{field} contains characters outside [A-Za-z0-9._:-]");
    }
    Ok(())
}

pub(super) fn validate_sha256_v2(field: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{field} must be a lowercase 64-character SHA-256 digest");
    }
    Ok(())
}

fn validate_capability_v2(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("session bearer must be 32 bytes encoded as lowercase hexadecimal");
    }
    Ok(())
}

fn validate_client_mutation_v2(sequence: u64, request_id: &str) -> Result<()> {
    if sequence == 0 || sequence == u64::MAX {
        bail!("session mutation sequence must be between one and u64::MAX - 1");
    }
    validate_identifier_v2("client_request_id", request_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosted_remote::MAX_HOSTED_ERROR_BYTES;

    #[test]
    fn client_mutation_sequence_reserves_zero_and_u64_max() {
        assert!(validate_client_mutation_v2(0, "request").is_err());
        assert!(validate_client_mutation_v2(u64::MAX, "request").is_err());
        assert!(validate_client_mutation_v2(1, "request").is_ok());
        assert!(validate_client_mutation_v2(u64::MAX - 1, "request").is_ok());
    }

    #[test]
    fn v2_error_constructors_truncate_only_at_utf8_boundaries() {
        let message = format!(
            "{}{}",
            "a".repeat(MAX_HOSTED_ERROR_BYTES - 1),
            "é".repeat(8)
        );
        let protocol = HostedProtocolErrorV2::new("invalid-frame", message.clone(), false);
        let outcome = OperationOutcomeV2::failed(
            OperationFailureStageV2::Admission,
            "invalid-frame",
            message,
        );

        assert!(protocol.message.ends_with(" [truncated]"));
        let OperationOutcomeV2::Failed { message, .. } = outcome else {
            unreachable!("failed constructor returned a successful outcome")
        };
        assert_eq!(message, protocol.message);
        assert_eq!(
            message.trim_end_matches(" [truncated]").len(),
            MAX_HOSTED_ERROR_BYTES - 1
        );
    }
}
