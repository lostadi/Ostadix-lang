use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::placement_protocol::NodeProfileV1;

use super::RegistryError;

pub const REGISTRY_SCHEMA_V1: u16 = 1;
pub const MAX_REGISTRY_EVENTS: usize = 16_384;
pub const MAX_REGISTRY_SNAPSHOTS: usize = 256;
pub const MAX_NAMESPACE_BYTES: usize = 128;
pub const MAX_NODE_ID_BYTES: usize = 128;

pub type RegistryPublicKeyV1 = [u8; 32];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileStalenessPolicyV1 {
    #[default]
    Reject,
    AllowExpired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceRootV1 {
    schema_version: u16,
    namespace: String,
    root_public_key: RegistryPublicKeyV1,
    valid_from_ms: u64,
    expires_at_ms: u64,
}

impl NamespaceRootV1 {
    pub fn new(
        namespace: impl Into<String>,
        root_public_key: RegistryPublicKeyV1,
        valid_from_ms: u64,
        expires_at_ms: u64,
    ) -> Result<Self, RegistryError> {
        let namespace = namespace.into();
        validate_namespace("root namespace", &namespace)?;
        validate_validity("namespace root", valid_from_ms, expires_at_ms)?;
        Ok(Self {
            schema_version: REGISTRY_SCHEMA_V1,
            namespace,
            root_public_key,
            valid_from_ms,
            expires_at_ms,
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn public_key(&self) -> &RegistryPublicKeyV1 {
        &self.root_public_key
    }

    pub fn valid_from_ms(&self) -> u64 {
        self.valid_from_ms
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    pub(crate) fn validate(&self) -> Result<(), RegistryError> {
        validate_version("namespace root", self.schema_version)?;
        validate_namespace("root namespace", &self.namespace)?;
        validate_validity("namespace root", self.valid_from_ms, self.expires_at_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceDelegationV1 {
    schema_version: u16,
    parent_namespace: String,
    child_namespace: String,
    delegate_public_key: RegistryPublicKeyV1,
    valid_from_ms: u64,
    expires_at_ms: u64,
}

impl NamespaceDelegationV1 {
    pub fn new(
        parent_namespace: impl Into<String>,
        child_namespace: impl Into<String>,
        delegate_public_key: RegistryPublicKeyV1,
        valid_from_ms: u64,
        expires_at_ms: u64,
    ) -> Result<Self, RegistryError> {
        let parent_namespace = parent_namespace.into();
        let child_namespace = child_namespace.into();
        validate_namespace("delegation parent namespace", &parent_namespace)?;
        validate_namespace("delegation child namespace", &child_namespace)?;
        if !is_strict_descendant(&child_namespace, &parent_namespace) {
            return Err(RegistryError::InvalidDelegationScope {
                parent: parent_namespace,
                child: child_namespace,
            });
        }
        validate_validity("namespace delegation", valid_from_ms, expires_at_ms)?;
        Ok(Self {
            schema_version: REGISTRY_SCHEMA_V1,
            parent_namespace,
            child_namespace,
            delegate_public_key,
            valid_from_ms,
            expires_at_ms,
        })
    }

    pub fn parent_namespace(&self) -> &str {
        &self.parent_namespace
    }

    pub fn child_namespace(&self) -> &str {
        &self.child_namespace
    }

    pub fn delegate_public_key(&self) -> &RegistryPublicKeyV1 {
        &self.delegate_public_key
    }

    pub fn valid_from_ms(&self) -> u64 {
        self.valid_from_ms
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    pub(crate) fn validate(&self) -> Result<(), RegistryError> {
        validate_version("namespace delegation", self.schema_version)?;
        validate_namespace("delegation parent namespace", &self.parent_namespace)?;
        validate_namespace("delegation child namespace", &self.child_namespace)?;
        if !is_strict_descendant(&self.child_namespace, &self.parent_namespace) {
            return Err(RegistryError::InvalidDelegationScope {
                parent: self.parent_namespace.clone(),
                child: self.child_namespace.clone(),
            });
        }
        validate_validity(
            "namespace delegation",
            self.valid_from_ms,
            self.expires_at_ms,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePublicationV1 {
    schema_version: u16,
    namespace: String,
    node_id: String,
    profile: NodeProfileV1,
}

impl ProfilePublicationV1 {
    pub fn new(
        namespace: impl Into<String>,
        node_id: impl Into<String>,
        profile: NodeProfileV1,
    ) -> Result<Self, RegistryError> {
        let namespace = namespace.into();
        let node_id = node_id.into();
        validate_namespace("profile namespace", &namespace)?;
        validate_node_id(&node_id)?;
        if profile.descriptor().node_id() != node_id {
            return Err(RegistryError::ProfileNodeMismatch {
                profile: profile.descriptor().node_id().to_owned(),
                publication: node_id,
            });
        }
        Ok(Self {
            schema_version: REGISTRY_SCHEMA_V1,
            namespace,
            node_id,
            profile,
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn profile(&self) -> &NodeProfileV1 {
        &self.profile
    }

    pub(crate) fn validate(&self) -> Result<(), RegistryError> {
        validate_version("profile publication", self.schema_version)?;
        validate_namespace("profile namespace", &self.namespace)?;
        validate_node_id(&self.node_id)?;
        if self.profile.descriptor().node_id() != self.node_id {
            return Err(RegistryError::ProfileNodeMismatch {
                profile: self.profile.descriptor().node_id().to_owned(),
                publication: self.node_id.clone(),
            });
        }
        Ok(())
    }
}

// This enum is the stable V1 registry-event vocabulary. Keep its direct
// construction shape aligned with the frozen record model.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "record", rename_all = "kebab-case")]
pub enum RegistryEventBodyV1 {
    NamespaceRoot(NamespaceRootV1),
    NamespaceDelegation(NamespaceDelegationV1),
    PublishProfile(ProfilePublicationV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEventV1 {
    schema_version: u16,
    sequence: u64,
    previous_event_sha256: Option<[u8; 32]>,
    issued_at_ms: u64,
    namespace: String,
    signer_public_key: RegistryPublicKeyV1,
    body: RegistryEventBodyV1,
}

impl RegistryEventV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sequence: u64,
        previous_event_sha256: Option<[u8; 32]>,
        issued_at_ms: u64,
        namespace: String,
        signer_public_key: RegistryPublicKeyV1,
        body: RegistryEventBodyV1,
    ) -> Result<Self, RegistryError> {
        if sequence == 0 {
            return Err(RegistryError::ZeroSequence);
        }
        validate_namespace("event namespace", &namespace)?;
        Ok(Self {
            schema_version: REGISTRY_SCHEMA_V1,
            sequence,
            previous_event_sha256,
            issued_at_ms,
            namespace,
            signer_public_key,
            body,
        })
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn previous_event_sha256(&self) -> Option<&[u8; 32]> {
        self.previous_event_sha256.as_ref()
    }

    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn signer_public_key(&self) -> &RegistryPublicKeyV1 {
        &self.signer_public_key
    }

    pub fn body(&self) -> &RegistryEventBodyV1 {
        &self.body
    }

    pub(crate) fn validate_shape(&self) -> Result<(), RegistryError> {
        validate_version("registry event", self.schema_version)?;
        if self.sequence == 0 {
            return Err(RegistryError::ZeroSequence);
        }
        validate_namespace("event namespace", &self.namespace)?;
        match &self.body {
            RegistryEventBodyV1::NamespaceRoot(root) => root.validate(),
            RegistryEventBodyV1::NamespaceDelegation(delegation) => delegation.validate(),
            RegistryEventBodyV1::PublishProfile(publication) => publication.validate(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRegistryEventV1 {
    event: RegistryEventV1,
    signature: Vec<u8>,
}

impl SignedRegistryEventV1 {
    pub(crate) fn new(event: RegistryEventV1, signature: [u8; 64]) -> Self {
        Self {
            event,
            signature: signature.to_vec(),
        }
    }

    pub fn event(&self) -> &RegistryEventV1 {
        &self.event
    }

    pub fn signature(&self) -> &[u8] {
        &self.signature
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySnapshotV1 {
    schema_version: u16,
    events: Vec<SignedRegistryEventV1>,
}

impl RegistrySnapshotV1 {
    pub fn new(root: SignedRegistryEventV1) -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_V1,
            events: vec![root],
        }
    }

    pub fn events(&self) -> &[SignedRegistryEventV1] {
        &self.events
    }

    pub fn last_sequence(&self) -> u64 {
        self.events
            .last()
            .map(|signed| signed.event.sequence)
            .unwrap_or(0)
    }

    pub(crate) fn events_mut(&mut self) -> &mut Vec<SignedRegistryEventV1> {
        &mut self.events
    }

    pub(crate) fn validate_shape(&self) -> Result<(), RegistryError> {
        validate_version("registry snapshot", self.schema_version)?;
        if self.events.is_empty() {
            return Err(RegistryError::EmptySnapshot);
        }
        if self.events.len() > MAX_REGISTRY_EVENTS {
            return Err(RegistryError::TooManyEvents {
                maximum: MAX_REGISTRY_EVENTS,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryStoreV1 {
    schema_version: u16,
    snapshots: Vec<RegistrySnapshotV1>,
}

impl RegistryStoreV1 {
    pub fn new(snapshot: RegistrySnapshotV1) -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_V1,
            snapshots: vec![snapshot],
        }
    }

    pub fn snapshots(&self) -> &[RegistrySnapshotV1] {
        &self.snapshots
    }

    pub(crate) fn snapshots_mut(&mut self) -> &mut Vec<RegistrySnapshotV1> {
        &mut self.snapshots
    }

    pub(crate) fn validate_shape(&self) -> Result<(), RegistryError> {
        validate_version("registry store", self.schema_version)?;
        if self.snapshots.len() > MAX_REGISTRY_SNAPSHOTS {
            return Err(RegistryError::TooManySnapshots {
                maximum: MAX_REGISTRY_SNAPSHOTS,
            });
        }
        for snapshot in &self.snapshots {
            snapshot.validate_shape()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryRootPinV1 {
    namespace: String,
    public_key: RegistryPublicKeyV1,
}

impl RegistryRootPinV1 {
    pub fn new(
        namespace: impl Into<String>,
        public_key: RegistryPublicKeyV1,
    ) -> Result<Self, RegistryError> {
        let namespace = namespace.into();
        validate_namespace("pinned root namespace", &namespace)?;
        Ok(Self {
            namespace,
            public_key,
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn public_key(&self) -> &RegistryPublicKeyV1 {
        &self.public_key
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryTrustV1 {
    schema_version: u16,
    roots: Vec<RegistryRootPinV1>,
}

impl RegistryTrustV1 {
    pub fn new(roots: impl IntoIterator<Item = RegistryRootPinV1>) -> Result<Self, RegistryError> {
        let mut roots: Vec<_> = roots.into_iter().collect();
        roots.sort_by(|left, right| {
            left.namespace
                .cmp(&right.namespace)
                .then_with(|| left.public_key.cmp(&right.public_key))
        });
        roots.dedup();
        if roots.is_empty() {
            return Err(RegistryError::Empty {
                field: "registry trust roots",
            });
        }
        Ok(Self {
            schema_version: REGISTRY_SCHEMA_V1,
            roots,
        })
    }

    pub fn roots(&self) -> &[RegistryRootPinV1] {
        &self.roots
    }

    pub(crate) fn validate(&self) -> Result<(), RegistryError> {
        validate_version("registry trust", self.schema_version)?;
        if self.roots.is_empty() {
            return Err(RegistryError::Empty {
                field: "registry trust roots",
            });
        }
        let canonical = Self::new(self.roots.clone())?;
        if canonical.roots != self.roots {
            return Err(RegistryError::NonCanonicalEncoding);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegistryProfileKeyV1 {
    namespace: String,
    node_id: String,
}

impl RegistryProfileKeyV1 {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub(crate) fn new(namespace: String, node_id: String) -> Self {
        Self { namespace, node_id }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRegistryProfileV1 {
    publication: ProfilePublicationV1,
    event_sha256: [u8; 32],
    issued_at_ms: u64,
    expires_at_ms: u64,
    stale: bool,
}

impl VerifiedRegistryProfileV1 {
    pub fn publication(&self) -> &ProfilePublicationV1 {
        &self.publication
    }

    pub fn event_sha256(&self) -> &[u8; 32] {
        &self.event_sha256
    }

    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub(crate) fn new(
        publication: ProfilePublicationV1,
        event_sha256: [u8; 32],
        issued_at_ms: u64,
        expires_at_ms: u64,
        stale: bool,
    ) -> Self {
        Self {
            publication,
            event_sha256,
            issued_at_ms,
            expires_at_ms,
            stale,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRegistryV1 {
    profiles: BTreeMap<RegistryProfileKeyV1, VerifiedRegistryProfileV1>,
    verified_snapshots: usize,
    last_sequences: BTreeMap<String, u64>,
}

impl VerifiedRegistryV1 {
    pub fn profiles(&self) -> &BTreeMap<RegistryProfileKeyV1, VerifiedRegistryProfileV1> {
        &self.profiles
    }

    pub fn verified_snapshots(&self) -> usize {
        self.verified_snapshots
    }

    pub fn last_sequences(&self) -> &BTreeMap<String, u64> {
        &self.last_sequences
    }

    pub(crate) fn new(
        profiles: BTreeMap<RegistryProfileKeyV1, VerifiedRegistryProfileV1>,
        verified_snapshots: usize,
        last_sequences: BTreeMap<String, u64>,
    ) -> Self {
        Self {
            profiles,
            verified_snapshots,
            last_sequences,
        }
    }
}

pub(crate) fn validate_version(record: &'static str, found: u16) -> Result<(), RegistryError> {
    if found == REGISTRY_SCHEMA_V1 {
        Ok(())
    } else {
        Err(RegistryError::UnsupportedVersion {
            record,
            found,
            expected: REGISTRY_SCHEMA_V1,
        })
    }
}

pub(crate) fn validate_validity(
    record: &'static str,
    valid_from_ms: u64,
    expires_at_ms: u64,
) -> Result<(), RegistryError> {
    if valid_from_ms < expires_at_ms {
        Ok(())
    } else {
        Err(RegistryError::InvalidValidity { record })
    }
}

pub(crate) fn validate_namespace(
    field: &'static str,
    namespace: &str,
) -> Result<(), RegistryError> {
    if namespace.is_empty() {
        return Err(RegistryError::Empty { field });
    }
    if namespace.len() > MAX_NAMESPACE_BYTES {
        return Err(RegistryError::TooLong {
            field,
            maximum: MAX_NAMESPACE_BYTES,
        });
    }
    let valid = namespace.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
            && segment
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && segment
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
    });
    if valid {
        Ok(())
    } else {
        Err(RegistryError::InvalidNamespace {
            field,
            value: namespace.to_owned(),
        })
    }
}

pub(crate) fn validate_node_id(node_id: &str) -> Result<(), RegistryError> {
    if node_id.is_empty() {
        return Err(RegistryError::Empty {
            field: "profile node identity",
        });
    }
    if node_id.len() > MAX_NODE_ID_BYTES {
        return Err(RegistryError::TooLong {
            field: "profile node identity",
            maximum: MAX_NODE_ID_BYTES,
        });
    }
    if node_id.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(RegistryError::InvalidNamespace {
            field: "profile node identity",
            value: node_id.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn is_within_scope(namespace: &str, scope: &str) -> bool {
    namespace == scope
        || namespace
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(crate) fn is_strict_descendant(namespace: &str, scope: &str) -> bool {
    namespace != scope && is_within_scope(namespace, scope)
}
