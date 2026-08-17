//! Descriptor-based placement authority for hosted Ostadix execution.
//!
//! This module is intentionally transport-independent.  Its records describe
//! requirements, targets, warrants, observations, and leases, while callers
//! remain responsible for authenticating the detached record envelopes.  A
//! record is never authority merely because it deserialized successfully.

mod projection;

/// Historical nested path for the canonical placement-protocol vocabulary.
///
/// The protocol is compiled exactly once as the crate-private
/// `placement_protocol` module. This inline module is a public projection of
/// that one identity, not a second module declaration over the same files.
pub mod protocol {
    pub use crate::placement_protocol::*;
}

pub use crate::placement_protocol::*;
pub use projection::{
    requirement_footprint_for_island, requirement_footprint_for_plan_node,
    requirement_footprint_for_program_node, PlacementIntentV1,
    SESSION_SERIALIZED_OPAQUE_EFFECTS_CAPABILITY_V1,
    SESSION_SERIALIZED_OPAQUE_EFFECTS_NAMESPACE_V1, SESSION_SERIALIZED_OPAQUE_EFFECTS_NAME_V1,
};
