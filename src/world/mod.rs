//! Shared hosted vocabulary for the Ostadix World integration layer.
//!
//! This layer provides identity, stale-reference comparison helpers, planner
//! inspection, and a bounded identity-only hosted/native wire ABI. Governor
//! membership, authoritative snapshots, lease enforcement, resource
//! publication, distributed dispatch, and placement remain later protocol
//! layers.

pub mod grounding;
pub mod identity;
pub mod identity_wire;

pub use grounding::{
    CapabilityGrounding, CapsuleGrounding, GroundingError, GroundingReport, OValueFlowGrounding,
    OValueFlowRelation, OperationGrounding,
};
pub use identity::{
    ArtifactId, ArtifactPublicationIdentity, AttemptGeneration, AttemptIdentity, CapabilityId,
    CapabilityIdentity, CheckpointId, CheckpointIdentity, DomainGeneration, DomainId,
    DomainIdentity, GovernorIdentity, GovernorLogIndex, GovernorTerm, KernelWorldBinding, LeaseId,
    LeaseIdentity, NodeGeneration, NodeId, NodeIdentity, ObjectId, ObjectIdentity, ObjectVersion,
    ProcessGeneration, ProcessId, ProcessIdentity, ReceiptId, ReceiptIdentity, ResourceGeneration,
    ResourceId, ResourceIdentity, ResourceOwner, TaskAttemptIdentity, TaskId, TaskIdentity,
    WorldEpoch, WorldId, WorldIdentity, WorldIdentityError,
};
pub use identity_wire::{
    identity_v1_conformance_bytes, identity_v1_conformance_records, IdentityWireError,
    IdentityWireKind, IdentityWireRecord, IDENTITY_WIRE_HEADER_BYTES, IDENTITY_WIRE_MAGIC,
    IDENTITY_WIRE_VERSION, MAX_IDENTITY_WIRE_RECORD_BYTES,
};
