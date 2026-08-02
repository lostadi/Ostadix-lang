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
string_identity!(ProcessId, "process identity");
string_identity!(NodeId, "node identity");
string_identity!(DomainId, "domain identity");
string_identity!(ObjectId, "object identity");
string_identity!(CapabilityId, "capability identity");
string_identity!(LeaseId, "lease identity");
string_identity!(TaskId, "task identity");
string_identity!(CheckpointId, "checkpoint identity");
string_identity!(ReceiptId, "receipt identity");

generation_identity!(WorldEpoch, "world epoch");
generation_identity!(GovernorTerm, "governor term");
generation_identity!(GovernorLogIndex, "governor log index");
generation_identity!(NodeGeneration, "node generation");
generation_identity!(DomainGeneration, "domain generation");
generation_identity!(ProcessGeneration, "process generation");
generation_identity!(ResourceGeneration, "resource generation");
generation_identity!(ObjectVersion, "object version");
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

/// Exact replicated-state position for a Governor serving a World.
///
/// This value is descriptive identity only. It is not a leadership proof and
/// carries no authority to act for the World.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernorIdentity {
    world: WorldIdentity,
    term: GovernorTerm,
    log_index: GovernorLogIndex,
}

impl GovernorIdentity {
    pub fn new(world: WorldIdentity, term: GovernorTerm, log_index: GovernorLogIndex) -> Self {
        Self {
            world,
            term,
            log_index,
        }
    }

    pub fn world(&self) -> &WorldIdentity {
        &self.world
    }

    pub fn term(&self) -> GovernorTerm {
        self.term
    }

    pub fn log_index(&self) -> GovernorLogIndex {
        self.log_index
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        self.world.require_current(&reference.world)?;
        require_generation("governor term", self.term.get(), reference.term.get())?;
        require_generation(
            "governor log index",
            self.log_index.get(),
            reference.log_index.get(),
        )
    }
}

impl fmt::Display for GovernorIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/governor:term-{}@{}",
            self.world, self.term, self.log_index
        )
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
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    domain: DomainIdentity,
    process: ProcessId,
    generation: ProcessGeneration,
}

impl ProcessIdentity {
    pub fn new(domain: DomainIdentity, process: ProcessId, generation: ProcessGeneration) -> Self {
        Self {
            domain,
            process,
            generation,
        }
    }

    pub fn domain(&self) -> &DomainIdentity {
        &self.domain
    }

    pub fn process(&self) -> &ProcessId {
        &self.process
    }

    pub fn generation(&self) -> ProcessGeneration {
        self.generation
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        self.domain.require_current(&reference.domain)?;
        require_same_logical("process", &self.process, &reference.process)?;
        require_generation(
            "process generation",
            self.generation.get(),
            reference.generation.get(),
        )
    }
}

impl fmt::Display for ProcessIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/process:{}@{}",
            self.domain, self.process, self.generation
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope", deny_unknown_fields)]
pub enum ResourceOwner {
    World { world: WorldIdentity },
    Node { node: NodeIdentity },
    Domain { domain: DomainIdentity },
    Process { process: ProcessIdentity },
}

