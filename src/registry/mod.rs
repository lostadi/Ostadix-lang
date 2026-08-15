//! Transport-independent, append-only placement registry.
//!
//! Registry v1 authenticates namespace-scoped node-profile publication. It is
//! intentionally usable through local files without claiming a discovery or
//! network service. Any future transport carries the same canonical snapshots.

pub mod bundle;
mod crypto;
mod error;
mod model;
mod store;
mod verify;

use serde::Serialize;
use sha2::{Digest, Sha256};

pub use crypto::{
    append_namespace_delegation, append_profile_publication, append_profile_to_store,
    create_registry_root, registry_public_key_id, RegistrySignerV1,
};
pub use error::RegistryError;
pub use model::{
    NamespaceDelegationV1, NamespaceRootV1, ProfilePublicationV1, ProfileStalenessPolicyV1,
    RegistryEventBodyV1, RegistryEventV1, RegistryProfileKeyV1, RegistryPublicKeyV1,
    RegistryRootPinV1, RegistrySnapshotV1, RegistryStoreV1, RegistryTrustV1, SignedRegistryEventV1,
    VerifiedRegistryProfileV1, VerifiedRegistryV1, MAX_NAMESPACE_BYTES, MAX_NODE_ID_BYTES,
    MAX_REGISTRY_EVENTS, MAX_REGISTRY_SNAPSHOTS, REGISTRY_SCHEMA_V1,
};
pub use store::{
    append_profile_to_registry_state, atomic_write_node_profile_json, atomic_write_registry_store,
    atomic_write_registry_trust, export_registry_store, import_registry_store,
    read_node_profile_json, read_registry_store, read_registry_trust, read_signing_key,
    write_new_registry_state, RegistryStatePathsV1, MAX_REGISTRY_INPUT_BYTES,
};
pub use verify::{merge_registry_store, verify_registry_store};

const REGISTRY_EVENT_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/REGISTRY-SIGNED-EVENT/V1\0";
const REGISTRY_RECORD_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/REGISTRY-RECORD/V1\0";

/// Deterministic canonical CBOR shared with Ostadix's existing wire codec.
pub fn canonical_registry_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, RegistryError> {
    crate::wire::encode_message(value).map_err(|error| RegistryError::Canonical(error.to_string()))
}

pub fn registry_record_sha256<T: Serialize>(value: &T) -> Result<[u8; 32], RegistryError> {
    domain_digest(
        REGISTRY_RECORD_DIGEST_DOMAIN_V1,
        &canonical_registry_bytes(value)?,
    )
}

pub fn registry_event_sha256(event: &SignedRegistryEventV1) -> Result<[u8; 32], RegistryError> {
    domain_digest(
        REGISTRY_EVENT_DIGEST_DOMAIN_V1,
        &canonical_registry_bytes(event)?,
    )
}

fn domain_digest(domain: &[u8], bytes: &[u8]) -> Result<[u8; 32], RegistryError> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| RegistryError::Canonical("registry record is too large".to_owned()))?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}
