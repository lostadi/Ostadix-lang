//! Canonical project-profile logical HGraph schema.
//!
//! [`LogicalHGraphV1`] is the first World PR8 graph-layer artifact. It records
//! semantic project operations, typed dependencies, exact source binding, and
//! complete effect facts without carrying placement, runtime materialization,
//! recovery, or authority grants. Planner-local operation identifiers are not
//! [`crate::world::TaskIdentity`] values; a later deployment layer must bind
//! them explicitly rather than inferring World identity here.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::effects::{ActorResourceId, EffectConfidence, EffectSummary, Fallibility, ResourceKey};
use crate::hgraph::ExecutableOp;
use crate::ir::PlanNodeId;
use crate::world::{
    ArtifactId, ArtifactPublicationIdentity, CapabilityIdentity, DomainIdentity, GovernorIdentity,
    NodeIdentity, ObjectIdentity, ProcessIdentity, ResourceIdentity, TaskAttemptIdentity,
    WorldIdentity, WorldIdentityError,
};

use super::model::{RouteFailureContinuation, RouteGuard, RouteKind, RoutePolicy};
use super::plan::{
    ProjectCancellationSemantics, ProjectDependency, ProjectExecutionPlan, ProjectHGraph,
    ProjectPlanOperation, RoutePlanFacts,
};

pub const LOGICAL_HGRAPH_SCHEMA_V1: u16 = 1;
pub const MAX_LOGICAL_HGRAPH_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_LOGICAL_OPERATIONS: usize = 65_536;
pub const MAX_LOGICAL_RESOURCES_PER_EFFECT: usize = 4_096;
pub const MAX_LOGICAL_REFERENCES_PER_OPERATION: usize = 4_096;

const MAX_LOGICAL_LABEL_BYTES: usize = 4_096;
const LOGICAL_HGRAPH_DIGEST_DOMAIN: &[u8] = b"ostadix.world.logical-hgraph/v1\0";

#[derive(Debug, Error)]
pub enum LogicalHGraphError {
    #[error("invalid LogicalHGraphV1: {0}")]
    Invalid(String),
    #[error("LogicalHGraphV1 JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error("LogicalHGraphV1 record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("LogicalHGraphV1 bytes are not the canonical encoding")]
    NonCanonicalEncoding,
}

fn invalid(reason: impl Into<String>) -> LogicalHGraphError {
    LogicalHGraphError::Invalid(reason.into())
}