impl ResourceOwner {
    pub fn world(&self) -> &WorldId {
        match self {
            Self::World { world } => world.world(),
            Self::Node { node } => node.world(),
            Self::Domain { domain } => domain.node().world(),
            Self::Process { process } => process.domain().node().world(),
        }
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        match (self, reference) {
            (Self::World { world }, Self::World { world: other }) => world.require_current(other),
            (Self::Node { node }, Self::Node { node: other }) => node.require_current(other),
            (Self::Domain { domain }, Self::Domain { domain: other }) => {
                domain.require_current(other)
            }
            (Self::Process { process }, Self::Process { process: other }) => {
                process.require_current(other)
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
            Self::Process { process } => process.fmt(formatter),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceIdentity {
    owner: ResourceOwner,
    resource: ResourceId,
    generation: ResourceGeneration,
}

impl ResourceIdentity {
    pub fn new(owner: ResourceOwner, resource: ResourceId, generation: ResourceGeneration) -> Self {
        Self {
            owner,
            resource,
            generation,
        }
    }

    pub fn owner(&self) -> &ResourceOwner {
        &self.owner
    }

    pub fn resource(&self) -> &ResourceId {
        &self.resource
    }

    pub fn generation(&self) -> ResourceGeneration {
        self.generation
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        self.owner.require_current(&reference.owner)?;
        require_same_logical("resource", &self.resource, &reference.resource)?;
        require_generation(
            "resource generation",
            self.generation.get(),
            reference.generation.get(),
        )
    }
}

impl fmt::Display for ResourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/resource:{}@{}",
            self.owner, self.resource, self.generation
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectIdentity {
    world: WorldId,
    object: ObjectId,
    version: ObjectVersion,
}

impl ObjectIdentity {
    pub fn new(world: WorldId, object: ObjectId, version: ObjectVersion) -> Self {
        Self {
            world,
            object,
            version,
        }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn object(&self) -> &ObjectId {
        &self.object
    }

    pub fn version(&self) -> ObjectVersion {
        self.version
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        require_same_logical("world", &self.world, &reference.world)?;
        require_same_logical("object", &self.object, &reference.object)?;
        require_generation(
            "object version",
            self.version.get(),
            reference.version.get(),
        )
    }
}

impl fmt::Display for ObjectIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/object:{}@{}",
            self.world, self.object, self.version
        )
    }
}

/// Inert capability identity. Possessing this identifier does not grant the
/// capability or authorize any operation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityIdentity {
    world: WorldId,
    capability: CapabilityId,
}

impl CapabilityIdentity {
    pub fn new(world: WorldId, capability: CapabilityId) -> Self {
        Self { world, capability }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn capability(&self) -> &CapabilityId {
        &self.capability
    }
}

impl fmt::Display for CapabilityIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/capability:{}", self.world, self.capability)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseIdentity {
    world: WorldId,
    lease: LeaseId,
}

impl LeaseIdentity {
    pub fn new(world: WorldId, lease: LeaseId) -> Self {
        Self { world, lease }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn lease(&self) -> &LeaseId {
        &self.lease
    }
}

impl fmt::Display for LeaseIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/lease:{}", self.world, self.lease)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskIdentity {
    world: WorldId,
    task: TaskId,
}

impl TaskIdentity {
    pub fn new(world: WorldId, task: TaskId) -> Self {
        Self { world, task }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn task(&self) -> &TaskId {
        &self.task
    }
}

impl fmt::Display for TaskIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/task:{}", self.world, self.task)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptIdentity {
    world: WorldId,
    task: TaskId,
    attempt: AttemptGeneration,
}

impl AttemptIdentity {
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

impl fmt::Display for AttemptIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/task:{}@{}",
            self.world, self.task, self.attempt
        )
    }
}

/// Compatibility name retained for callers from the first World identity
/// slice. Both names denote the same generation-bound value.
pub type TaskAttemptIdentity = AttemptIdentity;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointIdentity {
    attempt: AttemptIdentity,
    checkpoint: CheckpointId,
}

impl CheckpointIdentity {
    pub fn new(attempt: AttemptIdentity, checkpoint: CheckpointId) -> Self {
        Self {
            attempt,
            checkpoint,
        }
    }

    pub fn attempt(&self) -> &AttemptIdentity {
        &self.attempt
    }

    pub fn checkpoint(&self) -> &CheckpointId {
        &self.checkpoint
    }

    pub fn require_current(&self, reference: &Self) -> Result<(), WorldIdentityError> {
        self.attempt.require_current(&reference.attempt)?;
        require_same_logical("checkpoint", &self.checkpoint, &reference.checkpoint)
    }
}

impl fmt::Display for CheckpointIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/checkpoint:{}", self.attempt, self.checkpoint)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptIdentity {
    world: WorldId,
    receipt: ReceiptId,
}

impl ReceiptIdentity {
    pub fn new(world: WorldId, receipt: ReceiptId) -> Self {
        Self { world, receipt }
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    pub fn receipt(&self) -> &ReceiptId {
        &self.receipt
    }
}

impl fmt::Display for ReceiptIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/receipt:{}", self.world, self.receipt)
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
