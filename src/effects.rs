//! Shared semantic effect model for O execution planning and scheduling.
//!
//! Effect summaries are derived before executable HGraph edges are built. They
//! therefore describe semantic dependencies, not merely worker-pool hints.
//! Unknown hosted work is conservatively connected to [`ResourceKey::HostWorld`]
//! and evaluator-local mutable state is represented separately.

use std::collections::BTreeSet;
use std::fmt;

use crate::ir::{ExecutionMode, PlanNodeId, PlanNodeKind};
use crate::world::identity::{
    ArtifactPublicationIdentity, CapabilityIdentity, DomainIdentity, GovernorIdentity,
    NodeIdentity, ObjectIdentity, ProcessIdentity, ResourceIdentity, TaskAttemptIdentity,
    WorldIdentity,
};

/// Stable identity for a persistent evaluator resource.
///
/// This intentionally excludes a process generation. The current process
/// registry does not expose a real generation, so including a constant zero
/// would falsely claim generation-sensitive identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorResourceId {
    pub canonical_language: String,
    pub environment: u32,
}

impl ActorResourceId {
    pub fn new(canonical_language: impl Into<String>, environment: u32) -> Self {
        Self {
            canonical_language: canonical_language.into(),
            environment,
        }
    }
}

impl fmt::Display for ActorResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]", self.canonical_language, self.environment)
    }
}

/// A semantic resource whose state may be consumed or produced by an operation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ResourceKey {
    /// Conservative umbrella for host-observable state not modeled precisely.
    HostWorld,
    /// One exact governed World epoch. This is not ambient host authority.
    WorldState(WorldIdentity),
    /// One exact descriptive Governor position for one World epoch.
    GovernorState(GovernorIdentity),
    /// One exact governed node generation.
    NodeState(NodeIdentity),
    /// One exact governed execution-domain generation.
    DomainState(DomainIdentity),
    /// One exact governed process generation.
    ProcessState(ProcessIdentity),
    /// The canonical generic state for one resource beneath an exact owner
    /// generation. Device and accelerator views also alias this key for
    /// scheduling, so they cannot bypass generic resource dependencies.
    GovernedResource(ResourceIdentity),
    /// One exact immutable-object version in a World.
    ObjectState(ObjectIdentity),
    /// Descriptive lifecycle state for an inert capability identifier.
    ///
    /// This key does not carry a grant or authorize an operation.
    CapabilityState(CapabilityIdentity),
    /// One exact World-wide namespace epoch.
    NamespaceState(WorldIdentity),
    /// One exact task-attempt generation.
    TaskState(TaskAttemptIdentity),
    /// Publication state for one content-addressed artifact in one World epoch.
    ArtifactState(ArtifactPublicationIdentity),
    /// One device resource beneath an exact governed owner generation.
    DeviceState(ResourceIdentity),
    /// One accelerator resource beneath an exact governed owner generation.
    AcceleratorState(ResourceIdentity),
    /// Mutable state owned by the O evaluator itself.
    EvaluatorState,
    /// One O-level scope binding.
    ScopeBinding(String),
    ProjectPath(String),
    HostPath(String),
    EnvVar(String),
    Stdio,
    Network(String),
    NetworkUnknown,
    Service(String),
    ActorState(ActorResourceId),
}

/// Stable semantic class for a governed [`ResourceKey`].
///
/// This is hosted planner vocabulary. It is not a wire tag, capability grant,
/// authoritative inventory record, or proof that an effect was mediated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum GovernedResourceKind {
    World,
    Governor,
    Node,
    Domain,
    Process,
    Resource,
    Object,
    Capability,
    Namespace,
    Task,
    Artifact,
    Device,
    Accelerator,
}

