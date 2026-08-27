//! Authenticated execution-Fabric transport and provider runtime.
//!
//! The public surface is deliberately limited to opt-in configuration,
//! bounded client framing, immutable attempt status, and provider identity.
//! Realization and exact ledger mutation remain crate-private authority paths.

mod client;
mod coordinator;
mod keys;
mod ledger;
mod profile;
mod provider;
mod realizer;
mod wire;

pub(crate) use client::{FabricAttemptClientV1, FabricClientFailureV1};
pub use coordinator::RemotePureExecutionConfigV1;
pub use keys::{
    read_fabric_node_signing_key_v1, read_fabric_public_key_v1,
    write_new_fabric_node_signing_key_v1, write_new_fabric_public_key_v1,
};
pub(crate) use profile::trusted_inline_fabric_profile_v1;
pub use profile::trusted_inline_fabric_realization_pipeline_sha256_v1;
pub(crate) use provider::serve_fabric_stream_v1;
pub use provider::{FabricAttemptProviderConfigV1, FabricAttemptProviderV1};
pub use wire::{
    read_fabric_client_response_v1, write_fabric_client_request_v1, FABRIC_LENGTH_PREFIX_BYTES_V1,
    MAX_FABRIC_REQUEST_PAYLOAD_BYTES_V1, MAX_FABRIC_RESPONSE_PAYLOAD_BYTES_V1,
};
