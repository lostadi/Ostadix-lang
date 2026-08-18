//! Shared hosted vocabulary for the Ostadix World integration layer.
//!
//! This layer provides identity, stale-reference comparison helpers, planner
//! inspection, a bounded identity ABI, and a strict canonical World record
//! codec with deterministic offline schema negotiation. Network transport,
//! authenticated sessions, Governor membership, authoritative snapshots, lease
//! enforcement, resource publication, distributed dispatch, and placement
//! remain later layers.

pub mod codec;
pub mod grounding;
pub use crate::resource_identity as identity;
pub mod identity_wire;
pub mod protocol;
pub mod receipt;
pub mod receipt_codec;
pub mod value;
pub mod value_codec;

pub use codec::{
    world_protocol_v1_conformance_bytes, world_protocol_v1_conformance_records, WorldCodecError,
    WorldWireKind, WorldWireRecord, WORLD_WIRE_CODEC_VERSION, WORLD_WIRE_HEADER_BYTES,
    WORLD_WIRE_MAGIC,
};

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
pub use protocol::{
    negotiate_schema, validate_rejection, validate_selection, NegotiatedSchema, SchemaNegotiation,
    SchemaOffer, SchemaRejection, SchemaRejectionReason, SchemaSelection, WorldProtocolError,
    MAX_WORLD_WIRE_RECORD_BYTES, WORLD_SCHEMA_V1, WORLD_WIRE_MIN_RECORD_BYTES,
};
pub use receipt::{
    CapabilityObservationV1, CapsuleObservationV1, CheckpointObservationV1, ComponentKindV1,
    ComponentObservationV1, EffectObservationV1, EvidenceObservationV1, ExecutionReceiptV1,
    ObjectObservationV1, ObjectRoleV1, PlacementRejectionV1, ReceiptCommitFenceV1,
    ReceiptContextV1, ReceiptCurrentStateV1, ReceiptError, ReceiptPlacementV1, ReceiptRight,
    ReceiptSubjectV1, ReceiptTerminalV1,
};
pub use receipt_codec::{
    decode_signed_receipt_v1, encode_signed_receipt_v1, inspect_signed_receipt_v1,
    project_receipt_semantic_sha256_v1, receipt_signing_preimage_v1, receipt_v1_sha256,
    verify_signed_receipt_v1, Ed25519ReceiptSigner, ReceiptKeyResolver, SignedExecutionReceiptV1,
    VerifiedExecutionReceiptV1, ED25519_SIGNATURE_ALGORITHM_V1, MAX_WORLD_RECEIPT_BYTES,
    PROJECT_RECEIPT_SEMANTIC_DOMAIN_V1, WORLD_RECEIPT_HEADER_BYTES, WORLD_RECEIPT_MAGIC,
    WORLD_RECEIPT_SCHEMA_V1,
};
pub use value::{
    AdmittedExtension, ExtensionEnvelope, HostedValueError, PortableCodeRef, PortableError,
    PortableOValue, PortableTagged, PortableValueError, PortableValueRecord,
};
pub use value_codec::{
    world_value_v1_conformance_bytes, world_value_v1_conformance_records,
    world_value_v1_conformance_sha256, MAX_OVALUE_BYTES_BYTES, MAX_OVALUE_DEPTH,
    MAX_OVALUE_IDENTIFIER_BYTES, MAX_OVALUE_INTEGER_BYTES, MAX_OVALUE_LIST_ITEMS,
    MAX_OVALUE_MAP_ENTRIES, MAX_OVALUE_NODES, MAX_OVALUE_RECORD_BYTES, MAX_OVALUE_TEXT_BYTES,
    MIN_OVALUE_RECORD_BYTES, OVALUE_NODE_HEADER_BYTES, OVALUE_WIRE_HEADER_BYTES, OVALUE_WIRE_MAGIC,
    OVALUE_WIRE_SCHEMA_V1,
};