impl GovernedResourceKind {
    pub const ALL: [Self; 13] = [
        Self::World,
        Self::Governor,
        Self::Node,
        Self::Domain,
        Self::Process,
        Self::Resource,
        Self::Object,
        Self::Capability,
        Self::Namespace,
        Self::Task,
        Self::Artifact,
        Self::Device,
        Self::Accelerator,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Governor => "governor",
            Self::Node => "node",
            Self::Domain => "domain",
            Self::Process => "process",
            Self::Resource => "resource",
            Self::Object => "object",
            Self::Capability => "capability",
            Self::Namespace => "namespace",
            Self::Task => "task",
            Self::Artifact => "artifact",
            Self::Device => "device",
            Self::Accelerator => "accelerator",
        }
    }
}

impl fmt::Display for GovernedResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl ResourceKey {
    /// Whether this key names host-observable state covered by `HostWorld`.
    pub fn is_host_resource(&self) -> bool {
        matches!(
            self,
            Self::HostWorld
                | Self::ProjectPath(_)
                | Self::HostPath(_)
                | Self::EnvVar(_)
                | Self::Stdio
                | Self::Network(_)
                | Self::NetworkUnknown
                | Self::Service(_)
        )
    }

    /// Whether this key names state described by explicit World vocabulary.
    ///
    /// Governed keys deliberately do not alias [`Self::HostWorld`]. They are
    /// descriptive vocabulary, not proof of mediation or authority. Current
    /// source `reads=`/`writes=` declarations cannot construct them; a later
    /// trusted lowering must keep `HostWorld` until mediation is actually
    /// established.
    pub fn is_governed_resource(&self) -> bool {
        self.governed_kind().is_some()
    }

    /// Return the precise governed class without collapsing it into
    /// [`Self::HostWorld`].
    pub fn governed_kind(&self) -> Option<GovernedResourceKind> {
        match self {
            Self::WorldState(_) => Some(GovernedResourceKind::World),
            Self::GovernorState(_) => Some(GovernedResourceKind::Governor),
            Self::NodeState(_) => Some(GovernedResourceKind::Node),
            Self::DomainState(_) => Some(GovernedResourceKind::Domain),
            Self::ProcessState(_) => Some(GovernedResourceKind::Process),
            Self::GovernedResource(_) => Some(GovernedResourceKind::Resource),
            Self::ObjectState(_) => Some(GovernedResourceKind::Object),
            Self::CapabilityState(_) => Some(GovernedResourceKind::Capability),
            Self::NamespaceState(_) => Some(GovernedResourceKind::Namespace),
            Self::TaskState(_) => Some(GovernedResourceKind::Task),
            Self::ArtifactState(_) => Some(GovernedResourceKind::Artifact),
            Self::DeviceState(_) => Some(GovernedResourceKind::Device),
            Self::AcceleratorState(_) => Some(GovernedResourceKind::Accelerator),
            Self::HostWorld
            | Self::EvaluatorState
            | Self::ScopeBinding(_)
            | Self::ProjectPath(_)
            | Self::HostPath(_)
            | Self::EnvVar(_)
            | Self::Stdio
            | Self::Network(_)
            | Self::NetworkUnknown
            | Self::Service(_)
            | Self::ActorState(_) => None,
        }
    }
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostWorld => f.write_str("HostWorld"),
            Self::WorldState(world) => write!(f, "world-state:{world}"),
            Self::GovernorState(governor) => write!(f, "governor-state:{governor}"),
            Self::NodeState(node) => write!(f, "node-state:{node}"),
            Self::DomainState(domain) => write!(f, "domain-state:{domain}"),
            Self::ProcessState(process) => write!(f, "process-state:{process}"),
            Self::GovernedResource(resource) => write!(f, "governed-resource:{resource}"),
            Self::ObjectState(object) => write!(f, "object-state:{object}"),
            Self::CapabilityState(capability) => write!(f, "capability-state:{capability}"),
            Self::NamespaceState(world) => write!(f, "namespace-state:{world}"),
            Self::TaskState(task) => write!(f, "task-state:{task}"),
            Self::ArtifactState(artifact) => write!(f, "artifact-state:{artifact}"),
            Self::DeviceState(device) => write!(f, "device-state:{device}"),
            Self::AcceleratorState(accelerator) => {
                write!(f, "accelerator-state:{accelerator}")
            }
            Self::EvaluatorState => f.write_str("EvaluatorState"),
            Self::ScopeBinding(name) => write!(f, "scope:{name}"),
            Self::ProjectPath(path) => write!(f, "project:{path}"),
            Self::HostPath(path) => write!(f, "host:{path}"),
            Self::EnvVar(name) => write!(f, "env:{name}"),
            Self::Stdio => f.write_str("stdio"),
            Self::Network(endpoint) => write!(f, "network:{endpoint}"),
            Self::NetworkUnknown => f.write_str("network:*"),
            Self::Service(name) => write!(f, "service:{name}"),
            Self::ActorState(actor) => write!(f, "actor:{actor}"),
        }
    }
}

