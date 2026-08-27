//! Canonical authority and envelope records for authenticated Fabric V1.
//!
//! This layer binds the frozen, authority-free M2 capsule to an explicitly
//! trusted issuer and exact provider. It owns no transport, filesystem,
//! evaluator, graph publication, settlement, or retry behavior.

mod codec;
mod crypto;
mod protocol;
#[cfg(test)]
mod tests;

pub(crate) use protocol::validate_node_id;

pub use codec::{
    decode_fabric_request_v1, decode_fabric_response_v1, encode_fabric_request_v1,
    encode_fabric_response_v1, encode_placement_lease_v3, fabric_lease_sha256_v3,
    FabricEncodedMessageV1,
};
pub use crypto::{
    FabricSigningKeyV1, PinnedFabricNodeKeyV1, TrustedFabricAuthoritiesV1,
    FABRIC_EXECUTION_LEASE_SIGNING_DOMAIN_V3, FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1,
};
pub use protocol::{
    ExecutionCellIncarnationV1, FabricAbandonmentV1, FabricAttemptQueryV1, FabricAttemptStatusV1,
    FabricAuthorityError, FabricExactPayloadV1, FabricRejectionV1, FabricRequestV1,
    FabricResponseV1, FabricSourceClosureV1, FabricSubmissionHeaderV1, FabricSubmissionV1,
    FabricTargetBindingV1, FabricTerminalCandidateV1, FabricTerminalStatusV1, PlacementLeaseV3,
    SignedExecutionLeaseV3, SignedTerminalCandidateReceiptV1, TerminalCandidateReceiptV1,
    FABRIC_CLOCK_SKEW_TOLERANCE_MS, FABRIC_PLACEMENT_LEASE_SCHEMA_V3, FABRIC_REQUEST_SCHEMA_V1,
    FABRIC_RESPONSE_SCHEMA_V1, FABRIC_SIGNED_LEASE_SCHEMA_V3,
    FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1, FABRIC_SOURCE_CLOSURE_DIALECT_V1,
    FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1, FABRIC_SOURCE_CLOSURE_SCHEMA_V1,
    FABRIC_SUBMISSION_SCHEMA_V1, FABRIC_TERMINAL_RECEIPT_SCHEMA_V1, MAX_FABRIC_HEADER_BYTES,
    MAX_FABRIC_LEASE_LIFETIME_MS,
};
