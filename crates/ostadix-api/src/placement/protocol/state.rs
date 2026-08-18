use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::resource_identity::ArtifactId;

use super::digest::{validate_fresh, validate_token, validate_window};
use super::records::require_authenticated;
use super::{
    CanonicalPlacementRecordV1, GenerationV1, PlacementValidationError, RecordAuthenticatorV1,
    SemanticDigestV1, UnixMillisV1,
};

pub const MAX_STATE_CAPACITY_OBSERVATION_LIFETIME_MS: u64 = 5_000;

/// Compatibility scope for one semantic checkpoint codec.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    rename_all = "kebab-case",
    tag = "kind",
    content = "identity",
    deny_unknown_fields
)]
pub enum SnapshotCompatibilityV2 {
    ExactImplementation,
    CompatibilityClass(SemanticDigestV1),
}

/// State behavior promised by one exact catalogued backend specification.
///
/// There is deliberately no unknown/default variant. An implementation absent
/// from the current catalog cannot authorize state operations.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum BackendStateSupportV2 {
    /// The canonical checkpoint is empty and realization retains no semantic
    /// state between operations.
    Stateless,
    /// The implementation must provide bounded checkpoint and restore through
    /// this exact codec. A failed transition poisons the session and publishes
    /// no durability claim.
    SemanticSnapshot {
        codec: SemanticDigestV1,
        compatibility: SnapshotCompatibilityV2,
    },
    /// State is represented by a signed node-generation/resource manifest.
    /// This tier never authorizes cross-node migration.
    ExternalPinned { manifest_schema: SemanticDigestV1 },
}

/// Hard state-session quota configuration. Zero is a valid administrative
/// capacity of zero; the default policy is supplied by the node CLI, not by
/// this protocol record.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateQuotaLimitsV2 {
    max_open_sessions: u32,
    max_actors_per_session: u32,
    max_snapshot_bytes_per_actor: u64,
    max_state_bytes_per_session: u64,
    max_state_bytes_total: u64,
}

impl StateQuotaLimitsV2 {
    pub fn new(
        max_open_sessions: u32,
        max_actors_per_session: u32,
        max_snapshot_bytes_per_actor: u64,
        max_state_bytes_per_session: u64,
        max_state_bytes_total: u64,
    ) -> Result<Self, PlacementValidationError> {
        if max_snapshot_bytes_per_actor > max_state_bytes_per_session {
            return Err(PlacementValidationError::InvalidStateQuota(
                "max_snapshot_bytes_per_actor exceeds max_state_bytes_per_session".to_owned(),
            ));
        }
        if max_state_bytes_per_session > max_state_bytes_total {
            return Err(PlacementValidationError::InvalidStateQuota(
                "max_state_bytes_per_session exceeds max_state_bytes_total".to_owned(),
            ));
        }
        Ok(Self {
            max_open_sessions,
            max_actors_per_session,
            max_snapshot_bytes_per_actor,
            max_state_bytes_per_session,
            max_state_bytes_total,
        })
    }

    pub fn max_open_sessions(&self) -> u32 {
        self.max_open_sessions
    }

    pub fn max_actors_per_session(&self) -> u32 {
        self.max_actors_per_session
    }

    pub fn max_snapshot_bytes_per_actor(&self) -> u64 {
        self.max_snapshot_bytes_per_actor
    }

    pub fn max_state_bytes_per_session(&self) -> u64 {
        self.max_state_bytes_per_session
    }

    pub fn max_state_bytes_total(&self) -> u64 {
        self.max_state_bytes_total
    }

