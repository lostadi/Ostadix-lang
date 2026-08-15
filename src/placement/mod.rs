//! Descriptor-based placement authority for hosted Ostadix execution.
//!
//! This module is intentionally transport-independent.  Its records describe
//! requirements, targets, warrants, observations, and leases, while callers
//! remain responsible for authenticating the detached record envelopes.  A
//! record is never authority merely because it deserialized successfully.

mod candidate;
mod digest;
mod error;
mod projection;
mod records;
mod requirement;
mod target;
mod warrant;

pub use candidate::{
    CandidateDecisionV1, CandidateRejectionV1, CandidateSetV1, PlacementCandidateInputV1,
    PlacementEligibilityV1,
};
pub use digest::{CanonicalPlacementRecordV1, GenerationV1, SemanticDigestV1, UnixMillisV1};
pub use error::PlacementValidationError;
pub use projection::{
    requirement_footprint_for_island, requirement_footprint_for_plan_node,
    requirement_footprint_for_program_node, PlacementIntentV1,
};
pub use records::{
    CapacityObservationV1, LeaseExpectationV1, NodeProfileV1, PlacementLeaseV1,
    PlacementReservationV1, RecordAuthenticationV1, RecordAuthenticatorV1, TaskAttemptIdV1,
};
pub use requirement::{
    CapabilityAtomV1, CapabilityKeyV1, EffectRequirementV1, EndiannessV1, EnvironmentRequirementV1,
    RequirementAtomV1, RequirementFootprintV1, ResourceKindV1,
};
pub use target::{
    ActorGenerationIdV1, ArtifactCacheKeyV1, BackendImplementationIdV1, PlatformDescriptorV1,
    TargetCapabilityModelV1, TargetDescriptorV1,
};
pub use warrant::{
    DischargedRequirementV1, PlacementTrustPolicyV1, PlacementWarrantV1, WarrantAssertionV1,
    WarrantDischargeV1, WarrantScopeV1, WarrantTierV1,
};
