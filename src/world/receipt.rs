//! Typed, authority-free facts carried by the canonical execution receipt.
//!
//! A receipt describes an execution. It is not a capability, a Governor
//! snapshot, or proof that the named state is still current. Signed-receipt
//! construction requires an explicit current-state fence; signature trust is an
//! independent policy supplied to the codec verifier.

use std::cmp::Ordering;

use thiserror::Error;

use super::identity::{
    ArtifactId, AttemptIdentity, CapabilityIdentity, CheckpointIdentity, DomainIdentity,
    GovernorIdentity, NodeIdentity, ObjectIdentity, ProcessIdentity, ReceiptIdentity,
    ResourceIdentity, WorldIdentity, WorldIdentityError,
};
use super::identity_wire::{IdentityWireError, IdentityWireRecord};
use super::value::{PortableValueError, PortableValueRecord};

pub const MAX_RECEIPT_COMPONENTS: usize = 8;
pub const MAX_RECEIPT_CAPABILITIES: usize = 16;
pub const MAX_RECEIPT_RIGHTS: usize = 16;
pub const MAX_RECEIPT_OBJECTS: usize = 16;
pub const MAX_RECEIPT_CAPSULES: usize = 8;
pub const MAX_RECEIPT_EFFECTS: usize = 16;
pub const MAX_RECEIPT_REJECTIONS: usize = 16;
pub const MAX_RECEIPT_CHECKPOINTS: usize = 16;
pub const MAX_RECEIPT_IDENTIFIER_BYTES: usize = 96;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ReceiptError {
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error(transparent)]
    IdentityWire(#[from] IdentityWireError),
    #[error(transparent)]
    PortableValue(#[from] PortableValueError),
    #[error("invalid receipt field `{field}`: {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    #[error("receipt {kind} has {actual} entries; maximum is {maximum}")]
    Limit {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("duplicate or noncanonical receipt {kind}")]
    NonCanonicalOrder { kind: &'static str },
    #[error("receipt commit fence does not equal its Governor identity")]
    CommitFenceMismatch,
    #[error("receipt has no current object matching `{0}`")]
    MissingCurrentObject(String),
    #[error("OWRECEIPT record is malformed: {0}")]
    Malformed(&'static str),
    #[error("OWRECEIPT record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("unsupported OWRECEIPT schema {0}")]
    UnsupportedSchema(u16),
    #[error("unsupported OWRECEIPT signature algorithm {0}")]
    UnsupportedSignatureAlgorithm(u16),
    #[error("receipt signer key `{0}` is not trusted by the supplied resolver")]
    UntrustedSigner(String),
    #[error("receipt signer key ID does not match the resolved public key")]
    SignerKeyIdMismatch,
    #[error("receipt Ed25519 signature is invalid")]
    InvalidSignature,
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ReceiptError> {
    if value.is_empty()
        || value.len() > MAX_RECEIPT_IDENTIFIER_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index != 0 && matches!(byte, b'.' | b'_' | b'-' | b'/' | b'+'))
        })
    {
        return Err(ReceiptError::InvalidField {
            field,
            reason: "must be a bounded lowercase identifier",
        });
    }
    Ok(())
}

fn validate_digest(field: &'static str, digest: &ArtifactId) -> Result<(), ReceiptError> {
    if digest.as_sha256().bytes().all(|byte| byte == b'0') {
        return Err(ReceiptError::InvalidField {
            field,
            reason: "all-zero SHA-256 is reserved",
        });
    }
    Ok(())
}

fn identity_key(record: IdentityWireRecord) -> Result<Vec<u8>, ReceiptError> {
    Ok(record.encode()?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptRight(String);

impl ReceiptRight {
    pub fn new(value: impl Into<String>) -> Result<Self, ReceiptError> {
        let value = value.into();
        validate_identifier("capability right", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ComponentKindV1 {
    Project = 1,
    LiveSystem = 2,
    KernelWorld = 3,
}

impl ComponentKindV1 {
    pub(crate) fn from_u8(value: u8) -> Result<Self, ReceiptError> {
        match value {
            1 => Ok(Self::Project),
            2 => Ok(Self::LiveSystem),
            3 => Ok(Self::KernelWorld),
            _ => Err(ReceiptError::Malformed("unknown component kind")),
        }
    }
}

/// Digest-bound project/live-system/KernelWorld observation. `generation` is
/// zero only for immutable project snapshots; live providers require nonzero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentObservationV1 {
    kind: ComponentKindV1,
    identity: String,
    generation: u64,
    digest: ArtifactId,
}

impl ComponentObservationV1 {
    pub fn new(
        kind: ComponentKindV1,
        identity: impl Into<String>,
        generation: u64,
        digest: ArtifactId,
    ) -> Result<Self, ReceiptError> {
        let identity = identity.into();
        validate_identifier("component identity", &identity)?;
        validate_digest("component digest", &digest)?;
        if kind != ComponentKindV1::Project && generation == 0 {
            return Err(ReceiptError::InvalidField {
                field: "component generation",
                reason: "live-system and KernelWorld generations must be nonzero",
            });
        }
        Ok(Self {
            kind,
            identity,
            generation,
            digest,
        })
    }

    pub fn kind(&self) -> ComponentKindV1 {
        self.kind
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn digest(&self) -> &ArtifactId {
        &self.digest
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptSubjectV1 {
    source: Option<ArtifactId>,
    bundle: Option<ArtifactId>,
    package: Option<ArtifactId>,
    logical_hgraph: Option<ArtifactId>,
    effects: Option<ArtifactId>,
}

impl ReceiptSubjectV1 {
    pub fn new(
        source: Option<ArtifactId>,
        bundle: Option<ArtifactId>,
        package: Option<ArtifactId>,
        logical_hgraph: Option<ArtifactId>,
        effects: Option<ArtifactId>,
    ) -> Result<Self, ReceiptError> {
        if source.is_none()
            && bundle.is_none()
            && package.is_none()
            && logical_hgraph.is_none()
            && effects.is_none()
        {
            return Err(ReceiptError::InvalidField {
                field: "subject",
                reason: "at least one content digest is required",
            });
        }
        for (field, digest) in [
            ("source digest", source.as_ref()),
            ("bundle digest", bundle.as_ref()),
            ("package digest", package.as_ref()),
            ("logical HGraph digest", logical_hgraph.as_ref()),
            ("effects digest", effects.as_ref()),
        ] {
            if let Some(digest) = digest {
                validate_digest(field, digest)?;
            }
        }
        Ok(Self {
            source,
            bundle,
            package,
            logical_hgraph,
            effects,
        })
    }

    pub fn source(&self) -> Option<&ArtifactId> {
        self.source.as_ref()
    }
    pub fn bundle(&self) -> Option<&ArtifactId> {
        self.bundle.as_ref()
    }
    pub fn package(&self) -> Option<&ArtifactId> {
        self.package.as_ref()
    }
    pub fn logical_hgraph(&self) -> Option<&ArtifactId> {
        self.logical_hgraph.as_ref()
    }
    pub fn effects(&self) -> Option<&ArtifactId> {
        self.effects.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptPlacementV1 {
    node: NodeIdentity,
    domain: DomainIdentity,
    process: Option<ProcessIdentity>,
    rejected: Vec<PlacementRejectionV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementRejectionV1 {
    node: NodeIdentity,
    reason: String,
}

impl PlacementRejectionV1 {
    pub fn new(node: NodeIdentity, reason: impl Into<String>) -> Result<Self, ReceiptError> {
        let reason = reason.into();
        validate_identifier("placement rejection reason", &reason)?;
        Ok(Self { node, reason })
    }
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl ReceiptPlacementV1 {
    pub fn new(
        node: NodeIdentity,
        domain: DomainIdentity,
        process: Option<ProcessIdentity>,
        mut rejected: Vec<PlacementRejectionV1>,
    ) -> Result<Self, ReceiptError> {
        if domain.node() != &node || process.as_ref().is_some_and(|p| p.domain() != &domain) {
            return Err(ReceiptError::InvalidField {
                field: "placement",
                reason: "domain/process must be nested beneath the selected node",
            });
        }
        if rejected.len() > MAX_RECEIPT_REJECTIONS {
            return Err(ReceiptError::Limit {
                kind: "placement rejections",
                actual: rejected.len(),
                maximum: MAX_RECEIPT_REJECTIONS,
            });
        }
        rejected.sort_by(|a, b| {
            (a.node.world(), a.node.node(), a.node.generation()).cmp(&(
                b.node.world(),
                b.node.node(),
                b.node.generation(),
            ))
        });
        if rejected.iter().any(|entry| entry.node == node)
            || rejected.windows(2).any(|pair| pair[0].node == pair[1].node)
        {
            return Err(ReceiptError::NonCanonicalOrder {
                kind: "placement rejection",
            });
        }
        Ok(Self {
            node,
            domain,
            process,
            rejected,
        })
    }
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }
    pub fn domain(&self) -> &DomainIdentity {
        &self.domain
    }
    pub fn process(&self) -> Option<&ProcessIdentity> {
        self.process.as_ref()
    }
    pub fn rejected(&self) -> &[PlacementRejectionV1] {
        &self.rejected
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptContextV1 {
    receipt: ReceiptIdentity,
    world: WorldIdentity,
    governor: GovernorIdentity,
    attempt: AttemptIdentity,
    placement: ReceiptPlacementV1,
}

impl ReceiptContextV1 {
    pub fn new(
        receipt: ReceiptIdentity,
        world: WorldIdentity,
        governor: GovernorIdentity,
        attempt: AttemptIdentity,
        placement: ReceiptPlacementV1,
    ) -> Result<Self, ReceiptError> {
        let world_id = world.world();
        if receipt.world() != world_id
            || governor.world() != &world
            || attempt.world() != world_id
            || placement.node().world() != world_id
        {
            return Err(ReceiptError::InvalidField {
                field: "context",
                reason: "all identities must name the same exact World",
            });
        }
        Ok(Self {
            receipt,
            world,
            governor,
            attempt,
            placement,
        })
    }
    pub fn receipt(&self) -> &ReceiptIdentity {
        &self.receipt
    }
    pub fn world(&self) -> &WorldIdentity {
        &self.world
    }
    pub fn governor(&self) -> &GovernorIdentity {
        &self.governor
    }
    pub fn attempt(&self) -> &AttemptIdentity {
        &self.attempt
    }
    pub fn placement(&self) -> &ReceiptPlacementV1 {
        &self.placement
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityObservationV1 {
    capability: CapabilityIdentity,
    rights: Vec<ReceiptRight>,
}

impl CapabilityObservationV1 {
    pub fn new(
        capability: CapabilityIdentity,
        mut rights: Vec<ReceiptRight>,
    ) -> Result<Self, ReceiptError> {
        if rights.is_empty() || rights.len() > MAX_RECEIPT_RIGHTS {
            return Err(ReceiptError::Limit {
                kind: "capability rights",
                actual: rights.len(),
                maximum: MAX_RECEIPT_RIGHTS,
            });
        }
        rights.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        if rights.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ReceiptError::NonCanonicalOrder {
                kind: "capability right",
            });
        }
        Ok(Self { capability, rights })
    }
    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }
    pub fn rights(&self) -> &[ReceiptRight] {
        &self.rights
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ObjectRoleV1 {
    Input = 1,
    Output = 2,
    Replica = 3,
    Transfer = 4,
}

impl ObjectRoleV1 {
    pub(crate) fn from_u8(value: u8) -> Result<Self, ReceiptError> {
        match value {
            1 => Ok(Self::Input),
            2 => Ok(Self::Output),
            3 => Ok(Self::Replica),
            4 => Ok(Self::Transfer),
            _ => Err(ReceiptError::Malformed("unknown object role")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectObservationV1 {
    object: ObjectIdentity,
    role: ObjectRoleV1,
    content: ArtifactId,
    bytes_len: u64,
}

impl ObjectObservationV1 {
    pub fn new(
        object: ObjectIdentity,
        role: ObjectRoleV1,
        content: ArtifactId,
        bytes_len: u64,
    ) -> Result<Self, ReceiptError> {
        validate_digest("object content", &content)?;
        Ok(Self {
            object,
            role,
            content,
            bytes_len,
        })
    }
    pub fn object(&self) -> &ObjectIdentity {
        &self.object
    }
    pub fn role(&self) -> ObjectRoleV1 {
        self.role
    }
    pub fn content(&self) -> &ArtifactId {
        &self.content
    }
    pub fn bytes_len(&self) -> u64 {
        self.bytes_len
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleObservationV1 {
    digest: ArtifactId,
    evaluator: String,
    affinity: NodeIdentity,
}

impl CapsuleObservationV1 {
    pub fn new(
        digest: ArtifactId,
        evaluator: impl Into<String>,
        affinity: NodeIdentity,
    ) -> Result<Self, ReceiptError> {
        let evaluator = evaluator.into();
        validate_digest("capsule digest", &digest)?;
        validate_identifier("capsule evaluator", &evaluator)?;
        Ok(Self {
            digest,
            evaluator,
            affinity,
        })
    }
    pub fn digest(&self) -> &ArtifactId {
        &self.digest
    }
    pub fn evaluator(&self) -> &str {
        &self.evaluator
    }
    pub fn affinity(&self) -> &NodeIdentity {
        &self.affinity
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectObservationV1 {
    resource: ResourceIdentity,
    before: ArtifactId,
    after: ArtifactId,
}

impl EffectObservationV1 {
    pub fn new(
        resource: ResourceIdentity,
        before: ArtifactId,
        after: ArtifactId,
    ) -> Result<Self, ReceiptError> {
        validate_digest("effect before", &before)?;
        validate_digest("effect after", &after)?;
        Ok(Self {
            resource,
            before,
            after,
        })
    }
    pub fn resource(&self) -> &ResourceIdentity {
        &self.resource
    }
    pub fn before(&self) -> &ArtifactId {
        &self.before
    }
    pub fn after(&self) -> &ArtifactId {
        &self.after
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointObservationV1 {
    checkpoint: CheckpointIdentity,
    state: ArtifactId,
    recovered: bool,
}

impl CheckpointObservationV1 {
    pub fn new(
        checkpoint: CheckpointIdentity,
        state: ArtifactId,
        recovered: bool,
    ) -> Result<Self, ReceiptError> {
        validate_digest("checkpoint state", &state)?;
        Ok(Self {
            checkpoint,
            state,
            recovered,
        })
    }
    pub fn checkpoint(&self) -> &CheckpointIdentity {
        &self.checkpoint
    }
    pub fn state(&self) -> &ArtifactId {
        &self.state
    }
    pub fn recovered(&self) -> bool {
        self.recovered
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptTerminalV1 {
    Success(PortableValueRecord),
    Failure {
        code: String,
        detail_digest: ArtifactId,
    },
    Cancelled,
    DeadlineExceeded,
    WorldFailed,
    WorldStopped,
}

impl ReceiptTerminalV1 {
    pub fn failure(
        code: impl Into<String>,
        detail_digest: ArtifactId,
    ) -> Result<Self, ReceiptError> {
        let code = code.into();
        validate_identifier("terminal failure code", &code)?;
        validate_digest("terminal failure detail", &detail_digest)?;
        Ok(Self::Failure {
            code,
            detail_digest,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptCommitFenceV1 {
    Uncommitted,
    Governed(GovernorIdentity),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceObservationV1 {
    gate: String,
    transcript: ArtifactId,
}

impl EvidenceObservationV1 {
    pub fn new(gate: impl Into<String>, transcript: ArtifactId) -> Result<Self, ReceiptError> {
        let gate = gate.into();
        validate_identifier("evidence gate", &gate)?;
        validate_digest("evidence transcript", &transcript)?;
        Ok(Self { gate, transcript })
    }
    pub fn gate(&self) -> &str {
        &self.gate
    }
    pub fn transcript(&self) -> &ArtifactId {
        &self.transcript
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionReceiptV1 {
    context: ReceiptContextV1,
    subject: ReceiptSubjectV1,
    components: Vec<ComponentObservationV1>,
    capabilities: Vec<CapabilityObservationV1>,
    objects: Vec<ObjectObservationV1>,
    capsules: Vec<CapsuleObservationV1>,
    effects: Vec<EffectObservationV1>,
    checkpoints: Vec<CheckpointObservationV1>,
    terminal: ReceiptTerminalV1,
    commit: ReceiptCommitFenceV1,
    evidence: Option<EvidenceObservationV1>,
}

impl ExecutionReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        context: ReceiptContextV1,
        subject: ReceiptSubjectV1,
        mut components: Vec<ComponentObservationV1>,
        mut capabilities: Vec<CapabilityObservationV1>,
        mut objects: Vec<ObjectObservationV1>,
        mut capsules: Vec<CapsuleObservationV1>,
        mut effects: Vec<EffectObservationV1>,
        mut checkpoints: Vec<CheckpointObservationV1>,
        terminal: ReceiptTerminalV1,
        commit: ReceiptCommitFenceV1,
        evidence: Option<EvidenceObservationV1>,
    ) -> Result<Self, ReceiptError> {
        enforce_limit("components", components.len(), MAX_RECEIPT_COMPONENTS)?;
        enforce_limit(
            "capability observations",
            capabilities.len(),
            MAX_RECEIPT_CAPABILITIES,
        )?;
        enforce_limit("object observations", objects.len(), MAX_RECEIPT_OBJECTS)?;
        enforce_limit("capsule observations", capsules.len(), MAX_RECEIPT_CAPSULES)?;
        enforce_limit("effect observations", effects.len(), MAX_RECEIPT_EFFECTS)?;
        enforce_limit(
            "checkpoint observations",
            checkpoints.len(),
            MAX_RECEIPT_CHECKPOINTS,
        )?;

        let world = context.world().world();
        if capabilities
            .iter()
            .any(|entry| entry.capability().world() != world)
            || objects.iter().any(|entry| entry.object().world() != world)
            || capsules
                .iter()
                .any(|entry| entry.affinity().world() != world)
            || effects
                .iter()
                .any(|entry| entry.resource().owner().world() != world)
            || checkpoints
                .iter()
                .any(|entry| entry.checkpoint().attempt().world() != world)
            || context
                .placement()
                .rejected()
                .iter()
                .any(|entry| entry.node().world() != world)
        {
            return Err(ReceiptError::InvalidField {
                field: "observations",
                reason: "every observation must name the receipt World",
            });
        }
        for checkpoint in &checkpoints {
            if checkpoint.checkpoint().attempt() != context.attempt() {
                return Err(ReceiptError::InvalidField {
                    field: "checkpoint",
                    reason: "checkpoint must belong to the receipt attempt",
                });
            }
        }
        match &terminal {
            ReceiptTerminalV1::Success(value) => value.validate()?,
            ReceiptTerminalV1::Failure {
                code,
                detail_digest,
            } => {
                validate_identifier("terminal failure code", code)?;
                validate_digest("terminal failure detail", detail_digest)?;
            }
            ReceiptTerminalV1::Cancelled
            | ReceiptTerminalV1::DeadlineExceeded
            | ReceiptTerminalV1::WorldFailed
            | ReceiptTerminalV1::WorldStopped => {}
        }
        if let ReceiptCommitFenceV1::Governed(fence) = &commit {
            if fence != context.governor() {
                return Err(ReceiptError::CommitFenceMismatch);
            }
        }

        components.sort_by(|a, b| {
            (a.kind, a.identity.as_bytes(), a.generation).cmp(&(
                b.kind,
                b.identity.as_bytes(),
                b.generation,
            ))
        });
        reject_adjacent_duplicates(&components, "component", |a, b| {
            a.kind == b.kind && a.identity == b.identity && a.generation == b.generation
        })?;

        sort_identity_entries(&mut capabilities, |entry| {
            identity_key(IdentityWireRecord::Capability(entry.capability.clone()))
        })?;
        sort_identity_entries(&mut objects, |entry| {
            identity_key(IdentityWireRecord::Object(entry.object.clone()))
        })?;
        capsules.sort_by(|a, b| {
            (a.digest.as_sha256(), a.evaluator.as_bytes())
                .cmp(&(b.digest.as_sha256(), b.evaluator.as_bytes()))
        });
        reject_adjacent_duplicates(&capsules, "capsule", |a, b| {
            a.digest == b.digest && a.evaluator == b.evaluator
        })?;
        sort_identity_entries(&mut effects, |entry| {
            identity_key(IdentityWireRecord::Resource(entry.resource.clone()))
        })?;
        sort_identity_entries(&mut checkpoints, |entry| {
            identity_key(IdentityWireRecord::Checkpoint(entry.checkpoint.clone()))
        })?;

        Ok(Self {
            context,
            subject,
            components,
            capabilities,
            objects,
            capsules,
            effects,
            checkpoints,
            terminal,
            commit,
            evidence,
        })
    }

    pub fn context(&self) -> &ReceiptContextV1 {
        &self.context
    }
    pub fn subject(&self) -> &ReceiptSubjectV1 {
        &self.subject
    }
    pub fn components(&self) -> &[ComponentObservationV1] {
        &self.components
    }
    pub fn capabilities(&self) -> &[CapabilityObservationV1] {
        &self.capabilities
    }
    pub fn objects(&self) -> &[ObjectObservationV1] {
        &self.objects
    }
    pub fn capsules(&self) -> &[CapsuleObservationV1] {
        &self.capsules
    }
    pub fn effects(&self) -> &[EffectObservationV1] {
        &self.effects
    }
    pub fn checkpoints(&self) -> &[CheckpointObservationV1] {
        &self.checkpoints
    }
    pub fn terminal(&self) -> &ReceiptTerminalV1 {
        &self.terminal
    }
    pub fn commit(&self) -> &ReceiptCommitFenceV1 {
        &self.commit
    }
    pub fn evidence(&self) -> Option<&EvidenceObservationV1> {
        self.evidence.as_ref()
    }

    pub fn validate_current(&self, current: &ReceiptCurrentStateV1) -> Result<(), ReceiptError> {
        current.world.require_current(self.context.world())?;
        current.governor.require_current(self.context.governor())?;
        current
            .node
            .require_current(self.context.placement().node())?;
        current
            .domain
            .require_current(self.context.placement().domain())?;
        match (&current.process, self.context.placement().process()) {
            (Some(current), Some(reference)) => current.require_current(reference)?,
            (None, None) => {}
            _ => {
                return Err(ReceiptError::InvalidField {
                    field: "current process",
                    reason: "process presence does not match the receipt",
                })
            }
        }
        current.attempt.require_current(self.context.attempt())?;
        for observation in &self.objects {
            let presented = observation.object();
            let found = current.objects.iter().find(|candidate| {
                candidate.world() == presented.world() && candidate.object() == presented.object()
            });
            let found =
                found.ok_or_else(|| ReceiptError::MissingCurrentObject(presented.to_string()))?;
            found.require_current(presented)?;
        }
        if let ReceiptCommitFenceV1::Governed(fence) = &self.commit {
            current.governor.require_current(fence)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptCurrentStateV1 {
    world: WorldIdentity,
    governor: GovernorIdentity,
    node: NodeIdentity,
    domain: DomainIdentity,
    process: Option<ProcessIdentity>,
    attempt: AttemptIdentity,
    objects: Vec<ObjectIdentity>,
}

impl ReceiptCurrentStateV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        world: WorldIdentity,
        governor: GovernorIdentity,
        node: NodeIdentity,
        domain: DomainIdentity,
        process: Option<ProcessIdentity>,
        attempt: AttemptIdentity,
        mut objects: Vec<ObjectIdentity>,
    ) -> Result<Self, ReceiptError> {
        enforce_limit("current objects", objects.len(), MAX_RECEIPT_OBJECTS)?;
        if governor.world() != &world
            || node.world() != world.world()
            || domain.node() != &node
            || process
                .as_ref()
                .is_some_and(|entry| entry.domain() != &domain)
            || attempt.world() != world.world()
            || objects.iter().any(|entry| entry.world() != world.world())
        {
            return Err(ReceiptError::InvalidField {
                field: "current state",
                reason: "current identities must form one exact World hierarchy",
            });
        }
        objects.sort_by(|left, right| {
            (left.world(), left.object(), left.version()).cmp(&(
                right.world(),
                right.object(),
                right.version(),
            ))
        });
        if objects
            .windows(2)
            .any(|pair| pair[0].world() == pair[1].world() && pair[0].object() == pair[1].object())
        {
            return Err(ReceiptError::NonCanonicalOrder {
                kind: "current object",
            });
        }
        Ok(Self {
            world,
            governor,
            node,
            domain,
            process,
            attempt,
            objects,
        })
    }

    pub fn world(&self) -> &WorldIdentity {
        &self.world
    }
    pub fn governor(&self) -> &GovernorIdentity {
        &self.governor
    }
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }
    pub fn domain(&self) -> &DomainIdentity {
        &self.domain
    }
    pub fn process(&self) -> Option<&ProcessIdentity> {
        self.process.as_ref()
    }
    pub fn attempt(&self) -> &AttemptIdentity {
        &self.attempt
    }
    pub fn objects(&self) -> &[ObjectIdentity] {
        &self.objects
    }
}

fn enforce_limit(kind: &'static str, actual: usize, maximum: usize) -> Result<(), ReceiptError> {
    if actual > maximum {
        Err(ReceiptError::Limit {
            kind,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn reject_adjacent_duplicates<T>(
    entries: &[T],
    kind: &'static str,
    same: impl Fn(&T, &T) -> bool,
) -> Result<(), ReceiptError> {
    if entries.windows(2).any(|pair| same(&pair[0], &pair[1])) {
        Err(ReceiptError::NonCanonicalOrder { kind })
    } else {
        Ok(())
    }
}

fn sort_identity_entries<T>(
    entries: &mut [T],
    key: impl Fn(&T) -> Result<Vec<u8>, ReceiptError>,
) -> Result<(), ReceiptError> {
    let mut keyed = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| Ok((key(entry)?, index)))
        .collect::<Result<Vec<_>, ReceiptError>>()?;
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    if keyed.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(ReceiptError::NonCanonicalOrder {
            kind: "identity observation",
        });
    }
    // Stable insertion based on precomputed canonical identity bytes.
    entries.sort_by(|a, b| match (key(a), key(b)) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => Ordering::Equal,
    });
    Ok(())
}