    pub fn permits(&self, reservation: &StateReservationV2) -> bool {
        reservation.actor_count <= self.max_actors_per_session
            && reservation.snapshot_bytes_per_actor <= self.max_snapshot_bytes_per_actor
            && reservation.state_bytes <= self.max_state_bytes_per_session
            && reservation.state_bytes <= self.max_state_bytes_total
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateQuotaLimitsWireV2 {
    max_open_sessions: u32,
    max_actors_per_session: u32,
    max_snapshot_bytes_per_actor: u64,
    max_state_bytes_per_session: u64,
    max_state_bytes_total: u64,
}

impl<'de> Deserialize<'de> for StateQuotaLimitsV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateQuotaLimitsWireV2::deserialize(deserializer)?;
        Self::new(
            wire.max_open_sessions,
            wire.max_actors_per_session,
            wire.max_snapshot_bytes_per_actor,
            wire.max_state_bytes_per_session,
            wire.max_state_bytes_total,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CanonicalPlacementRecordV1 for StateQuotaLimitsV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-quota-limits/v2";
}

/// Capacity reserved before any stateful actor is opened. Reservation, rather
/// than observed post-hoc byte use, makes the configured byte ceilings hard.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateReservationV2 {
    actor_count: u32,
    snapshot_bytes_per_actor: u64,
    state_bytes: u64,
}

impl StateReservationV2 {
    pub fn new(
        actor_count: u32,
        snapshot_bytes_per_actor: u64,
        state_bytes: u64,
    ) -> Result<Self, PlacementValidationError> {
        if actor_count == 0 {
            return Err(PlacementValidationError::Zero {
                field: "reserved state actors",
            });
        }
        let minimum_snapshot_reservation = u64::from(actor_count)
            .checked_mul(snapshot_bytes_per_actor)
            .ok_or_else(|| {
                PlacementValidationError::InvalidStateQuota(
                    "actor checkpoint reservation overflows u64".to_owned(),
                )
            })?;
        if state_bytes < minimum_snapshot_reservation {
            return Err(PlacementValidationError::InvalidStateQuota(
                "state_bytes is smaller than actor_count * snapshot_bytes_per_actor".to_owned(),
            ));
        }
        Ok(Self {
            actor_count,
            snapshot_bytes_per_actor,
            state_bytes,
        })
    }

    pub fn actor_count(&self) -> u32 {
        self.actor_count
    }

    pub fn snapshot_bytes_per_actor(&self) -> u64 {
        self.snapshot_bytes_per_actor
    }

    pub fn state_bytes(&self) -> u64 {
        self.state_bytes
    }

