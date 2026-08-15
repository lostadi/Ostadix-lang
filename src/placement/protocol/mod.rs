//! Canonical, transport-independent placement protocol.
//!
//! This module owns only deterministic records, structural validation, and
//! injected authority interfaces. Compiler projection, the compiled backend
//! catalog, clocks, signatures, storage, and network transports live outside
//! this boundary.

mod candidate;
mod catalog;
mod digest;
mod error;
mod records;
mod requirement;
mod state;
mod target;
mod warrant;

pub use candidate::{
    CandidateDecisionV1, CandidateRejectionV1, CandidateSetV1, PlacementCandidateInputV1,
    PlacementEligibilityV1,
};
pub use catalog::CurrentBackendCatalogV1;
pub use digest::{CanonicalPlacementRecordV1, GenerationV1, SemanticDigestV1, UnixMillisV1};
pub use error::PlacementValidationError;
pub use records::{
    CapacityObservationV1, LeaseExpectationV1, LeaseExpectationV2, LeaseStateBindingV2,
    NodeProfileV1, PlacementLeaseV1, PlacementLeaseV2, PlacementReservationV1,
    RecordAuthenticationV1, RecordAuthenticatorV1, StateControlExpectationV2, StateControlLeaseV2,
    TaskAttemptIdV1,
};
pub use requirement::{
    CapabilityAtomV1, CapabilityKeyV1, EffectRequirementV1, EndiannessV1, EnvironmentRequirementV1,
    RequirementAtomV1, RequirementFootprintV1, ResourceKindV1,
};
pub use state::{
    BackendStateSupportV2, ExternalPinnedStateManifestV2, SnapshotCompatibilityV2,
    StateCapacityObservationV2, StateCapacityRefusalV2, StateCheckpointPayloadV2,
    StateCheckpointV2, StateQuotaDimensionV2, StateQuotaLimitsV2, StateReservationV2,
    StateSessionIdV2,
};
pub use target::{
    ActorGenerationIdV1, ArtifactCacheKeyV1, BackendImplementationIdV1, PlatformDescriptorV1,
    TargetCapabilityModelV1, TargetDescriptorV1,
};
pub use warrant::{
    DischargedRequirementV1, PlacementTrustPolicyV1, PlacementWarrantV1, WarrantAssertionV1,
    WarrantDischargeV1, WarrantScopeV1, WarrantTierV1,
};