/// Provenance strength of an effect classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectConfidence {
    Verified,
    Conservative,
    UserDeclared,
}

/// Whether execution may fail after all graph inputs have materialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fallibility {
    Infallible,
    MayFail,
}

/// Policy controlling whether declarations may weaken a derived summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectTrustPolicy {
    /// Source declarations may add constraints but may not upgrade unverified
    /// work to pure, deterministic, or infallible.
    Strict,
}

/// Conservative semantic effects of one operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectSummary {
    pub deterministic: bool,
    pub fallibility: Fallibility,
    pub reads: BTreeSet<ResourceKey>,
    pub writes: BTreeSet<ResourceKey>,
    pub actor_state: Option<ActorResourceId>,
    /// The operation's complete resource footprint is not statically known.
    pub unknown: bool,
    pub network: bool,
    pub spawn: bool,
    pub clock: bool,
    pub confidence: EffectConfidence,
}

impl EffectSummary {
    /// Verified, deterministic, infallible, resource-free work.
    pub fn pure() -> Self {
        Self {
            deterministic: true,
            fallibility: Fallibility::Infallible,
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            actor_state: None,
            unknown: false,
            network: false,
            spawn: false,
            clock: false,
            confidence: EffectConfidence::Verified,
        }
    }

    /// Unknown hosted work. Unknown always reads and writes `HostWorld`.
    pub fn unknown() -> Self {
        let mut reads = BTreeSet::new();
        reads.insert(ResourceKey::HostWorld);
        let mut writes = BTreeSet::new();
        writes.insert(ResourceKey::HostWorld);
        Self {
            deterministic: false,
            fallibility: Fallibility::MayFail,
            reads,
            writes,
            actor_state: None,
            unknown: true,
            network: false,
            spawn: false,
            clock: false,
            confidence: EffectConfidence::Conservative,
        }
    }

    /// Unknown hosted work that may also inspect or mutate evaluator-local state.
    pub fn conservative_evaluator() -> Self {
        let mut summary = Self::unknown();
        summary.reads.insert(ResourceKey::EvaluatorState);
        summary.writes.insert(ResourceKey::EvaluatorState);
        summary
    }

    /// Attach a persistent actor-state transition.
    pub fn with_actor_state(mut self, actor: ActorResourceId) -> Self {
        if let Some(previous) = self.actor_state.replace(actor.clone()) {
            let previous = ResourceKey::ActorState(previous);
            self.reads.remove(&previous);
            self.writes.remove(&previous);
        }
        let resource = ResourceKey::ActorState(actor);
        self.reads.insert(resource.clone());
        self.writes.insert(resource);
        self
    }

    /// Union of read and write resource identities.
    pub fn resource_union(&self) -> BTreeSet<ResourceKey> {
        let mut resources = self.expanded_reads();
        resources.extend(self.expanded_writes());
        resources
    }

    /// Read footprint after applying scheduler-visible resource aliases.
    pub fn expanded_reads(&self) -> BTreeSet<ResourceKey> {
        let mut resources = self.reads.clone();
        expand_resource_aliases(&mut resources);
        resources
    }

