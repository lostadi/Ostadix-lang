//! Shared hosted vocabulary for the Ostadix World integration layer.
//!
//! PR1 deliberately provides identity, stale-reference comparison helpers,
//! and planner inspection only. Governor membership, authoritative snapshots,
//! leases, resource publication, distributed dispatch, and placement are later
//! protocol layers.

pub mod grounding;
pub mod identity;

pub use grounding::{
    CapabilityGrounding, CapsuleGrounding, GroundingError, GroundingReport, OValueFlowGrounding,
    OValueFlowRelation, OperationGrounding,
};
pub use identity::{
    ArtifactId, ArtifactPublicationIdentity, AttemptGeneration, DomainGeneration, DomainId,
    DomainIdentity, KernelWorldBinding, NodeGeneration, NodeId, NodeIdentity, ResourceId,
    ResourceIdentity, ResourceOwner, TaskAttemptIdentity, TaskId, WorldEpoch, WorldId,
    WorldIdentity, WorldIdentityError,
};
