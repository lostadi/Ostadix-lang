//! Shared, generation-bound identity vocabulary for governed Ostadix Worlds.
//!
//! These values identify state; they are not authority. Authority remains in a
//! live, authenticated capability broker. Every reference that can become
//! stale carries the generation of the independently mutable object it names,
//! so integer or name reuse cannot make an old reference current again. A
//! [`WorldEpoch`] is a snapshot/placement precondition; it is deliberately not
//! embedded in node, domain, or task identity because unrelated World updates
//! must not invalidate otherwise-live objects.

use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

const MAX_SIMPLE_ID_BYTES: usize = 128;
const MAX_RESOURCE_ID_BYTES: usize = 256;
const SHA256_HEX_BYTES: usize = 64;

/// Validation and stale-reference failures for governed identity.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorldIdentityError {
    #[error("{kind} must not be empty")]
    EmptyIdentifier { kind: &'static str },
    #[error("{kind} exceeds the {limit}-byte limit")]
    IdentifierTooLong { kind: &'static str, limit: usize },
    #[error("{kind} `{value}` contains an unsupported character or path component")]
    InvalidIdentifier { kind: &'static str, value: String },
    #[error("{kind} must be nonzero")]
    ZeroGeneration { kind: &'static str },
    #[error("{kind} is exhausted")]
    GenerationExhausted { kind: &'static str },
    #[error("{kind} identity mismatch: expected `{expected}`, got `{got}`")]
    IdentityMismatch {
        kind: &'static str,
        expected: String,
        got: String,
    },
    #[error("stale {kind}: expected generation {expected}, got {got}")]
    StaleGeneration {
        kind: &'static str,
        expected: u64,
        got: u64,
    },
    #[error("artifact identity must be exactly 64 lowercase hexadecimal SHA-256 characters")]
    InvalidArtifactDigest,
}

fn validate_component(kind: &'static str, value: &str) -> Result<(), WorldIdentityError> {
    if value.is_empty() {
        return Err(WorldIdentityError::EmptyIdentifier { kind });
    }
    if value.len() > MAX_SIMPLE_ID_BYTES {
        return Err(WorldIdentityError::IdentifierTooLong {
            kind,
            limit: MAX_SIMPLE_ID_BYTES,
        });
    }
    if matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(WorldIdentityError::InvalidIdentifier {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_resource_path(value: &str) -> Result<(), WorldIdentityError> {
    const KIND: &str = "resource identity";
    if value.is_empty() {
        return Err(WorldIdentityError::EmptyIdentifier { kind: KIND });
    }
    if value.len() > MAX_RESOURCE_ID_BYTES {
        return Err(WorldIdentityError::IdentifierTooLong {
            kind: KIND,
            limit: MAX_RESOURCE_ID_BYTES,
        });
    }
    if value.starts_with('/')
        || value.ends_with('/')
        || value.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || validate_component(KIND, component).is_err()
        })
    {
        return Err(WorldIdentityError::InvalidIdentifier {
            kind: KIND,
            value: value.to_owned(),
        });
    }
    Ok(())
}

macro_rules! string_identity {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, WorldIdentityError> {
                let value = value.into();
                validate_component($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = WorldIdentityError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = WorldIdentityError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

macro_rules! generation_identity {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(NonZeroU64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, WorldIdentityError> {
                NonZeroU64::new(value)
                    .map(Self)
                    .ok_or(WorldIdentityError::ZeroGeneration { kind: $kind })
            }

            pub fn get(self) -> u64 {
                self.0.get()
            }

            pub fn next(self) -> Result<Self, WorldIdentityError> {
                self.get()
                    .checked_add(1)
                    .ok_or(WorldIdentityError::GenerationExhausted { kind: $kind })
                    .and_then(Self::new)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(formatter)
            }
        }

        impl TryFrom<u64> for $name {
            type Error = WorldIdentityError;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = u64::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_identity!(WorldId, "world identity");
string_identity!(NodeId, "node identity");
string_identity!(DomainId, "domain identity");
string_identity!(TaskId, "task identity");

generation_identity!(WorldEpoch, "world epoch");
generation_identity!(NodeGeneration, "node generation");
generation_identity!(DomainGeneration, "domain generation");
generation_identity!(AttemptGeneration, "attempt generation");

/// A typed, relative resource name, for example `cpu/slot-0`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, WorldIdentityError> {
        let value = value.into();
        validate_resource_path(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ResourceId {
    type Err = WorldIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ResourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Content identity for an immutable artifact. Mutable publication state is
/// represented separately by [`ArtifactPublicationIdentity`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ArtifactId(String);

impl ArtifactId {
    pub fn from_sha256(value: impl Into<String>) -> Result<Self, WorldIdentityError> {
        let value = value.into();
        if value.len() != SHA256_HEX_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(WorldIdentityError::InvalidArtifactDigest);
        }
        Ok(Self(value))
    }

    pub fn as_sha256(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "sha256:{}", self.0)
    }
}

impl FromStr for ArtifactId {
    type Err = WorldIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_sha256(value.strip_prefix("sha256:").unwrap_or(value))
    }
}

impl<'de> Deserialize<'de> for ArtifactId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        // Wire records use one canonical bare-lowercase digest spelling even
        // though the human-facing Display/FromStr pair accepts `sha256:`.
        Self::from_sha256(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldIdentity {
    world: WorldId,
    epoch: WorldEpoch,
}

impl WorldIdentity {
    pub fn new(world: WorldId, epoch: WorldEpoch) -> Self {
        Self { world, epoch }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn epoch(&self) -> WorldEpoch {
        self.epoch
    }

    /// Fence a reference against the exact current World epoch.
    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        require_same_logical("world", &self.world, &reference.world)?;
        require_generation("world epoch", self.epoch.get(), reference.epoch.get())
    }
}

impl fmt::Display for WorldIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.world, self.epoch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeIdentity {
    world: WorldId,
    node: NodeId,
    generation: NodeGeneration,
}

impl NodeIdentity {
    pub fn new(world: WorldId, node: NodeId, generation: NodeGeneration) -> Self {
        Self {
            world,
            node,
            generation,
        }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn node(&self) -> &NodeId {
        &self.node
    }

    pub fn generation(&self) -> NodeGeneration {
        self.generation
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        require_same_logical("world", &self.world, &reference.world)?;
        require_same_logical("node", &self.node, &reference.node)?;
        require_generation(
            "node generation",
            self.generation.get(),
            reference.generation.get(),
        )
    }
}

impl fmt::Display for NodeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/node:{}@{}",
            self.world, self.node, self.generation
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainIdentity {
    node: NodeIdentity,
    domain: DomainId,
    generation: DomainGeneration,
}

impl DomainIdentity {
    pub fn new(node: NodeIdentity, domain: DomainId, generation: DomainGeneration) -> Self {
        Self {
            node,
            domain,
            generation,
        }
    }

    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }

    pub fn domain(&self) -> &DomainId {
        &self.domain
    }

    pub fn generation(&self) -> DomainGeneration {
        self.generation
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        self.node.require_current(&reference.node)?;
        require_same_logical("domain", &self.domain, &reference.domain)?;
        require_generation(
            "domain generation",
            self.generation.get(),
            reference.generation.get(),
        )
    }
}

impl fmt::Display for DomainIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/domain:{}@{}",
            self.node, self.domain, self.generation
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope", deny_unknown_fields)]
pub enum ResourceOwner {
    World { world: WorldId },
    Node { node: NodeIdentity },
    Domain { domain: DomainIdentity },
}

impl ResourceOwner {
    pub fn world(&self) -> &WorldId {
        match self {
            Self::World { world } => world,
            Self::Node { node } => node.world(),
            Self::Domain { domain } => domain.node().world(),
        }
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        match (self, reference) {
            (Self::World { world }, Self::World { world: other }) => {
                require_same_logical("world", world, other)
            }
            (Self::Node { node }, Self::Node { node: other }) => node.require_current(other),
            (Self::Domain { domain }, Self::Domain { domain: other }) => {
                domain.require_current(other)
            }
            _ => Err(WorldIdentityError::IdentityMismatch {
                kind: "resource owner",
                expected: self.to_string(),
                got: reference.to_string(),
            }),
        }
    }
}

impl fmt::Display for ResourceOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::World { world } => world.fmt(formatter),
            Self::Node { node } => node.fmt(formatter),
            Self::Domain { domain } => domain.fmt(formatter),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceIdentity {
    owner: ResourceOwner,
    resource: ResourceId,
}

impl ResourceIdentity {
    pub fn new(owner: ResourceOwner, resource: ResourceId) -> Self {
        Self { owner, resource }
    }

    pub fn owner(&self) -> &ResourceOwner {
        &self.owner
    }

    pub fn resource(&self) -> &ResourceId {
        &self.resource
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        self.owner.require_current(&reference.owner)?;
        require_same_logical("resource", &self.resource, &reference.resource)
    }
}

impl fmt::Display for ResourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/resource:{}", self.owner, self.resource)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAttemptIdentity {
    world: WorldId,
    task: TaskId,
    attempt: AttemptGeneration,
}

impl TaskAttemptIdentity {
    pub fn new(world: WorldId, task: TaskId, attempt: AttemptGeneration) -> Self {
        Self {
            world,
            task,
            attempt,
        }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn task(&self) -> &TaskId {
        &self.task
    }

    pub fn attempt(&self) -> AttemptGeneration {
        self.attempt
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        require_same_logical("world", &self.world, &reference.world)?;
        require_same_logical("task", &self.task, &reference.task)?;
        require_generation("task attempt", self.attempt.get(), reference.attempt.get())
    }
}

impl fmt::Display for TaskAttemptIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/task:{}@{}",
            self.world, self.task, self.attempt
        )
    }
}

/// Publication membership for an immutable artifact at an exact World
/// snapshot. The [`ArtifactId`] itself remains global content identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPublicationIdentity {
    world: WorldIdentity,
    artifact: ArtifactId,
}

impl ArtifactPublicationIdentity {
    pub fn new(world: WorldIdentity, artifact: ArtifactId) -> Self {
        Self { world, artifact }
    }

    pub fn world(&self) -> &WorldIdentity {
        &self.world
    }

    pub fn artifact(&self) -> &ArtifactId {
        &self.artifact
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        self.world.require_current(&reference.world)?;
        require_same_logical("artifact", &self.artifact, &reference.artifact)
    }
}

impl fmt::Display for ArtifactPublicationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/artifact:{}", self.world, self.artifact)
    }
}

/// Descriptive provenance for a verified KernelWorld bound beneath an
/// explicitly supplied execution domain. It does not contain a bearer token
/// and cannot grant authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelWorldBinding {
    domain: DomainIdentity,
    kernel_world_name: ResourceId,
    package: ArtifactId,
    provider_generation: NonZeroU64,
}

impl KernelWorldBinding {
    /// Construct validated descriptive provenance. This does not verify a
    /// package and cannot mint authority; trusted hosted callers should prefer
    /// `KernelWorldIdentity::bind_execution_domain`.
    pub fn from_descriptive_parts(
        domain: DomainIdentity,
        kernel_world_name: impl Into<String>,
        package_sha256: impl Into<String>,
        provider_generation: u64,
    ) -> Result<Self, WorldIdentityError> {
        let kernel_world_name = ResourceId::new(kernel_world_name)?;
        let provider_generation =
            NonZeroU64::new(provider_generation).ok_or(WorldIdentityError::ZeroGeneration {
                kind: "kernel world generation",
            })?;
        Ok(Self {
            domain,
            kernel_world_name,
            package: ArtifactId::from_sha256(package_sha256)?,
            provider_generation,
        })
    }

    pub fn domain(&self) -> &DomainIdentity {
        &self.domain
    }

    pub fn kernel_world_name(&self) -> &str {
        self.kernel_world_name.as_str()
    }

    pub fn package(&self) -> &ArtifactId {
        &self.package
    }

    /// Provider lifecycle generation inside the allocated execution domain.
    /// This is provenance, not the domain's registry-allocated generation.
    pub fn provider_generation(&self) -> u64 {
        self.provider_generation.get()
    }
}

fn require_same_logical<T: fmt::Display + PartialEq>(
    kind: &'static str,
    expected: &T,
    got: &T,
) -> Result<(), WorldIdentityError> {
    if expected == got {
        Ok(())
    } else {
        Err(WorldIdentityError::IdentityMismatch {
            kind,
            expected: expected.to_string(),
            got: got.to_string(),
        })
    }
}

fn require_generation(
    kind: &'static str,
    expected: u64,
    got: u64,
) -> Result<(), WorldIdentityError> {
    if expected == got {
        Ok(())
    } else {
        Err(WorldIdentityError::StaleGeneration {
            kind,
            expected,
            got,
        })
    }
}