    /// Write footprint after applying scheduler-visible resource aliases.
    pub fn expanded_writes(&self) -> BTreeSet<ResourceKey> {
        let mut resources = self.writes.clone();
        expand_resource_aliases(&mut resources);
        resources
    }

    /// Canonical read/write leases consumed by executable HGraph lowering.
    ///
    /// This preserves access mode (unlike `resource_union`) and expands every
    /// scheduler-visible alias. Precise host resources share a read lease on
    /// `HostWorld`, while an explicit ambient `HostWorld` access is exclusive.
    /// The latter matches the fail-closed compatibility predicate: unknown
    /// ambient work cannot overlap even a precise host read, while two precise
    /// disjoint host resources do not become aliases merely because they are
    /// governed by the same umbrella.
    pub fn scheduling_accesses(&self) -> (BTreeSet<ResourceKey>, BTreeSet<ResourceKey>) {
        let mut reads = self.expanded_reads();
        let mut writes = self.expanded_writes();
        let touches_ambient_host =
            reads.contains(&ResourceKey::HostWorld) || writes.contains(&ResourceKey::HostWorld);
        let touches_precise_host = reads
            .iter()
            .chain(writes.iter())
            .any(|resource| resource.is_host_resource() && *resource != ResourceKey::HostWorld);
        if touches_precise_host {
            reads.insert(ResourceKey::HostWorld);
        }
        if touches_ambient_host {
            // Ambient HostWorld is deliberately an exclusive lease. Unknown
            // hosted work already writes it; promoting an explicitly ambient
            // read-only spelling keeps graph topology aligned with
            // `conflicts_with` without making precise host paths exclusive.
            writes.insert(ResourceKey::HostWorld);
        }
        (reads, writes)
    }

    /// Compatibility-friendly alias for [`Self::resource_union`].
    pub fn resources(&self) -> BTreeSet<ResourceKey> {
        self.resource_union()
    }

    /// All semantic resources accessed by this operation.
    ///
    /// This name is used by executable-graph construction; retain the older
    /// aliases while downstream callers migrate to the shared model.
    pub fn accessed_resources(&self) -> BTreeSet<ResourceKey> {
        self.resource_union()
    }

    /// Exact worker-pool eligibility predicate for semantic classification.
    pub fn is_verified_pure_infallible(&self) -> bool {
        self.confidence == EffectConfidence::Verified
            && self.deterministic
            && self.fallibility == Fallibility::Infallible
            && !self.unknown
            && self.actor_state.is_none()
            && self.reads.is_empty()
            && self.writes.is_empty()
            && !self.network
            && !self.spawn
            && !self.clock
    }

    /// Compatibility conflict predicate used by analysis and transition code.
    /// Production readiness should ultimately be derived from resource nodes.
    pub fn conflicts_with(&self, other: &EffectSummary) -> bool {
        if let (Some(left), Some(right)) = (&self.actor_state, &other.actor_state) {
            if left == right {
                return true;
            }
        }

        let (self_reads, self_writes) = self.scheduling_accesses();
        let (other_reads, other_writes) = other.scheduling_accesses();

        resource_conflict(&self_writes, &other_writes)
            || resource_conflict(&self_writes, &other_reads)
            || resource_conflict(&other_writes, &self_reads)
    }
}