    pub fn validate_against(
        &self,
        limits: &StateQuotaLimitsV2,
    ) -> Result<(), PlacementValidationError> {
        if self.actor_count > limits.max_actors_per_session {
            return Err(PlacementValidationError::StateQuotaExceeded {
                dimension: StateQuotaDimensionV2::ActorsPerSession.label().to_owned(),
                requested: u64::from(self.actor_count),
                limit: u64::from(limits.max_actors_per_session),
            });
        }
        if self.snapshot_bytes_per_actor > limits.max_snapshot_bytes_per_actor {
            return Err(PlacementValidationError::StateQuotaExceeded {
                dimension: StateQuotaDimensionV2::SnapshotBytesPerActor
                    .label()
                    .to_owned(),
                requested: self.snapshot_bytes_per_actor,
                limit: limits.max_snapshot_bytes_per_actor,
            });
        }
        if self.state_bytes > limits.max_state_bytes_per_session {
            return Err(PlacementValidationError::StateQuotaExceeded {
                dimension: StateQuotaDimensionV2::StateBytesPerSession
                    .label()
                    .to_owned(),
                requested: self.state_bytes,
                limit: limits.max_state_bytes_per_session,
            });
        }
        if self.state_bytes > limits.max_state_bytes_total {
            return Err(PlacementValidationError::StateQuotaExceeded {
                dimension: StateQuotaDimensionV2::StateBytesTotal.label().to_owned(),
                requested: self.state_bytes,
                limit: limits.max_state_bytes_total,
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateReservationWireV2 {
    actor_count: u32,
    snapshot_bytes_per_actor: u64,
    state_bytes: u64,
}

impl<'de> Deserialize<'de> for StateReservationV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateReservationWireV2::deserialize(deserializer)?;
        Self::new(
            wire.actor_count,
            wire.snapshot_bytes_per_actor,
            wire.state_bytes,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CanonicalPlacementRecordV1 for StateReservationV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-reservation/v2";
}

/// Stable identity for one accepted state session on one node generation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateSessionIdV2 {
    node_id: String,
    node_generation: GenerationV1,
    session_nonce: SemanticDigestV1,
}

impl StateSessionIdV2 {
    pub fn new(
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        session_nonce: SemanticDigestV1,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("state session node identity", &node_id)?;
        Ok(Self {
            node_id,
            node_generation,
            session_nonce,
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn session_nonce(&self) -> &SemanticDigestV1 {
        &self.session_nonce
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateSessionIdWireV2 {
    node_id: String,
    node_generation: GenerationV1,
    session_nonce: SemanticDigestV1,
}

impl<'de> Deserialize<'de> for StateSessionIdV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateSessionIdWireV2::deserialize(deserializer)?;
        Self::new(wire.node_id, wire.node_generation, wire.session_nonce)
            .map_err(serde::de::Error::custom)
    }
}

impl CanonicalPlacementRecordV1 for StateSessionIdV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-session/v2";
}

/// Exact payload represented by a checkpoint record. The semantic payload is
/// content-addressed rather than embedded, keeping placement records bounded.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum StateCheckpointPayloadV2 {
    Stateless,
    SemanticSnapshot {
        codec: SemanticDigestV1,
        compatibility: SnapshotCompatibilityV2,
        artifact: ArtifactId,
        byte_len: u64,
    },
    ExternalPinned {
        manifest: SemanticDigestV1,
    },
}

/// Captured state material. This record alone is not a durability receipt.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateCheckpointV2 {
    session: StateSessionIdV2,
    actor_generation: SemanticDigestV1,
    backend_implementation: SemanticDigestV1,
    checkpoint_generation: GenerationV1,
    payload: StateCheckpointPayloadV2,
    captured_at: UnixMillisV1,
}

impl StateCheckpointV2 {
    pub fn new(
        session: StateSessionIdV2,
        actor_generation: SemanticDigestV1,
        backend_implementation: SemanticDigestV1,
        checkpoint_generation: GenerationV1,
        payload: StateCheckpointPayloadV2,
        captured_at: UnixMillisV1,
    ) -> Self {
        Self {
            session,
            actor_generation,
            backend_implementation,
            checkpoint_generation,
            payload,
            captured_at,
        }
    }

    pub fn session(&self) -> &StateSessionIdV2 {
        &self.session
    }

    pub fn payload(&self) -> &StateCheckpointPayloadV2 {
        &self.payload
    }
}

impl CanonicalPlacementRecordV1 for StateCheckpointV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-checkpoint/v2";
}

/// Signed-payload material for state that remains attached to one exact node
/// generation and its governed resource identities.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalPinnedStateManifestV2 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    node_generation: GenerationV1,
    backend_implementation: SemanticDigestV1,
    actor_generation: SemanticDigestV1,
    manifest_schema: SemanticDigestV1,
    resource_identities: BTreeSet<SemanticDigestV1>,
    accounted_state_bytes: u64,
    issued_at: UnixMillisV1,
}

impl ExternalPinnedStateManifestV2 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer_key: SemanticDigestV1,
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        backend_implementation: SemanticDigestV1,
        actor_generation: SemanticDigestV1,
        manifest_schema: SemanticDigestV1,
        resource_identities: impl IntoIterator<Item = SemanticDigestV1>,
        accounted_state_bytes: u64,
        issued_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("pinned state node identity", &node_id)?;
        let resource_identities = resource_identities.into_iter().collect::<BTreeSet<_>>();
        if resource_identities.is_empty() {
            return Err(PlacementValidationError::EmptyPinnedStateResources);
        }
        Ok(Self {
            issuer_key,
            node_id,
            node_generation,
            backend_implementation,
            actor_generation,
            manifest_schema,
            resource_identities,
            accounted_state_bytes,
            issued_at,
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn resource_identities(&self) -> &BTreeSet<SemanticDigestV1> {
        &self.resource_identities
    }

    pub fn validate_authentication(
        &self,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        require_authenticated(
            "external-pinned state manifest",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalPinnedStateManifestWireV2 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    node_generation: GenerationV1,
    backend_implementation: SemanticDigestV1,
    actor_generation: SemanticDigestV1,
    manifest_schema: SemanticDigestV1,
    resource_identities: BTreeSet<SemanticDigestV1>,
    accounted_state_bytes: u64,
    issued_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for ExternalPinnedStateManifestV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ExternalPinnedStateManifestWireV2::deserialize(deserializer)?;
        Self::new(
            wire.issuer_key,
            wire.node_id,
            wire.node_generation,
            wire.backend_implementation,
            wire.actor_generation,
            wire.manifest_schema,
            wire.resource_identities,
            wire.accounted_state_bytes,
            wire.issued_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CanonicalPlacementRecordV1 for ExternalPinnedStateManifestV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/external-pinned-state-manifest/v2";
}

/// Short-lived signed observation of capacity already reserved by accepted
/// sessions. It is distinct from semantic backend support.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateCapacityObservationV2 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    node_generation: GenerationV1,
    capacity_generation: GenerationV1,
    limits: StateQuotaLimitsV2,
    open_sessions: u32,
    state_bytes_reserved: u64,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl StateCapacityObservationV2 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer_key: SemanticDigestV1,
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        capacity_generation: GenerationV1,
        limits: StateQuotaLimitsV2,
        open_sessions: u32,
        state_bytes_reserved: u64,
        issued_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("state capacity node identity", &node_id)?;
        validate_window(
            "state capacity observation",
            issued_at,
            expires_at,
            MAX_STATE_CAPACITY_OBSERVATION_LIFETIME_MS,
        )?;
        if open_sessions > limits.max_open_sessions {
            return Err(PlacementValidationError::InvalidStateQuota(
                "open session count exceeds configured maximum".to_owned(),
            ));
        }
        if state_bytes_reserved > limits.max_state_bytes_total {
            return Err(PlacementValidationError::InvalidStateQuota(
                "reserved state bytes exceed configured total".to_owned(),
            ));
        }
        Ok(Self {
            issuer_key,
            node_id,
            node_generation,
            capacity_generation,
            limits,
            open_sessions,
            state_bytes_reserved,
            issued_at,
            expires_at,
        })
    }

    pub fn limits(&self) -> &StateQuotaLimitsV2 {
        &self.limits
    }

    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        &self.issuer_key
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn capacity_generation(&self) -> GenerationV1 {
        self.capacity_generation
    }

    pub fn open_sessions(&self) -> u32 {
        self.open_sessions
    }

    pub fn state_bytes_reserved(&self) -> u64 {
        self.state_bytes_reserved
    }

    pub fn issued_at(&self) -> UnixMillisV1 {
        self.issued_at
    }

    pub fn expires_at(&self) -> UnixMillisV1 {
        self.expires_at
    }

    pub fn available_sessions(&self) -> u32 {
        self.limits.max_open_sessions - self.open_sessions
    }

    pub fn available_state_bytes(&self) -> u64 {
        self.limits.max_state_bytes_total - self.state_bytes_reserved
    }

    pub fn can_admit(&self, reservation: &StateReservationV2) -> bool {
        self.available_sessions() > 0
            && reservation.validate_against(&self.limits).is_ok()
            && reservation.state_bytes <= self.available_state_bytes()
    }

    pub fn validate_at(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        validate_fresh(
            "state capacity observation",
            self.issued_at,
            self.expires_at,
            now,
        )?;
        require_authenticated(
            "state capacity observation",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateCapacityObservationWireV2 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    node_generation: GenerationV1,
    capacity_generation: GenerationV1,
    limits: StateQuotaLimitsV2,
    open_sessions: u32,
    state_bytes_reserved: u64,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for StateCapacityObservationV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateCapacityObservationWireV2::deserialize(deserializer)?;
        Self::new(
            wire.issuer_key,
            wire.node_id,
            wire.node_generation,
            wire.capacity_generation,
            wire.limits,
            wire.open_sessions,
            wire.state_bytes_reserved,
            wire.issued_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CanonicalPlacementRecordV1 for StateCapacityObservationV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-capacity-observation/v2";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateQuotaDimensionV2 {
    OpenSessions,
    ActorsPerSession,
    SnapshotBytesPerActor,
    StateBytesPerSession,
    StateBytesTotal,
}

impl StateQuotaDimensionV2 {
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenSessions => "max-open-sessions",
            Self::ActorsPerSession => "max-actors-per-session",
            Self::SnapshotBytesPerActor => "max-snapshot-bytes-per-actor",
            Self::StateBytesPerSession => "max-state-bytes-per-session",
            Self::StateBytesTotal => "max-state-bytes-total",
        }
    }
}

/// Authenticated, short-lived evidence that one exact admission request did
/// not fit mutable state capacity. It is not a semantic target rejection.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateCapacityRefusalV2 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    node_generation: GenerationV1,
    capacity_generation: GenerationV1,
    request: SemanticDigestV1,
    dimension: StateQuotaDimensionV2,
    requested: u64,
    in_use: u64,
    limit: u64,
    observed_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl StateCapacityRefusalV2 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer_key: SemanticDigestV1,
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        capacity_generation: GenerationV1,
        request: SemanticDigestV1,
        dimension: StateQuotaDimensionV2,
        requested: u64,
        in_use: u64,
        limit: u64,
        observed_at: UnixMillisV1,
        expires_at: UnixMillisV1,
    ) -> Result<Self, PlacementValidationError> {
        let node_id = node_id.into();
        validate_token("state capacity refusal node identity", &node_id)?;
        validate_window(
            "state capacity refusal",
            observed_at,
            expires_at,
            MAX_STATE_CAPACITY_OBSERVATION_LIFETIME_MS,
        )?;
        if requested == 0 || in_use.saturating_add(requested) <= limit {
            return Err(PlacementValidationError::InvalidStateQuota(
                "capacity refusal does not exceed the stated limit".to_owned(),
            ));
        }
        Ok(Self {
            issuer_key,
            node_id,
            node_generation,
            capacity_generation,
            request,
            dimension,
            requested,
            in_use,
            limit,
            observed_at,
            expires_at,
        })
    }

    pub fn dimension(&self) -> StateQuotaDimensionV2 {
        self.dimension
    }

    pub fn validate_at(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        validate_fresh(
            "state capacity refusal",
            self.observed_at,
            self.expires_at,
            now,
        )?;
        require_authenticated(
            "state capacity refusal",
            &self.issuer_key,
            self.semantic_digest()?,
            authenticator,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateCapacityRefusalWireV2 {
    issuer_key: SemanticDigestV1,
    node_id: String,
    node_generation: GenerationV1,
    capacity_generation: GenerationV1,
    request: SemanticDigestV1,
    dimension: StateQuotaDimensionV2,
    requested: u64,
    in_use: u64,
    limit: u64,
    observed_at: UnixMillisV1,
    expires_at: UnixMillisV1,
}

impl<'de> Deserialize<'de> for StateCapacityRefusalV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateCapacityRefusalWireV2::deserialize(deserializer)?;
        Self::new(
            wire.issuer_key,
            wire.node_id,
            wire.node_generation,
            wire.capacity_generation,
            wire.request,
            wire.dimension,
            wire.requested,
            wire.in_use,
            wire.limit,
            wire.observed_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CanonicalPlacementRecordV1 for StateCapacityRefusalV2 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/state-capacity-refusal/v2";
}