fn validate_text(value: &str, field: &str) -> Result<(), LogicalHGraphError> {
    if value.is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    if value.len() > MAX_LOGICAL_LABEL_BYTES {
        return Err(invalid(format!(
            "{field} exceeds {MAX_LOGICAL_LABEL_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(format!("{field} contains a control character")));
    }
    Ok(())
}

fn validate_digest(digest: &ArtifactId, field: &str) -> Result<(), LogicalHGraphError> {
    if digest.as_sha256().bytes().all(|byte| byte == b'0') {
        return Err(invalid(format!(
            "{field} uses the reserved all-zero digest"
        )));
    }
    Ok(())
}

fn ensure_strict_order<T: Ord>(values: &[T], field: &str) -> Result<(), LogicalHGraphError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid(format!(
            "{field} must be strictly ordered without duplicates"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalOperationIdV1(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalProjectSourceV1 {
    pub project_name: String,
    pub bundle: ArtifactId,
    pub target: String,
    pub alternatives: Vec<String>,
    pub policy: LogicalRoutePolicyV1,
    pub cancellation: LogicalCancellationV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogicalRoutePolicyV1 {
    Explicit { route_id: String },
    Default,
    Fallback,
    AnySuccess,
    RaceSuccess,
    RaceSettle,
    All,
    VerifyEquivalent,
    BenchmarkAndSelect,
}

impl From<&RoutePolicy> for LogicalRoutePolicyV1 {
    fn from(policy: &RoutePolicy) -> Self {
        match policy {
            RoutePolicy::Explicit(route_id) => Self::Explicit {
                route_id: route_id.clone(),
            },
            RoutePolicy::Default => Self::Default,
            RoutePolicy::Fallback => Self::Fallback,
            RoutePolicy::AnySuccess => Self::AnySuccess,
            RoutePolicy::RaceSuccess => Self::RaceSuccess,
            RoutePolicy::RaceSettle => Self::RaceSettle,
            RoutePolicy::All => Self::All,
            RoutePolicy::VerifyEquivalent => Self::VerifyEquivalent,
            RoutePolicy::BenchmarkAndSelect => Self::BenchmarkAndSelect,
        }
    }
}

impl LogicalRoutePolicyV1 {
    fn to_project(&self) -> RoutePolicy {
        match self {
            Self::Explicit { route_id } => RoutePolicy::Explicit(route_id.clone()),
            Self::Default => RoutePolicy::Default,
            Self::Fallback => RoutePolicy::Fallback,
            Self::AnySuccess => RoutePolicy::AnySuccess,
            Self::RaceSuccess => RoutePolicy::RaceSuccess,
            Self::RaceSettle => RoutePolicy::RaceSettle,
            Self::All => RoutePolicy::All,
            Self::VerifyEquivalent => RoutePolicy::VerifyEquivalent,
            Self::BenchmarkAndSelect => RoutePolicy::BenchmarkAndSelect,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalCancellationV1 {
    None,
    StopAfterSuccess,
    CancelLosers,
}

impl From<ProjectCancellationSemantics> for LogicalCancellationV1 {
    fn from(cancellation: ProjectCancellationSemantics) -> Self {
        match cancellation {
            ProjectCancellationSemantics::None => Self::None,
            ProjectCancellationSemantics::StopAfterSuccess => Self::StopAfterSuccess,
            ProjectCancellationSemantics::CancelLosers => Self::CancelLosers,
        }
    }
}

impl LogicalCancellationV1 {
    const fn to_project(self) -> ProjectCancellationSemantics {
        match self {
            Self::None => ProjectCancellationSemantics::None,
            Self::StopAfterSuccess => ProjectCancellationSemantics::StopAfterSuccess,
            Self::CancelLosers => ProjectCancellationSemantics::CancelLosers,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalDependencyKindV1 {
    Value,
    Success,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalDependencyV1 {
    pub predecessor: LogicalOperationIdV1,
    pub requirement: LogicalDependencyKindV1,
}

impl TryFrom<ProjectDependency> for LogicalDependencyV1 {
    type Error = LogicalHGraphError;

    fn try_from(dependency: ProjectDependency) -> Result<Self, Self::Error> {
        Ok(match dependency {
            ProjectDependency::Value(id) => Self {
                predecessor: LogicalOperationIdV1(
                    u64::try_from(id.0)
                        .map_err(|_| invalid("project dependency operation id does not fit u64"))?,
                ),
                requirement: LogicalDependencyKindV1::Value,
            },
            ProjectDependency::Success(id) => Self {
                predecessor: LogicalOperationIdV1(
                    u64::try_from(id.0)
                        .map_err(|_| invalid("project dependency operation id does not fit u64"))?,
                ),
                requirement: LogicalDependencyKindV1::Success,
            },
        })
    }
}

impl LogicalDependencyV1 {
    fn to_project(&self) -> Result<ProjectDependency, LogicalHGraphError> {
        let predecessor = usize::try_from(self.predecessor.0)
            .map(PlanNodeId)
            .map_err(|_| invalid("logical dependency id does not fit usize"))?;
        Ok(match self.requirement {
            LogicalDependencyKindV1::Value => ProjectDependency::Value(predecessor),
            LogicalDependencyKindV1::Success => ProjectDependency::Success(predecessor),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogicalOperationKindV1 {
    MaterializeProject,
    BuildRoute { route_id: String },
    RunRoute { route_id: String },
    SelectRoute { policy: LogicalRoutePolicyV1 },
    CompareRouteResults,
}

impl LogicalOperationKindV1 {
    fn to_project(&self) -> ExecutableOp {
        match self {
            Self::MaterializeProject => ExecutableOp::MaterializeProject,
            Self::BuildRoute { route_id } => ExecutableOp::BuildRoute {
                route_id: route_id.clone(),
            },
            Self::RunRoute { route_id } => ExecutableOp::RunRoute {
                route_id: route_id.clone(),
            },
            Self::SelectRoute { policy } => ExecutableOp::SelectRoute {
                policy: policy.to_project().token(),
            },
            Self::CompareRouteResults => ExecutableOp::CompareRouteResults,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalArtifactRoleV1 {
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalArtifactRefV1 {
    pub role: LogicalArtifactRoleV1,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalAuthorityRequirementV1 {
    /// Descriptive requirement only. It is not a capability or authority grant.
    pub resource: LogicalResourceV1,
    pub right: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalRouteKindV1 {
    InterpreterCommand,
    CompiledBinary,
    BuildTarget,
    PackageEntrypoint,
    ShellTask,
    OEvaluator,
    Composite,
}

impl From<RouteKind> for LogicalRouteKindV1 {
    fn from(kind: RouteKind) -> Self {
        match kind {
            RouteKind::InterpreterCommand => Self::InterpreterCommand,
            RouteKind::CompiledBinary => Self::CompiledBinary,
            RouteKind::BuildTarget => Self::BuildTarget,
            RouteKind::PackageEntrypoint => Self::PackageEntrypoint,
            RouteKind::ShellTask => Self::ShellTask,
            RouteKind::OEvaluator => Self::OEvaluator,
            RouteKind::Composite => Self::Composite,
        }
    }
}

impl LogicalRouteKindV1 {
    const fn to_project(self) -> RouteKind {
        match self {
            Self::InterpreterCommand => RouteKind::InterpreterCommand,
            Self::CompiledBinary => RouteKind::CompiledBinary,
            Self::BuildTarget => RouteKind::BuildTarget,
            Self::PackageEntrypoint => RouteKind::PackageEntrypoint,
            Self::ShellTask => RouteKind::ShellTask,
            Self::OEvaluator => RouteKind::OEvaluator,
            Self::Composite => RouteKind::Composite,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogicalRouteGuardV1 {
    PlatformOs { value: String },
    CommandAvailable { command: String },
    EnvVarSet { name: String },
}

impl From<&RouteGuard> for LogicalRouteGuardV1 {
    fn from(guard: &RouteGuard) -> Self {
        match guard {
            RouteGuard::PlatformOs(value) => Self::PlatformOs {
                value: value.clone(),
            },
            RouteGuard::CommandAvailable(command) => Self::CommandAvailable {
                command: command.clone(),
            },
            RouteGuard::EnvVarSet(name) => Self::EnvVarSet { name: name.clone() },
        }
    }
}

impl LogicalRouteGuardV1 {
    fn to_project(&self) -> RouteGuard {
        match self {
            Self::PlatformOs { value } => RouteGuard::PlatformOs(value.clone()),
            Self::CommandAvailable { command } => RouteGuard::CommandAvailable(command.clone()),
            Self::EnvVarSet { name } => RouteGuard::EnvVarSet(name.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalFailureContinuationV1 {
    Unproven,
    DeclaredIdempotent,
}

impl From<RouteFailureContinuation> for LogicalFailureContinuationV1 {
    fn from(value: RouteFailureContinuation) -> Self {
        match value {
            RouteFailureContinuation::Unproven => Self::Unproven,
            RouteFailureContinuation::DeclaredIdempotent => Self::DeclaredIdempotent,
        }
    }
}

impl LogicalFailureContinuationV1 {
    const fn to_project(self) -> RouteFailureContinuation {
        match self {
            Self::Unproven => RouteFailureContinuation::Unproven,
            Self::DeclaredIdempotent => RouteFailureContinuation::DeclaredIdempotent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalRouteFactsV1 {
    pub route_kind: LogicalRouteKindV1,
    pub executable: Option<String>,
    pub evaluator: Option<String>,
    pub entrypoint: Option<String>,
    pub prerequisites: Vec<String>,
    pub guards: Vec<LogicalRouteGuardV1>,
    pub environment_keys: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub declared_reads: Vec<String>,
    pub declared_writes: Vec<String>,
    pub declared_pure: bool,
    pub failure_continuation: LogicalFailureContinuationV1,
}

impl From<&RoutePlanFacts> for LogicalRouteFactsV1 {
    fn from(facts: &RoutePlanFacts) -> Self {
        Self {
            route_kind: facts.kind.into(),
            executable: facts.executable.clone(),
            evaluator: facts.evaluator.clone(),
            entrypoint: facts.entrypoint.clone(),
            prerequisites: facts.prerequisites.clone(),
            guards: facts.guards.iter().map(Into::into).collect(),
            environment_keys: facts.environment_keys.clone(),
            inputs: facts.inputs.clone(),
            outputs: facts.outputs.clone(),
            declared_reads: facts.declared_reads.clone(),
            declared_writes: facts.declared_writes.clone(),
            declared_pure: facts.declared_pure,
            failure_continuation: facts.failure_continuation.into(),
        }
    }
}

impl LogicalRouteFactsV1 {
    fn to_project(&self) -> RoutePlanFacts {
        RoutePlanFacts {
            kind: self.route_kind.to_project(),
            executable: self.executable.clone(),
            evaluator: self.evaluator.clone(),
            entrypoint: self.entrypoint.clone(),
            prerequisites: self.prerequisites.clone(),
            guards: self
                .guards
                .iter()
                .map(LogicalRouteGuardV1::to_project)
                .collect(),
            environment_keys: self.environment_keys.clone(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            declared_reads: self.declared_reads.clone(),
            declared_writes: self.declared_writes.clone(),
            declared_pure: self.declared_pure,
            failure_continuation: self.failure_continuation.to_project(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalActorResourceV1 {
    pub canonical_language: String,
    pub environment: u32,
}

impl From<&ActorResourceId> for LogicalActorResourceV1 {
    fn from(actor: &ActorResourceId) -> Self {
        Self {
            canonical_language: actor.canonical_language.clone(),
            environment: actor.environment,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogicalResourceV1 {
    HostWorld,
    WorldState {
        world: WorldIdentity,
    },
    GovernorState {
        governor: GovernorIdentity,
    },
    NodeState {
        node: NodeIdentity,
    },
    DomainState {
        domain: DomainIdentity,
    },
    ProcessState {
        process: ProcessIdentity,
    },
    GovernedResource {
        resource: ResourceIdentity,
    },
    ObjectState {
        object: ObjectIdentity,
    },
    CapabilityState {
        capability: CapabilityIdentity,
    },
    NamespaceState {
        world: WorldIdentity,
    },
    TaskState {
        attempt: TaskAttemptIdentity,
    },
    ArtifactState {
        artifact: ArtifactPublicationIdentity,
    },
    DeviceState {
        device: ResourceIdentity,
    },
    AcceleratorState {
        accelerator: ResourceIdentity,
    },
    EvaluatorState,
    ScopeBinding {
        name: String,
    },
    ProjectPath {
        path: String,
    },
    HostPath {
        path: String,
    },
    EnvVar {
        name: String,
    },
    Stdio,
    Network {
        endpoint: String,
    },
    NetworkUnknown,
    Service {
        name: String,
    },
    ActorState {
        actor: LogicalActorResourceV1,
    },
}

impl LogicalResourceV1 {
    pub fn is_governed(&self) -> bool {
        matches!(
            self,
            Self::WorldState { .. }
                | Self::GovernorState { .. }
                | Self::NodeState { .. }
                | Self::DomainState { .. }
                | Self::ProcessState { .. }
                | Self::GovernedResource { .. }
                | Self::ObjectState { .. }
                | Self::CapabilityState { .. }
                | Self::NamespaceState { .. }
                | Self::TaskState { .. }
                | Self::ArtifactState { .. }
                | Self::DeviceState { .. }
                | Self::AcceleratorState { .. }
        )
    }

    fn validate(&self) -> Result<(), LogicalHGraphError> {
        match self {
            Self::ScopeBinding { name } | Self::EnvVar { name } | Self::Service { name } => {
                validate_text(name, "logical resource name")
            }
            Self::ProjectPath { path } | Self::HostPath { path } => {
                validate_text(path, "logical resource path")
            }
            Self::Network { endpoint } => validate_text(endpoint, "network endpoint"),
            Self::ActorState { actor } => {
                validate_text(&actor.canonical_language, "actor language")
            }
            _ => Ok(()),
        }
    }

    fn to_project(&self) -> ResourceKey {
        match self {
            Self::HostWorld => ResourceKey::HostWorld,
            Self::WorldState { world } => ResourceKey::WorldState(world.clone()),
            Self::GovernorState { governor } => ResourceKey::GovernorState(governor.clone()),
            Self::NodeState { node } => ResourceKey::NodeState(node.clone()),
            Self::DomainState { domain } => ResourceKey::DomainState(domain.clone()),
            Self::ProcessState { process } => ResourceKey::ProcessState(process.clone()),
            Self::GovernedResource { resource } => ResourceKey::GovernedResource(resource.clone()),
            Self::ObjectState { object } => ResourceKey::ObjectState(object.clone()),
            Self::CapabilityState { capability } => {
                ResourceKey::CapabilityState(capability.clone())
            }
            Self::NamespaceState { world } => ResourceKey::NamespaceState(world.clone()),
            Self::TaskState { attempt } => ResourceKey::TaskState(attempt.clone()),
            Self::ArtifactState { artifact } => ResourceKey::ArtifactState(artifact.clone()),
            Self::DeviceState { device } => ResourceKey::DeviceState(device.clone()),
            Self::AcceleratorState { accelerator } => {
                ResourceKey::AcceleratorState(accelerator.clone())
            }
            Self::EvaluatorState => ResourceKey::EvaluatorState,
            Self::ScopeBinding { name } => ResourceKey::ScopeBinding(name.clone()),
            Self::ProjectPath { path } => ResourceKey::ProjectPath(path.clone()),
            Self::HostPath { path } => ResourceKey::HostPath(path.clone()),
            Self::EnvVar { name } => ResourceKey::EnvVar(name.clone()),
            Self::Stdio => ResourceKey::Stdio,
            Self::Network { endpoint } => ResourceKey::Network(endpoint.clone()),
            Self::NetworkUnknown => ResourceKey::NetworkUnknown,
            Self::Service { name } => ResourceKey::Service(name.clone()),
            Self::ActorState { actor } => ResourceKey::ActorState(ActorResourceId {
                canonical_language: actor.canonical_language.clone(),
                environment: actor.environment,
            }),
        }
    }
}

impl From<&ResourceKey> for LogicalResourceV1 {
    fn from(resource: &ResourceKey) -> Self {
        match resource {
            ResourceKey::HostWorld => Self::HostWorld,
            ResourceKey::WorldState(world) => Self::WorldState {
                world: world.clone(),
            },
            ResourceKey::GovernorState(governor) => Self::GovernorState {
                governor: governor.clone(),
            },
            ResourceKey::NodeState(node) => Self::NodeState { node: node.clone() },
            ResourceKey::DomainState(domain) => Self::DomainState {
                domain: domain.clone(),
            },
            ResourceKey::ProcessState(process) => Self::ProcessState {
                process: process.clone(),
            },
            ResourceKey::GovernedResource(resource) => Self::GovernedResource {
                resource: resource.clone(),
            },
            ResourceKey::ObjectState(object) => Self::ObjectState {
                object: object.clone(),
            },
            ResourceKey::CapabilityState(capability) => Self::CapabilityState {
                capability: capability.clone(),
            },
            ResourceKey::NamespaceState(world) => Self::NamespaceState {
                world: world.clone(),
            },
            ResourceKey::TaskState(attempt) => Self::TaskState {
                attempt: attempt.clone(),
            },
            ResourceKey::ArtifactState(artifact) => Self::ArtifactState {
                artifact: artifact.clone(),
            },
            ResourceKey::DeviceState(device) => Self::DeviceState {
                device: device.clone(),
            },
            ResourceKey::AcceleratorState(accelerator) => Self::AcceleratorState {
                accelerator: accelerator.clone(),
            },
            ResourceKey::EvaluatorState => Self::EvaluatorState,
            ResourceKey::ScopeBinding(name) => Self::ScopeBinding { name: name.clone() },
            ResourceKey::ProjectPath(path) => Self::ProjectPath { path: path.clone() },
            ResourceKey::HostPath(path) => Self::HostPath { path: path.clone() },
            ResourceKey::EnvVar(name) => Self::EnvVar { name: name.clone() },
            ResourceKey::Stdio => Self::Stdio,
            ResourceKey::Network(endpoint) => Self::Network {
                endpoint: endpoint.clone(),
            },
            ResourceKey::NetworkUnknown => Self::NetworkUnknown,
            ResourceKey::Service(name) => Self::Service { name: name.clone() },
            ResourceKey::ActorState(actor) => Self::ActorState {
                actor: actor.into(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalFallibilityV1 {
    Infallible,
    MayFail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalEffectConfidenceV1 {
    Verified,
    Conservative,
    UserDeclared,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalEffectSummaryV1 {
    pub deterministic: bool,
    pub fallibility: LogicalFallibilityV1,
    pub reads: Vec<LogicalResourceV1>,
    pub writes: Vec<LogicalResourceV1>,
    /// Exact scheduler-visible union after applying resource aliases.
    pub scheduler_resources: Vec<LogicalResourceV1>,
    pub actor_state: Option<LogicalActorResourceV1>,
    pub unknown: bool,
    pub network: bool,
    pub spawn: bool,
    pub clock: bool,
    pub confidence: LogicalEffectConfidenceV1,
}

impl LogicalEffectSummaryV1 {
    fn from_effects(effects: &EffectSummary) -> Self {
        let mut reads = effects
            .reads
            .iter()
            .map(LogicalResourceV1::from)
            .collect::<Vec<_>>();
        let mut writes = effects
            .writes
            .iter()
            .map(LogicalResourceV1::from)
            .collect::<Vec<_>>();
        let scheduler_resources = effects
            .accessed_resources()
            .iter()
            .map(LogicalResourceV1::from)
            .collect::<Vec<_>>();
        reads.sort();
        reads.dedup();
        writes.sort();
        writes.dedup();
        Self {
            deterministic: effects.deterministic,
            fallibility: match effects.fallibility {
                Fallibility::Infallible => LogicalFallibilityV1::Infallible,
                Fallibility::MayFail => LogicalFallibilityV1::MayFail,
            },
            reads,
            writes,
            scheduler_resources,
            actor_state: effects.actor_state.as_ref().map(Into::into),
            unknown: effects.unknown,
            network: effects.network,
            spawn: effects.spawn,
            clock: effects.clock,
            confidence: match effects.confidence {
                EffectConfidence::Verified => LogicalEffectConfidenceV1::Verified,
                EffectConfidence::Conservative => LogicalEffectConfidenceV1::Conservative,
                EffectConfidence::UserDeclared => LogicalEffectConfidenceV1::UserDeclared,
            },
        }
    }

    fn validate(&self) -> Result<(), LogicalHGraphError> {
        for (field, resources) in [
            ("effect reads", &self.reads),
            ("effect writes", &self.writes),
            ("scheduler resources", &self.scheduler_resources),
        ] {
            if resources.len() > MAX_LOGICAL_RESOURCES_PER_EFFECT {
                return Err(invalid(format!(
                    "{field} has {} entries; maximum is {MAX_LOGICAL_RESOURCES_PER_EFFECT}",
                    resources.len()
                )));
            }
            ensure_strict_order(resources, field)?;
            for resource in resources {
                resource.validate()?;
            }
        }
        if self.unknown
            && (!self.reads.contains(&LogicalResourceV1::HostWorld)
                || !self.writes.contains(&LogicalResourceV1::HostWorld))
        {
            return Err(invalid(
                "unknown effects must retain HostWorld in both reads and writes",
            ));
        }
        let actor_resource = self
            .actor_state
            .as_ref()
            .map(|actor| LogicalResourceV1::ActorState {
                actor: actor.clone(),
            });
        if let Some(actor_resource) = actor_resource {
            if !self.reads.contains(&actor_resource) || !self.writes.contains(&actor_resource) {
                return Err(invalid(
                    "actor_state must appear in both effect reads and writes",
                ));
            }
        } else if self
            .reads
            .iter()
            .chain(&self.writes)
            .any(|resource| matches!(resource, LogicalResourceV1::ActorState { .. }))
        {
            return Err(invalid(
                "ActorState resources require the matching actor_state field",
            ));
        }
        self.to_project()?;
        Ok(())
    }

    fn to_project(&self) -> Result<EffectSummary, LogicalHGraphError> {
        let effects = EffectSummary {
            deterministic: self.deterministic,
            fallibility: match self.fallibility {
                LogicalFallibilityV1::Infallible => Fallibility::Infallible,
                LogicalFallibilityV1::MayFail => Fallibility::MayFail,
            },
            reads: self
                .reads
                .iter()
                .map(LogicalResourceV1::to_project)
                .collect(),
            writes: self
                .writes
                .iter()
                .map(LogicalResourceV1::to_project)
                .collect(),
            actor_state: self.actor_state.as_ref().map(|actor| ActorResourceId {
                canonical_language: actor.canonical_language.clone(),
                environment: actor.environment,
            }),
            unknown: self.unknown,
            network: self.network,
            spawn: self.spawn,
            clock: self.clock,
            confidence: match self.confidence {
                LogicalEffectConfidenceV1::Verified => EffectConfidence::Verified,
                LogicalEffectConfidenceV1::Conservative => EffectConfidence::Conservative,
                LogicalEffectConfidenceV1::UserDeclared => EffectConfidence::UserDeclared,
            },
        };
        let scheduler_resources = effects
            .accessed_resources()
            .iter()
            .map(LogicalResourceV1::from)
            .collect::<Vec<_>>();
        if self.scheduler_resources != scheduler_resources {
            return Err(invalid(
                "scheduler resources do not match expanded effect resources",
            ));
        }
        Ok(effects)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalOperationV1 {
    pub id: LogicalOperationIdV1,
    pub kind: LogicalOperationKindV1,
    pub branch: Option<u32>,
    pub dependencies: Vec<LogicalDependencyV1>,
    pub effects: LogicalEffectSummaryV1,
    pub route_facts: Option<LogicalRouteFactsV1>,
    pub authority_requirements: Vec<LogicalAuthorityRequirementV1>,
    pub artifact_refs: Vec<LogicalArtifactRefV1>,
}

impl LogicalOperationV1 {
    fn to_project(&self) -> Result<ProjectPlanOperation, LogicalHGraphError> {
        let id = usize::try_from(self.id.0)
            .map(PlanNodeId)
            .map_err(|_| invalid("logical operation id does not fit usize"))?;
        let branch = self
            .branch
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid("logical branch does not fit usize"))?;
        Ok(ProjectPlanOperation {
            id,
            op: self.kind.to_project(),
            dependencies: self
                .dependencies
                .iter()
                .map(LogicalDependencyV1::to_project)
                .collect::<Result<Vec<_>, _>>()?,
            effects: self.effects.to_project()?,
            branch,
            route_facts: self
                .route_facts
                .as_ref()
                .map(LogicalRouteFactsV1::to_project),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalHGraphV1 {
    pub schema_version: u16,
    pub source: LogicalProjectSourceV1,
    pub operations: Vec<LogicalOperationV1>,
    pub roots: Vec<LogicalOperationIdV1>,
}

impl LogicalHGraphV1 {
    fn to_project_plan(&self) -> Result<ProjectExecutionPlan, LogicalHGraphError> {
        Ok(ProjectExecutionPlan {
            project_name: self.source.project_name.clone(),
            bundle_digest: self.source.bundle.as_sha256().to_owned(),
            target: self.source.target.clone(),
            alternatives: self.source.alternatives.clone(),
            policy: self.source.policy.to_project(),
            cancellation: self.source.cancellation.to_project(),
            operations: self
                .operations
                .iter()
                .map(LogicalOperationV1::to_project)
                .collect::<Result<Vec<_>, _>>()?,
            roots: self
                .roots
                .iter()
                .map(|root| {
                    usize::try_from(root.0)
                        .map(PlanNodeId)
                        .map_err(|_| invalid("logical root id does not fit usize"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    /// Normalize one hosted project plan/HGraph after validating their exact
    /// projection relationship. This does not mint World task identity or
    /// authority.
    pub fn from_project(project: &ProjectHGraph) -> Result<Self, LogicalHGraphError> {
        project
            .plan
            .validate_projection(&project.graph)
            .map_err(|error| invalid(format!("project source/projection is invalid: {error}")))?;

        let bundle = ArtifactId::from_sha256(project.plan.bundle_digest.clone())?;
        let source = LogicalProjectSourceV1 {
            project_name: project.plan.project_name.clone(),
            bundle,
            target: project.plan.target.clone(),
            alternatives: project.plan.alternatives.clone(),
            policy: (&project.plan.policy).into(),
            cancellation: project.plan.cancellation.into(),
        };
        let mut operations = Vec::with_capacity(project.plan.operations.len());
        for operation in &project.plan.operations {
            let id = u64::try_from(operation.id.0)
                .map_err(|_| invalid("project operation id does not fit u64"))?;
            let kind = match &operation.op {
                ExecutableOp::MaterializeProject => LogicalOperationKindV1::MaterializeProject,
                ExecutableOp::BuildRoute { route_id } => LogicalOperationKindV1::BuildRoute {
                    route_id: route_id.clone(),
                },
                ExecutableOp::RunRoute { route_id } => LogicalOperationKindV1::RunRoute {
                    route_id: route_id.clone(),
                },
                ExecutableOp::SelectRoute { .. } => LogicalOperationKindV1::SelectRoute {
                    policy: (&project.plan.policy).into(),
                },
                ExecutableOp::CompareRouteResults => LogicalOperationKindV1::CompareRouteResults,
                other => return Err(invalid(format!("unsupported project operation {other:?}"))),
            };
            let branch = operation
                .branch
                .map(u32::try_from)
                .transpose()
                .map_err(|_| invalid("project branch index does not fit u32"))?;
            let route_facts = operation.route_facts.as_ref().map(Into::into);
            let mut artifact_refs = Vec::new();
            if let Some(facts) = &operation.route_facts {
                artifact_refs.extend(facts.inputs.iter().cloned().map(|path| {
                    LogicalArtifactRefV1 {
                        role: LogicalArtifactRoleV1::Input,
                        path,
                    }
                }));
                artifact_refs.extend(facts.outputs.iter().cloned().map(|path| {
                    LogicalArtifactRefV1 {
                        role: LogicalArtifactRoleV1::Output,
                        path,
                    }
                }));
                artifact_refs.sort();
                artifact_refs.dedup();
            }
            operations.push(LogicalOperationV1 {
                id: LogicalOperationIdV1(id),
                kind,
                branch,
                dependencies: operation
                    .dependencies
                    .iter()
                    .copied()
                    .map(LogicalDependencyV1::try_from)
                    .collect::<Result<Vec<_>, _>>()?,
                effects: LogicalEffectSummaryV1::from_effects(&operation.effects),
                route_facts,
                // The hosted project profile exposes ambient HostWorld work. It
                // has no trusted authority lowering and therefore emits none.
                authority_requirements: Vec::new(),
                artifact_refs,
            });
        }
        let graph = Self {
            schema_version: LOGICAL_HGRAPH_SCHEMA_V1,
            source,
            operations,
            roots: project
                .plan
                .roots
                .iter()
                .map(|root| {
                    u64::try_from(root.0)
                        .map(LogicalOperationIdV1)
                        .map_err(|_| invalid("project root id does not fit u64"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        graph.validate()?;
        Ok(graph)
    }

    pub fn validate(&self) -> Result<(), LogicalHGraphError> {
        if self.schema_version != LOGICAL_HGRAPH_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        validate_text(&self.source.project_name, "project name")?;
        validate_digest(&self.source.bundle, "bundle digest")?;
        validate_text(&self.source.target, "selection target")?;
        if self.source.alternatives.is_empty() {
            return Err(invalid("source has no selected alternatives"));
        }
        let mut alternatives = BTreeSet::new();
        for alternative in &self.source.alternatives {
            validate_text(alternative, "alternative route id")?;
            if !alternatives.insert(alternative) {
                return Err(invalid(format!(
                    "source repeats alternative route `{alternative}`"
                )));
            }
        }
        match &self.source.policy {
            LogicalRoutePolicyV1::Explicit { route_id } => {
                validate_text(route_id, "explicit route id")?;
                if self.source.alternatives != [route_id.clone()] {
                    return Err(invalid(
                        "explicit policy must name the sole selected alternative",
                    ));
                }
            }
            LogicalRoutePolicyV1::Default if self.source.alternatives.len() != 1 => {
                return Err(invalid(
                    "default policy must have exactly one selected alternative",
                ))
            }
            _ => {}
        }
        let expected_cancellation = match self.source.policy {
            LogicalRoutePolicyV1::Fallback | LogicalRoutePolicyV1::AnySuccess => {
                LogicalCancellationV1::StopAfterSuccess
            }
            LogicalRoutePolicyV1::RaceSuccess | LogicalRoutePolicyV1::RaceSettle => {
                LogicalCancellationV1::CancelLosers
            }
            _ => LogicalCancellationV1::None,
        };
        if self.source.cancellation != expected_cancellation {
            return Err(invalid("cancellation semantics disagree with route policy"));
        }

        if self.operations.is_empty() || self.operations.len() > MAX_LOGICAL_OPERATIONS {
            return Err(invalid(format!(
                "operation count {} is outside 1..={MAX_LOGICAL_OPERATIONS}",
                self.operations.len()
            )));
        }
        let mut selection = None;
        let mut comparisons = 0usize;
        let mut materialization_branches = BTreeSet::new();
        for (index, operation) in self.operations.iter().enumerate() {
            let expected_id = u64::try_from(index)
                .map_err(|_| invalid("logical operation index does not fit u64"))?;
            if operation.id.0 != expected_id {
                return Err(invalid(format!(
                    "operation index {index} has noncanonical id {}",
                    operation.id.0
                )));
            }
            let mut dependencies = BTreeSet::new();
            for dependency in &operation.dependencies {
                if dependency.predecessor.0 >= operation.id.0 {
                    return Err(invalid(format!(
                        "operation {} has non-preceding dependency {}",
                        operation.id.0, dependency.predecessor.0
                    )));
                }
                if !dependencies.insert(dependency) {
                    return Err(invalid(format!(
                        "operation {} repeats a dependency",
                        operation.id.0
                    )));
                }
            }
            operation.effects.validate()?;
            if operation
                .effects
                .reads
                .iter()
                .chain(&operation.effects.writes)
                .chain(&operation.effects.scheduler_resources)
                .any(LogicalResourceV1::is_governed)
            {
                return Err(invalid(format!(
                    "operation {} fabricates a governed resource without trusted lowering",
                    operation.id.0
                )));
            }
            if operation.authority_requirements.len() > MAX_LOGICAL_REFERENCES_PER_OPERATION
                || operation.artifact_refs.len() > MAX_LOGICAL_REFERENCES_PER_OPERATION
            {
                return Err(invalid(format!(
                    "operation {} exceeds the reference limit",
                    operation.id.0
                )));
            }
            if !operation.authority_requirements.is_empty() {
                return Err(invalid(format!(
                    "operation {} invents authority requirements; the hosted v1 profile admits none",
                    operation.id.0
                )));
            }
            ensure_strict_order(&operation.artifact_refs, "artifact references")?;
            for artifact in &operation.artifact_refs {
                validate_text(&artifact.path, "artifact path")?;
            }
            let mut expected_artifacts = Vec::new();
            if let Some(facts) = &operation.route_facts {
                validate_route_facts(facts)?;
                expected_artifacts.extend(facts.inputs.iter().cloned().map(|path| {
                    LogicalArtifactRefV1 {
                        role: LogicalArtifactRoleV1::Input,
                        path,
                    }
                }));
                expected_artifacts.extend(facts.outputs.iter().cloned().map(|path| {
                    LogicalArtifactRefV1 {
                        role: LogicalArtifactRoleV1::Output,
                        path,
                    }
                }));
            }
            expected_artifacts.sort();
            expected_artifacts.dedup();
            if operation.artifact_refs != expected_artifacts {
                return Err(invalid(format!(
                    "operation {} artifact references differ from route facts",
                    operation.id.0
                )));
            }
            let branch = operation
                .branch
                .map(usize::try_from)
                .transpose()
                .map_err(|_| invalid("logical branch does not fit usize"))?;
            if branch.is_some_and(|branch| branch >= self.source.alternatives.len()) {
                return Err(invalid(format!(
                    "operation {} has an out-of-range branch",
                    operation.id.0
                )));
            }
            match &operation.kind {
                LogicalOperationKindV1::MaterializeProject => {
                    let branch = branch.ok_or_else(|| {
                        invalid(format!(
                            "materialize operation {} has no branch",
                            operation.id.0
                        ))
                    })?;
                    if !operation.dependencies.is_empty() || operation.route_facts.is_some() {
                        return Err(invalid(format!(
                            "materialize operation {} has dependencies or route facts",
                            operation.id.0
                        )));
                    }
                    if !materialization_branches.insert(branch) {
                        return Err(invalid(format!(
                            "materialization branch {branch} is repeated"
                        )));
                    }
                }
                LogicalOperationKindV1::BuildRoute { route_id }
                | LogicalOperationKindV1::RunRoute { route_id } => {
                    validate_text(route_id, "operation route id")?;
                    if branch.is_none() || operation.route_facts.is_none() {
                        return Err(invalid(format!(
                            "route operation {} lacks branch or route facts",
                            operation.id.0
                        )));
                    }
                }
                LogicalOperationKindV1::SelectRoute { policy } => {
                    if policy != &self.source.policy
                        || branch.is_some()
                        || operation.route_facts.is_some()
                    {
                        return Err(invalid(format!(
                            "selection operation {} disagrees with source policy or metadata",
                            operation.id.0
                        )));
                    }
                    if selection.replace(operation.id).is_some() {
                        return Err(invalid("logical graph has multiple selection operations"));
                    }
                }
                LogicalOperationKindV1::CompareRouteResults => {
                    comparisons += 1;
                    if branch.is_some() || operation.route_facts.is_some() {
                        return Err(invalid(format!(
                            "comparison operation {} has route metadata",
                            operation.id.0
                        )));
                    }
                }
            }
        }
        let expected_branches = (0..self.source.alternatives.len()).collect::<BTreeSet<_>>();
        if materialization_branches != expected_branches {
            return Err(invalid(
                "materialization operations do not cover every selected branch exactly",
            ));
        }
        let selection = selection.ok_or_else(|| invalid("logical graph has no selection"))?;
        let operation_count = u64::try_from(self.operations.len())
            .map_err(|_| invalid("logical operation count does not fit u64"))?;
        if selection.0.checked_add(1) != Some(operation_count) || self.roots != [selection] {
            return Err(invalid(
                "selection must be the terminal operation and sole logical root",
            ));
        }
        if matches!(self.source.policy, LogicalRoutePolicyV1::VerifyEquivalent) {
            if comparisons != 1 {
                return Err(invalid(
                    "verify-equivalent policy requires one comparison operation",
                ));
            }
        } else if comparisons != 0 {
            return Err(invalid(
                "comparison operations are exclusive to verify-equivalent policy",
            ));
        }
        self.to_project_plan()?.validate().map_err(|error| {
            invalid(format!(
                "logical project profile violates planner invariants: {error}"
            ))
        })?;
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, LogicalHGraphError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_LOGICAL_HGRAPH_BYTES {
            return Err(LogicalHGraphError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_LOGICAL_HGRAPH_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decode and validate JSON, accepting noncanonical whitespace or field
    /// order. [`Self::canonical_bytes`] always returns the unique encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, LogicalHGraphError> {
        if bytes.len() > MAX_LOGICAL_HGRAPH_BYTES {
            return Err(LogicalHGraphError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_LOGICAL_HGRAPH_BYTES,
            });
        }
        let graph: Self = serde_json::from_slice(bytes)?;
        graph.validate()?;
        Ok(graph)
    }

    /// Decode only the unique compact JSON encoding produced by this schema.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, LogicalHGraphError> {
        let graph = Self::decode(bytes)?;
        if graph.canonical_bytes()? != bytes {
            return Err(LogicalHGraphError::NonCanonicalEncoding);
        }
        Ok(graph)
    }

    pub fn digest(&self) -> Result<ArtifactId, LogicalHGraphError> {
        let bytes = self.canonical_bytes()?;
        let byte_count = u64::try_from(bytes.len())
            .map_err(|_| invalid("canonical logical graph length does not fit u64"))?;
        let mut hasher = Sha256::new();
        hasher.update(LOGICAL_HGRAPH_DIGEST_DOMAIN);
        hasher.update(byte_count.to_le_bytes());
        hasher.update(&bytes);
        Ok(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
    }

    /// Compare against one trusted project plan/HGraph and reject substitution.
    ///
    /// This checks the project's embedded bundle digest; the upstream project
    /// builder or coordinator remains responsible for validating the supplied
    /// [`super::ProjectBundle`] bytes against that plan.
    pub fn validate_trusted_project(
        &self,
        project: &ProjectHGraph,
    ) -> Result<(), LogicalHGraphError> {
        let canonical = Self::from_project(project)?;
        if self != &canonical {
            return Err(invalid(
                "logical graph does not match the supplied project source and HGraph",
            ));
        }
        Ok(())
    }
}

fn validate_route_facts(facts: &LogicalRouteFactsV1) -> Result<(), LogicalHGraphError> {
    for (field, value) in [
        ("route executable", facts.executable.as_deref()),
        ("route evaluator", facts.evaluator.as_deref()),
        ("route entrypoint", facts.entrypoint.as_deref()),
    ] {
        if let Some(value) = value {
            validate_text(value, field)?;
        }
    }
    for (field, values) in [
        ("route prerequisite", facts.prerequisites.as_slice()),
        ("environment key", facts.environment_keys.as_slice()),
        ("route input", facts.inputs.as_slice()),
        ("route output", facts.outputs.as_slice()),
        ("declared read", facts.declared_reads.as_slice()),
        ("declared write", facts.declared_writes.as_slice()),
    ] {
        if values.len() > MAX_LOGICAL_REFERENCES_PER_OPERATION {
            return Err(invalid(format!(
                "{field} count exceeds {MAX_LOGICAL_REFERENCES_PER_OPERATION}"
            )));
        }
        for value in values {
            validate_text(value, field)?;
        }
    }
    if facts.guards.len() > MAX_LOGICAL_REFERENCES_PER_OPERATION {
        return Err(invalid("route guard count exceeds the reference limit"));
    }
    for guard in &facts.guards {
        match guard {
            LogicalRouteGuardV1::PlatformOs { value } => validate_text(value, "guard OS")?,
            LogicalRouteGuardV1::CommandAvailable { command } => {
                validate_text(command, "guard command")?
            }
            LogicalRouteGuardV1::EnvVarSet { name } => validate_text(name, "guard env var")?,
        }
    }
    Ok(())
}

impl ProjectHGraph {
    /// Return the canonical versioned logical layer for this exact validated
    /// project plan/HGraph projection.
    pub fn logical_v1(&self) -> Result<LogicalHGraphV1, LogicalHGraphError> {
        LogicalHGraphV1::from_project(self)
    }
}