fn expand_resource_aliases(resources: &mut BTreeSet<ResourceKey>) {
    // Until endpoint-level network independence is verified, every exact
    // endpoint also participates in the shared unknown-network state. This
    // makes `network:*` a real alias in the executable graph instead of a
    // display-only spelling that could bypass an exact `network:...` key.
    if resources
        .iter()
        .any(|resource| matches!(resource, ResourceKey::Network(_)))
    {
        resources.insert(ResourceKey::NetworkUnknown);
    }

    // A typed device or accelerator view is more precise than the canonical
    // generic resource state, but it is never independent from that state.
    // Adding the generic alias here makes both the conflict predicate and the
    // executable HGraph share the same version chain.
    let governed_aliases = resources
        .iter()
        .filter_map(|resource| match resource {
            ResourceKey::DeviceState(resource) | ResourceKey::AcceleratorState(resource) => {
                Some(ResourceKey::GovernedResource(resource.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    resources.extend(governed_aliases);
}

fn resource_conflict(left: &BTreeSet<ResourceKey>, right: &BTreeSet<ResourceKey>) -> bool {
    left.iter().any(|resource| right.contains(resource))
        || (touches_network(left) && touches_network(right))
}

fn touches_network(resources: &BTreeSet<ResourceKey>) -> bool {
    resources.iter().any(|resource| {
        matches!(
            resource,
            ResourceKey::Network(_) | ResourceKey::NetworkUnknown
        )
    })
}

/// Parsed source-level constraints on an operation's derived effect summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectDeclaration {
    pub purity: Option<DeclaredPurity>,
    pub reads: BTreeSet<ResourceKey>,
    pub writes: BTreeSet<ResourceKey>,
    pub serial_host: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclaredPurity {
    Pure,
    Unknown,
}

impl EffectDeclaration {
    pub fn is_empty(&self) -> bool {
        self.purity.is_none()
            && self.reads.is_empty()
            && self.writes.is_empty()
            && !self.serial_host
    }

    pub fn recognizes_entry(entry: &str) -> bool {
        ["effects=", "reads=", "writes=", "serial="]
            .iter()
            .any(|prefix| entry.starts_with(prefix))
    }

    /// Parse effect-related entries from a comma-separated block attribute.
    /// Unrelated attributes such as `lazy`, `defer`, authority names, and
    /// `cap=name` are deliberately left to the block-options parser.
    pub fn parse(attr: Option<&str>) -> Result<Self, String> {
        let mut declaration = Self::default();
        let Some(attr) = attr else {
            return Ok(declaration);
        };

        let mut seen_effects = false;
        let mut seen_reads = false;
        let mut seen_writes = false;
        let mut seen_serial = false;

        for entry in attr.split(',').map(str::trim) {
            if entry.is_empty() {
                return Err("empty block effect attribute".to_string());
            }
            if let Some(value) = entry.strip_prefix("effects=") {
                if seen_effects {
                    return Err("duplicate `effects=` declaration".to_string());
                }
                seen_effects = true;
                declaration.purity = Some(match value {
                    "pure" => DeclaredPurity::Pure,
                    "unknown" => DeclaredPurity::Unknown,
                    _ => {
                        return Err(format!(
                            "invalid effect classification `{value}`; expected `pure` or `unknown`"
                        ))
                    }
                });
            } else if let Some(value) = entry.strip_prefix("reads=") {
                if seen_reads {
                    return Err("duplicate `reads=` declaration".to_string());
                }
                seen_reads = true;
                declaration.reads = parse_resources(value)?;
            } else if let Some(value) = entry.strip_prefix("writes=") {
                if seen_writes {
                    return Err("duplicate `writes=` declaration".to_string());
                }
                seen_writes = true;
                declaration.writes = parse_resources(value)?;
            } else if let Some(value) = entry.strip_prefix("serial=") {
                if seen_serial {
                    return Err("duplicate `serial=` declaration".to_string());
                }
                seen_serial = true;
                if value != "host" {
                    return Err(format!(
                        "invalid serialization domain `{value}`; expected `host`"
                    ));
                }
                declaration.serial_host = true;
            }
        }

        Ok(declaration)
    }

    /// Apply only conservative changes under the selected trust policy.
    pub fn apply_checked(
        &self,
        mut base: EffectSummary,
        policy: EffectTrustPolicy,
    ) -> Result<EffectSummary, String> {
        match policy {
            EffectTrustPolicy::Strict => {}
        }

        let base_was_verified_pure = base.is_verified_pure_infallible();
        match self.purity {
            Some(DeclaredPurity::Pure) if !base_was_verified_pure => {
                return Err(
                    "`effects=pure` cannot upgrade an unverified, effectful, or fallible operation"
                        .to_string(),
                );
            }
            Some(DeclaredPurity::Pure) | None => {}
            Some(DeclaredPurity::Unknown) => {
                base.deterministic = false;
                base.fallibility = Fallibility::MayFail;
                base.unknown = true;
                base.confidence = EffectConfidence::Conservative;
                base.reads.insert(ResourceKey::HostWorld);
                base.writes.insert(ResourceKey::HostWorld);
            }
        }

        let has_declared_resources =
            !self.reads.is_empty() || !self.writes.is_empty() || self.serial_host;
        let has_declared_host_resources = self.serial_host
            || self.reads.iter().any(ResourceKey::is_host_resource)
            || self.writes.iter().any(ResourceKey::is_host_resource);

        base.reads.extend(self.reads.iter().cloned());
        base.writes.extend(self.writes.iter().cloned());

        if self.serial_host {
            base.reads.insert(ResourceKey::HostWorld);
            base.writes.insert(ResourceKey::HostWorld);
        }

        // A source declaration is not proof that a host footprint is complete.
        // On a previously resource-free operation, retain a HostWorld umbrella
        // so it cannot become independent from an unknown hosted operation.
        if base_was_verified_pure && has_declared_host_resources {
            base.reads.insert(ResourceKey::HostWorld);
            base.writes.insert(ResourceKey::HostWorld);
        }
        if base_was_verified_pure
            && has_declared_resources
            && self.purity != Some(DeclaredPurity::Unknown)
        {
            base.confidence = EffectConfidence::UserDeclared;
        }

        Ok(base)
    }
}

fn parse_resources(list: &str) -> Result<BTreeSet<ResourceKey>, String> {
    if list.trim().is_empty() {
        return Err("resource declaration must not be empty".to_string());
    }
    let mut resources = BTreeSet::new();
    for item in list.split(['+', ';']).map(str::trim) {
        if item.is_empty() {
            return Err(format!("empty resource in `{list}`"));
        }
        resources.insert(parse_resource(item)?);
    }
    Ok(resources)
}

fn parse_resource(item: &str) -> Result<ResourceKey, String> {
    match item {
        "stdio" => return Ok(ResourceKey::Stdio),
        "network:*" | "network" => return Ok(ResourceKey::NetworkUnknown),
        "host:*" | "hostworld" => return Ok(ResourceKey::HostWorld),
        "evaluator" => return Ok(ResourceKey::EvaluatorState),
        _ => {}
    }

    let (kind, value) = item
        .split_once(':')
        .ok_or_else(|| format!("invalid resource `{item}`; expected `kind:value`"))?;
    if value.is_empty() {
        return Err(format!("resource `{item}` has an empty identity"));
    }

    if is_governed_source_resource_kind(kind) {
        return Err(format!(
            "governed resource kind `{kind}` in `{item}` requires trusted lowering; source effect declarations cannot mint governed state or authority"
        ));
    }

    match kind {
        "project" => {
            if value.starts_with('/') {
                return Err(format!(
                    "project resource `{value}` must be relative to the project root"
                ));
            }
            Ok(ResourceKey::ProjectPath(value.to_string()))
        }
        "host" => Ok(ResourceKey::HostPath(value.to_string())),
        "env" => {
            validate_identifier(value, "environment variable")?;
            Ok(ResourceKey::EnvVar(value.to_string()))
        }
        "scope" => {
            validate_identifier(value, "scope binding")?;
            Ok(ResourceKey::ScopeBinding(value.to_string()))
        }
        "network" => Ok(ResourceKey::Network(value.to_string())),
        "service" => Ok(ResourceKey::Service(value.to_string())),
        "actor" => parse_actor_resource(value).map(ResourceKey::ActorState),
        _ => Err(format!(
            "unknown resource kind `{kind}` in `{item}`; expected project, host, env, scope, network, service, or actor"
        )),
    }
}

/// Parse one user-declared ambient resource for trusted compiler subsystems.
///
/// Project-plan lowering shares the same grammar and, critically, the same
/// governed-resource rejection as `.O` block effects. This is crate-private:
/// it does not expose an authority-minting API to callers.
pub(crate) fn parse_declared_resource(item: &str) -> Result<ResourceKey, String> {
    parse_resource(item)
}

fn is_governed_source_resource_kind(kind: &str) -> bool {
    matches!(
        kind,
        "world"
            | "world-state"
            | "governor"
            | "governor-state"
            | "node"
            | "node-state"
            | "domain"
            | "domain-state"
            | "process"
            | "process-state"
            | "resource"
            | "resource-state"
            | "governed-resource"
            | "object"
            | "object-state"
            | "capability"
            | "capability-state"
            | "namespace"
            | "namespace-state"
            | "task"
            | "task-state"
            | "artifact"
            | "artifact-state"
            | "device"
            | "device-state"
            | "accelerator"
            | "accelerator-state"
    )
}

fn validate_identifier(value: &str, label: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{label} must not be empty"));
    };
    if !(first == '_' || first.is_ascii_alphabetic())
        || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(format!("invalid {label} `{value}`"));
    }
    Ok(())
}

fn parse_actor_resource(value: &str) -> Result<ActorResourceId, String> {
    let (language, environment) = value
        .strip_suffix(']')
        .and_then(|value| value.rsplit_once('['))
        .ok_or_else(|| {
            format!("invalid actor resource `{value}`; expected `language[environment]`")
        })?;
    if language.is_empty() {
        return Err("actor resource language must not be empty".to_string());
    }
    let environment = environment
        .parse::<u32>()
        .map_err(|_| format!("invalid actor environment `{environment}`"))?;
    if environment == u32::MAX {
        return Err("actor resources require an explicit persistent environment".to_string());
    }
    Ok(ActorResourceId::new(language, environment))
}

/// Derive the conservative semantic effects of one plan node.
pub fn effect_summary_for_plan_node(
    id: PlanNodeId,
    kind: &PlanNodeKind,
) -> Result<EffectSummary, String> {
    match kind {
        PlanNodeKind::Text => Ok(EffectSummary::pure()),
        PlanNodeKind::Load { name } => {
            let mut summary = EffectSummary::pure();
            summary.fallibility = Fallibility::MayFail;
            summary
                .reads
                .insert(ResourceKey::ScopeBinding(name.clone()));
            Ok(summary)
        }
        PlanNodeKind::Store { name } => {
            let mut summary = EffectSummary::pure();
            summary
                .writes
                .insert(ResourceKey::ScopeBinding(name.clone()));
            Ok(summary)
        }
        PlanNodeKind::Group { .. } => Ok(EffectSummary::pure()),
        PlanNodeKind::Call { .. }
        | PlanNodeKind::Request { .. }
        | PlanNodeKind::Schedule { .. } => Ok(EffectSummary::conservative_evaluator()),
        PlanNodeKind::Exec {
            env_id,
            attr,
            backend,
            ..
        } => {
            let has_deferred_control = attr
                .iter()
                .flat_map(|attr| attr.split(','))
                .any(|entry| matches!(entry.trim(), "lazy" | "defer"));
            let trusted_inline = backend.pure
                && backend.execution == ExecutionMode::InlineValue
                && matches!(
                    backend.canonical.as_str(),
                    "html" | "markdown" | "text" | "latex"
                )
                && !has_deferred_control;

            let mut summary = if trusted_inline {
                EffectSummary::pure()
            } else {
                EffectSummary::conservative_evaluator()
            };

            // An explicit environment denotes persistent evaluator identity.
            // Even today's state-free inline implementations receive the
            // token conservatively so future backend changes cannot make an
            // indexed block silently share mutable state off-graph.
            if *env_id != u32::MAX {
                summary = summary
                    .with_actor_state(ActorResourceId::new(backend.canonical.clone(), *env_id));
            }

            let declaration = EffectDeclaration::parse(attr.as_deref())
                .map_err(|error| format!("plan node {}: {error}", id.0))?;
            declaration
                .apply_checked(summary, EffectTrustPolicy::Strict)
                .map_err(|error| format!("plan node {}: {error}", id.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::identity::{
        ResourceGeneration, ResourceId, ResourceOwner, WorldEpoch, WorldId,
    };

    #[test]
    fn resource_display_is_stable() {
        let actor = ActorResourceId::new("python", 3);
        assert_eq!(actor.to_string(), "python[3]");
        assert_eq!(
            ResourceKey::ActorState(actor).to_string(),
            "actor:python[3]"
        );
        assert_eq!(ResourceKey::HostWorld.to_string(), "HostWorld");
    }

    #[test]
    fn parser_rejects_malformed_resources() {
        assert!(EffectDeclaration::parse(Some("reads=project:/absolute")).is_err());
        assert!(EffectDeclaration::parse(Some("reads=env:bad-name")).is_err());
        assert!(EffectDeclaration::parse(Some("reads=actor:python[*]")).is_err());
        assert!(EffectDeclaration::parse(Some("effects=trusted")).is_err());
    }

    #[test]
    fn exact_network_resources_share_the_unknown_network_state() {
        let mut summary = EffectSummary::pure();
        summary
            .reads
            .insert(ResourceKey::Network("api.example.test".into()));

        let resources = summary.resource_union();
        assert!(resources.contains(&ResourceKey::Network("api.example.test".into())));
        assert!(resources.contains(&ResourceKey::NetworkUnknown));
    }

    #[test]
    fn unknown_network_aliases_exact_endpoints_in_conflict_checks() {
        let mut unknown_writer = EffectSummary::pure();
        unknown_writer.writes.insert(ResourceKey::NetworkUnknown);

        let mut exact_reader = EffectSummary::pure();
        exact_reader
            .reads
            .insert(ResourceKey::Network("api.example.test".into()));
        assert!(unknown_writer.conflicts_with(&exact_reader));

        let mut other_exact_writer = EffectSummary::pure();
        other_exact_writer
            .writes
            .insert(ResourceKey::Network("mirror.example.test".into()));
        assert!(other_exact_writer.conflicts_with(&exact_reader));

        let mut unknown_reader = EffectSummary::pure();
        unknown_reader.reads.insert(ResourceKey::NetworkUnknown);
        assert!(other_exact_writer.conflicts_with(&unknown_reader));
        assert!(!unknown_reader.conflicts_with(&exact_reader));
    }

    #[test]
    fn precise_host_resources_share_a_read_umbrella_without_aliasing_each_other() {
        let mut left = EffectSummary::pure();
        left.writes
            .insert(ResourceKey::ProjectPath("left.json".into()));
        let mut right = EffectSummary::pure();
        right
            .writes
            .insert(ResourceKey::ProjectPath("right.json".into()));

        let (left_reads, left_writes) = left.scheduling_accesses();
        assert!(left_reads.contains(&ResourceKey::HostWorld));
        assert!(!left_writes.contains(&ResourceKey::HostWorld));
        assert!(!left.conflicts_with(&right));

        let ambient = EffectSummary::unknown();
        assert!(left.conflicts_with(&ambient));
        assert!(right.conflicts_with(&ambient));
    }

    #[test]
    fn governed_device_views_alias_their_generic_resource_state() {
        let identity = ResourceIdentity::new(
            ResourceOwner::World {
                world: WorldIdentity::new(
                    WorldId::new("test-world").unwrap(),
                    WorldEpoch::new(1).unwrap(),
                ),
            },
            ResourceId::new("device/test").unwrap(),
            ResourceGeneration::new(1).unwrap(),
        );
        let mut device_reader = EffectSummary::pure();
        device_reader
            .reads
            .insert(ResourceKey::DeviceState(identity.clone()));
        let mut generic_writer = EffectSummary::pure();
        generic_writer
            .writes
            .insert(ResourceKey::GovernedResource(identity));

        assert!(device_reader.conflicts_with(&generic_writer));
    }
}
