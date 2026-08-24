//! Task-oriented execution intents shared by the unified `o` front door.
//!
//! This module is deliberately an orchestrator over existing engines. It
//! classifies and validates an input before execution, binds the exact source
//! or project-bundle identity, and then calls the evaluator, Project HGraph,
//! or project-mesh API without parsing another CLI's human output.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend_catalog::{BackendAdapterKind, ExecutionMode};
use crate::eval::{Evaluator, ExecutionTrace, TraceEvent};
use crate::evidence::ExecutionIntentV1;
use crate::execution_contract::Policy;
use crate::hosted_remote::project_mesh::{
    execute_mesh_selection_observed, observe_mesh_peers_read_only, MeshExecutionConfig,
    MeshExecutionOutcome, MeshExecutionTraceV1, MeshLocalFallback, MeshReadOnlyDiscoveryConfig,
    MeshRequirement, MeshTraceEventV1,
};
use crate::ir::{BackendRegistry, OIr, OIrProgram};
use crate::parser::Parser;
use crate::project::executor::{
    execute_selection_with_configured_executor, ConfiguredProjectExecution, PROJECT_EXECUTOR_ENV,
};
use crate::project::runtime::{potential_route_execution_count, RunOptions};
use crate::project::{
    build_project_hgraph, DeploymentPlanV1, OExecutionResult, ProjectAttemptState,
    ProjectAttemptTrace, ProjectBundle, RoutePolicy,
};
use crate::value::OValue;

pub mod record;
pub mod store;
pub use record::*;
pub use store::*;

pub const O_EXECUTION_TRACE_SCHEMA_V1: &str = "ostadix.oir-execution-trace/v1";

/// Exact supported input classes for the V1 task-oriented front door.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentInputKindV1 {
    OrdinaryO,
    ProjectDirectory,
    LiftedProject,
}

impl IntentInputKindV1 {
    pub const fn token(self) -> &'static str {
        match self {
            Self::OrdinaryO => "ordinary_o",
            Self::ProjectDirectory => "project_directory",
            Self::LiftedProject => "lifted_project",
        }
    }
}

/// Local executor selected for an ordinary `.O` program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalOExecutorV1 {
    ConfiguredGraph,
    ForcedGraph,
    ForcedSerial,
}

impl LocalOExecutorV1 {
    pub const fn token(self) -> &'static str {
        match self {
            Self::ConfiguredGraph => "local_hgraph",
            Self::ForcedGraph => "local_hgraph_forced",
            Self::ForcedSerial => "local_serial",
        }
    }
}

/// Concrete project executor selected after preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectExecutorV1 {
    Compatibility,
    Hgraph,
    MeshPrefer,
    MeshRequired,
}

impl ProjectExecutorV1 {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Compatibility => "project_compatibility",
            Self::Hgraph => "project_hgraph",
            Self::MeshPrefer => "project_mesh_prefer",
            Self::MeshRequired => "project_mesh_required",
        }
    }
}

/// Caller policy used to prepare one exact execution intent.
#[derive(Clone, Debug, Default)]
pub struct PrepareExecutionOptionsV1 {
    pub route: Option<String>,
    pub route_policy: Option<RoutePolicy>,
    pub route_declarations: Vec<String>,
    pub excluded_project_paths: Vec<PathBuf>,
    pub parallel_auto: bool,
    /// True only when the user explicitly supplied `--mesh`. It remains
    /// distinct from the mesh-prefer configuration implied by parallel auto.
    pub explicit_mesh: bool,
    pub mesh: Option<MeshExecutionConfig>,
    pub ordinary_executor: Option<LocalOExecutorV1>,
    pub local_workers: Option<usize>,
    pub backend_grants: Vec<String>,
    pub shim_dir: PathBuf,
}

/// Exact, authority-free identities produced during ordinary-O preflight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrdinaryPlanIdentitiesV1 {
    pub source_sha256: String,
    pub oir_sha256: String,
    pub plan_sha256: String,
    pub analyzed_graph_sha256: String,
    pub execution_intent_sha256: String,
}

/// Exact, authority-free identities produced during project preflight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPlanIdentitiesV1 {
    pub bundle_sha256: String,
    pub logical_hgraph_sha256: String,
    pub deployment_plan_sha256: String,
}

/// A validated ordinary `.O` execution. It contains source-derived runtime
/// objects only in memory; the private run store receives identities/results,
/// never the source text.
#[derive(Debug)]
pub struct PreparedOrdinaryOExecutionV1 {
    pub input_path: PathBuf,
    pub source_bytes_len: u64,
    pub program: OIrProgram,
    pub identities: OrdinaryPlanIdentitiesV1,
    pub execution_intent: ExecutionIntentV1,
    pub executor: LocalOExecutorV1,
    pub parallel_auto: bool,
    pub local_workers: Option<usize>,
    pub backend_grants: Vec<String>,
    pub shim_dir: PathBuf,
    static_plan: String,
}

/// A validated project execution over a directory or lifted bundle.
#[derive(Debug)]
pub struct PreparedProjectExecutionV1 {
    pub input_kind: IntentInputKindV1,
    pub input_path: PathBuf,
    pub bundle: ProjectBundle,
    pub identities: ProjectPlanIdentitiesV1,
    pub route: Option<String>,
    pub policy: Option<RoutePolicy>,
    pub selected_target: String,
    pub effective_policy: String,
    pub route_declaration_sha256: Vec<String>,
    pub parallel_auto: bool,
    pub executor: ProjectExecutorV1,
    pub mesh: Option<MeshExecutionConfig>,
    static_plan: String,
}

#[derive(Debug)]
pub enum PreparedExecutionIntentV1 {
    OrdinaryO(PreparedOrdinaryOExecutionV1),
    Project(PreparedProjectExecutionV1),
}

impl PreparedExecutionIntentV1 {
    pub const fn input_kind(&self) -> IntentInputKindV1 {
        match self {
            Self::OrdinaryO(_) => IntentInputKindV1::OrdinaryO,
            Self::Project(project) => project.input_kind,
        }
    }

    pub fn input_path(&self) -> &Path {
        match self {
            Self::OrdinaryO(ordinary) => &ordinary.input_path,
            Self::Project(project) => &project.input_path,
        }
    }

    pub fn input_digest(&self) -> &str {
        match self {
            Self::OrdinaryO(ordinary) => &ordinary.identities.source_sha256,
            Self::Project(project) => &project.identities.bundle_sha256,
        }
    }

    pub fn engine_token(&self) -> &'static str {
        match self {
            Self::OrdinaryO(ordinary) => ordinary.executor.token(),
            Self::Project(project) => project.executor.token(),
        }
    }

    /// Render the exact static OIR/HGraph or Project HGraph/deployment view.
    /// This performs no discovery, state access, placement, or execution.
    pub fn static_plan(&self) -> &str {
        match self {
            Self::OrdinaryO(ordinary) => &ordinary.static_plan,
            Self::Project(project) => &project.static_plan,
        }
    }

    /// Project the source/bundle binding retained by the private record store.
    pub fn run_input_identity(&self) -> RunInputIdentityV1 {
        RunInputIdentityV1 {
            kind: match self.input_kind() {
                IntentInputKindV1::OrdinaryO => RunInputKindV1::OrdinaryO,
                IntentInputKindV1::ProjectDirectory => RunInputKindV1::ProjectDirectory,
                IntentInputKindV1::LiftedProject => RunInputKindV1::LiftedProjectBundle,
            },
            path: self.input_path().to_path_buf(),
            digest_sha256: self.input_digest().to_string(),
        }
    }

    pub fn run_plan_identities(&self) -> PlanIdentitiesV1 {
        match self {
            Self::OrdinaryO(ordinary) => PlanIdentitiesV1 {
                oir_sha256: Some(ordinary.identities.oir_sha256.clone()),
                execution_plan_sha256: Some(ordinary.identities.plan_sha256.clone()),
                hgraph_sha256: Some(ordinary.identities.analyzed_graph_sha256.clone()),
                execution_intent_sha256: Some(ordinary.identities.execution_intent_sha256.clone()),
                deployment_sha256: None,
            },
            Self::Project(project) => PlanIdentitiesV1 {
                oir_sha256: None,
                execution_plan_sha256: None,
                hgraph_sha256: Some(project.identities.logical_hgraph_sha256.clone()),
                execution_intent_sha256: None,
                deployment_sha256: Some(project.identities.deployment_plan_sha256.clone()),
            },
        }
    }

    pub fn run_intent_observation(&self) -> ExecutionIntentObservationV1 {
        match self {
            Self::OrdinaryO(ordinary) => ExecutionIntentObservationV1 {
                engine: ordinary.executor.token().to_string(),
                target: Some("local".to_string()),
                selected_route: None,
                route_policy: None,
                route_declarations: Vec::new(),
                parallel_policy: if ordinary.parallel_auto {
                    "auto_local_hgraph"
                } else {
                    "local"
                }
                .to_string(),
                local_worker_limit: ordinary
                    .local_workers
                    .and_then(|workers| u32::try_from(workers).ok()),
                mesh_mode: None,
                mesh_max_retries: None,
                mesh_fallback: None,
                mesh_discovery_timeout_ms: None,
                mesh_closed_registry: None,
                mesh_peer_root: None,
            },
            Self::Project(project) => {
                let mesh_mode = project.mesh.as_ref().map(|mesh| match mesh.requirement {
                    MeshRequirement::Prefer => "prefer".to_string(),
                    MeshRequirement::Required => "required".to_string(),
                });
                ExecutionIntentObservationV1 {
                    engine: project.executor.token().to_string(),
                    target: Some(project.selected_target.clone()),
                    selected_route: project.route.clone(),
                    route_policy: Some(project.effective_policy.clone()),
                    // Hashes bind explicit overrides without retaining command
                    // text or environment values from the declaration.
                    route_declarations: project.route_declaration_sha256.clone(),
                    parallel_policy: if project.parallel_auto {
                        "auto"
                    } else {
                        "project_policy"
                    }
                    .to_string(),
                    local_worker_limit: None,
                    mesh_mode,
                    mesh_max_retries: project.mesh.as_ref().map(|mesh| mesh.max_retries),
                    mesh_fallback: project.mesh.as_ref().map(|mesh| {
                        match mesh.local_fallback {
                            MeshLocalFallback::PreSend => "pre_send",
                            MeshLocalFallback::Idempotent => "idempotent",
                            MeshLocalFallback::Never => "never",
                        }
                        .to_string()
                    }),
                    mesh_discovery_timeout_ms: project.mesh.as_ref().map(|mesh| {
                        mesh.discovery_timeout
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX)
                    }),
                    mesh_closed_registry: project.mesh.as_ref().map(|mesh| !mesh.discover_lan),
                    mesh_peer_root: project
                        .mesh
                        .as_ref()
                        .and_then(|mesh| mesh.peer_root.clone()),
                }
            }
        }
    }

    /// Construct the small pre-execution seed written only after preflight.
    pub fn run_attempt_seed(&self, started_unix_nanos: u64) -> Result<RunAttemptSeedV1> {
        let seed = RunAttemptSeedV1 {
            input: self.run_input_identity(),
            intent: self.run_intent_observation(),
            plan: self.run_plan_identities(),
            started_unix_nanos,
        };
        seed.validate().map_err(anyhow::Error::msg)?;
        Ok(seed)
    }
}

/// Serializable projection of the evaluator's local lifecycle trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OExecutionTraceV1 {
    pub schema: String,
    pub events: Vec<OExecutionTraceEventV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum OExecutionTraceEventV1 {
    NodeReady {
        plan_node: usize,
    },
    NodeStarted {
        plan_node: usize,
    },
    NodeFinished {
        plan_node: usize,
        value_type: String,
        fingerprint: Option<String>,
    },
    NodeFailed {
        plan_node: usize,
        message: String,
    },
    NodeDiscarded {
        plan_node: usize,
        reason: String,
    },
}

impl OExecutionTraceV1 {
    pub fn from_execution_trace(trace: &ExecutionTrace) -> Self {
        let events = trace
            .events
            .iter()
            .map(|event| match event {
                TraceEvent::NodeReady(id) => Self::ready(id.0),
                TraceEvent::NodeStarted(id) => Self::started(id.0),
                TraceEvent::NodeFinished {
                    id,
                    value_type,
                    fingerprint,
                } => OExecutionTraceEventV1::NodeFinished {
                    plan_node: id.0,
                    value_type: value_type.clone(),
                    fingerprint: fingerprint.clone(),
                },
                TraceEvent::NodeFailed { id, message } => OExecutionTraceEventV1::NodeFailed {
                    plan_node: id.0,
                    message: message.clone(),
                },
                TraceEvent::NodeDiscarded { id, reason } => OExecutionTraceEventV1::NodeDiscarded {
                    plan_node: id.0,
                    reason: reason.clone(),
                },
            })
            .collect();
        Self {
            schema: O_EXECUTION_TRACE_SCHEMA_V1.to_string(),
            events,
        }
    }

    const fn ready(plan_node: usize) -> OExecutionTraceEventV1 {
        OExecutionTraceEventV1::NodeReady { plan_node }
    }

    const fn started(plan_node: usize) -> OExecutionTraceEventV1 {
        OExecutionTraceEventV1::NodeStarted { plan_node }
    }

    /// Strict structural validation for persisted evaluator observations.
    pub fn validate(&self) -> Result<()> {
        if self.schema != O_EXECUTION_TRACE_SCHEMA_V1 {
            bail!(
                "unsupported evaluator trace schema `{}` (expected {O_EXECUTION_TRACE_SCHEMA_V1})",
                self.schema
            );
        }
        if self.events.len() > 1_000_000 {
            bail!("evaluator trace exceeds the 1000000-event validation bound");
        }
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum State {
            Ready,
            Started,
            Terminal,
        }
        let mut states = BTreeMap::<usize, State>::new();
        for event in &self.events {
            let (plan_node, next, text) = match event {
                OExecutionTraceEventV1::NodeReady { plan_node } => (*plan_node, State::Ready, None),
                OExecutionTraceEventV1::NodeStarted { plan_node } => {
                    (*plan_node, State::Started, None)
                }
                OExecutionTraceEventV1::NodeFinished {
                    plan_node,
                    value_type,
                    fingerprint,
                } => {
                    validate_trace_text(value_type, "value type")?;
                    if let Some(fingerprint) = fingerprint {
                        validate_trace_text(fingerprint, "fingerprint")?;
                    }
                    (*plan_node, State::Terminal, None)
                }
                OExecutionTraceEventV1::NodeFailed { plan_node, message } => (
                    *plan_node,
                    State::Terminal,
                    Some((message, "failure message")),
                ),
                OExecutionTraceEventV1::NodeDiscarded { plan_node, reason } => (
                    *plan_node,
                    State::Terminal,
                    Some((reason, "discard reason")),
                ),
            };
            if let Some((text, label)) = text {
                validate_trace_text(text, label)?;
            }
            let prior = states.get(&plan_node).copied();
            let valid = matches!(
                (prior, next),
                (None, State::Ready)
                    | (Some(State::Ready), State::Started)
                    | (Some(State::Started), State::Terminal)
            );
            if !valid {
                bail!("invalid evaluator trace lifecycle for plan node P{plan_node}");
            }
            states.insert(plan_node, next);
        }
        Ok(())
    }
}

impl OrdinaryExecutionTraceV1 {
    /// Convert the intent executor's versioned projection into the canonical
    /// run-attachment projection without consulting live evaluator state.
    pub fn from_intent_trace(
        trace: &OExecutionTraceV1,
        identities: &OrdinaryPlanIdentitiesV1,
    ) -> Result<Self> {
        trace.validate()?;
        let events = trace
            .events
            .iter()
            .map(|event| match event {
                OExecutionTraceEventV1::NodeReady { plan_node } => {
                    OrdinaryTraceEventV1::NodeReady {
                        node: *plan_node as u64,
                    }
                }
                OExecutionTraceEventV1::NodeStarted { plan_node } => {
                    OrdinaryTraceEventV1::NodeStarted {
                        node: *plan_node as u64,
                    }
                }
                OExecutionTraceEventV1::NodeFinished {
                    plan_node,
                    value_type,
                    fingerprint,
                } => OrdinaryTraceEventV1::NodeFinished {
                    node: *plan_node as u64,
                    value_type: value_type.clone(),
                    fingerprint: fingerprint.clone(),
                },
                OExecutionTraceEventV1::NodeFailed { plan_node, message } => {
                    OrdinaryTraceEventV1::NodeFailed {
                        node: *plan_node as u64,
                        message: message.clone(),
                    }
                }
                OExecutionTraceEventV1::NodeDiscarded { plan_node, reason } => {
                    OrdinaryTraceEventV1::NodeDiscarded {
                        node: *plan_node as u64,
                        reason: reason.clone(),
                    }
                }
            })
            .collect();
        let converted = Self {
            schema: Self::SCHEMA.to_string(),
            input_sha256: identities.source_sha256.clone(),
            oir_sha256: identities.oir_sha256.clone(),
            execution_plan_sha256: identities.plan_sha256.clone(),
            hgraph_sha256: identities.analyzed_graph_sha256.clone(),
            execution_intent_sha256: identities.execution_intent_sha256.clone(),
            events,
        };
        converted.validate().map_err(anyhow::Error::msg)?;
        Ok(converted)
    }
}

fn validate_trace_text(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.contains('\0') || value.len() > 1_048_576 {
        bail!("evaluator trace {label} is empty, contains NUL, or exceeds 1 MiB");
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrdinaryOExecutionOutcomeV1 {
    pub value: OValue,
    pub elapsed_ns: u128,
    pub trace: OExecutionTraceV1,
}

/// Ordinary evaluator failure retaining the lifecycle trace produced before
/// the failure. It can be downcast from `anyhow::Error` by recorders.
#[derive(Debug)]
pub struct OrdinaryOExecutionErrorV1 {
    message: String,
    pub trace: OExecutionTraceV1,
}

impl OrdinaryOExecutionErrorV1 {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for OrdinaryOExecutionErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OrdinaryOExecutionErrorV1 {}

#[derive(Debug)]
pub struct ProjectExecutionObservationV1 {
    pub results: Vec<OExecutionResult>,
    pub project_trace: Option<ProjectAttemptTrace>,
    pub mesh_trace: Option<MeshExecutionTraceV1>,
    pub trace_unavailable_reason: Option<String>,
}

#[derive(Debug)]
pub enum ExecutionObservationV1 {
    OrdinaryO(OrdinaryOExecutionOutcomeV1),
    Project(ProjectExecutionObservationV1),
}

fn inspect_ordinary_source(
    source: &str,
) -> Result<(OIrProgram, crate::ir::ExecutionPlan, crate::hgraph::HGraph)> {
    let backends = BackendRegistry::global().registered_backend_tags();
    let mut parser = Parser::new(strip_shebang(source), &backends);
    let nodes = parser.parse().context("failed to parse .O source")?;
    let program = OIrProgram::lower(&nodes);
    let plan = program.plan();
    plan.validate(program.nodes.len())
        .map_err(anyhow::Error::msg)
        .context("invalid OIR execution plan")?;
    let graph = program
        .hgraph_for_plan(&plan)
        .map_err(anyhow::Error::msg)
        .context("failed to build HGraph for IR target")?;
    Ok((program, plan, graph))
}

/// Exact non-executing ordinary-O renderer shared with `olangc --target ir`.
pub fn render_ordinary_static_plan(source: &str) -> Result<String> {
    let (program, _plan, graph) = inspect_ordinary_source(source)?;
    Ok(format!(
        "{}\n{}",
        program.to_text(),
        graph.to_execution_text()
    ))
}

fn render_project_hgraph_static_plan(project: &crate::project::ProjectHGraph) -> Result<String> {
    let logical = project
        .logical_v1()
        .context("failed to normalize LogicalHGraphV1")?;
    let logical_digest = logical
        .digest()
        .context("failed to digest LogicalHGraphV1")?;
    let deployment = DeploymentPlanV1::hosted(&logical)
        .context("failed to construct hosted DeploymentPlanV1")?;
    let deployment_digest = deployment
        .digest()
        .context("failed to digest hosted DeploymentPlanV1")?;
    Ok(format!(
        "; LogicalHGraphV1\nlogical schema={} sha256={}\n; DeploymentPlanV1\ndeployment schema={} sha256={}\n{}{}",
        logical.schema_version,
        logical_digest.as_sha256(),
        deployment.schema_version,
        deployment_digest.as_sha256(),
        project.to_text(),
        deployment.to_text()
    ))
}

/// Exact non-executing project renderer shared with `olangc --target ir`.
pub fn render_project_static_plan(
    bundle: &ProjectBundle,
    route: Option<&str>,
    policy: Option<RoutePolicy>,
) -> Result<String> {
    let project = build_project_hgraph(bundle, route, policy)
        .map_err(anyhow::Error::msg)
        .context("failed to build logical project HGraph")?;
    render_project_hgraph_static_plan(&project)
}

/// Classify and completely preflight one supported execution input.
///
/// The returned value is the boundary after which the run-record store may
/// allocate an attempt. Every filesystem read, source parse, route override,
/// route selection, and graph/deployment identity check needed to know what
/// can execute has already completed successfully.
pub fn prepare_execution_intent(
    input: &Path,
    mut options: PrepareExecutionOptionsV1,
) -> Result<PreparedExecutionIntentV1> {
    if options.parallel_auto && options.explicit_mesh {
        bail!(
            "--parallel auto conflicts with explicit --mesh; use --mesh=required when remote execution is mandatory"
        );
    }
    if options.local_workers == Some(0) {
        bail!("--workers must be at least 1");
    }
    if options
        .local_workers
        .is_some_and(|workers| workers > u32::MAX as usize)
    {
        bail!(
            "--workers exceeds the versioned run-record limit of {}",
            u32::MAX
        );
    }
    if options.explicit_mesh && options.mesh.is_none() {
        bail!("internal intent error: explicit mesh selection has no mesh configuration");
    }
    validate_mesh_preflight(options.mesh.as_ref())?;

    let metadata = fs::metadata(input)
        .with_context(|| format!("failed to inspect input {}", input.display()))?;
    let canonical = input
        .canonicalize()
        .with_context(|| format!("failed to canonicalize input {}", input.display()))?;
    if canonical.to_str().is_none() {
        bail!("input path is not valid UTF-8: {}", canonical.display());
    }

    if metadata.is_dir() {
        return prepare_project_directory(&canonical, options);
    }
    if !metadata.is_file() {
        bail!(
            "unsupported input {}: expected an ordinary .O file, a project directory, or a lifted project .O bundle",
            canonical.display()
        );
    }
    if canonical
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("O")
    {
        bail!(
            "unsupported standalone foreign file {}. Bundle the heterogeneous codebase first with `o-link --project <DIRECTORY> -o project.O`, then run the directory or lifted project.O",
            canonical.display()
        );
    }

    let source = fs::read(&canonical)
        .with_context(|| format!("failed to read input source {}", canonical.display()))?;
    let source_text = std::str::from_utf8(&source)
        .with_context(|| format!("input source is not UTF-8: {}", canonical.display()))?;

    // Lifted bundle detection intentionally precedes ordinary O parsing: the
    // payload is inert O text and must be executed through project routes.
    if crate::project::lower::has_embedded_bundle(source_text) {
        let mut bundle = crate::project::lower::extract_bundle_from_o(source_text)
            .with_context(|| format!("failed to load lifted project {}", canonical.display()))?;
        crate::project::manifest::apply_cli_overrides(&mut bundle, &options.route_declarations)?;
        crate::project::finalize_default(&mut bundle);
        return prepare_project_bundle(
            IntentInputKindV1::LiftedProject,
            canonical,
            bundle,
            options,
        );
    }

    prepare_ordinary_o(canonical, &source, &mut options)
}

fn prepare_project_directory(
    input: &Path,
    options: PrepareExecutionOptionsV1,
) -> Result<PreparedExecutionIntentV1> {
    let name = crate::project::name_from_path(input);
    let bundle = crate::project::assemble_excluding(
        input,
        &name,
        &options.route_declarations,
        &options.excluded_project_paths,
    )
    .with_context(|| format!("failed to assemble project {}", input.display()))?;
    prepare_project_bundle(
        IntentInputKindV1::ProjectDirectory,
        input.to_path_buf(),
        bundle,
        options,
    )
}

fn prepare_ordinary_o(
    input: PathBuf,
    source: &[u8],
    options: &mut PrepareExecutionOptionsV1,
) -> Result<PreparedExecutionIntentV1> {
    if options.route.is_some()
        || options.route_policy.is_some()
        || !options.route_declarations.is_empty()
    {
        bail!(
            "project route flags are not valid for ordinary .O input {}; use a project directory or lifted project bundle",
            input.display()
        );
    }
    if options.mesh.is_some() || options.explicit_mesh {
        bail!(
            "mesh flags are not valid for ordinary .O input {}; ordinary OIR execution uses only the local HGraph worker pool",
            input.display()
        );
    }
    let source_text = std::str::from_utf8(source).context("ordinary .O source is not UTF-8")?;
    for grant in &options.backend_grants {
        crate::eval::validate_backend_grant_spec(grant)
            .with_context(|| format!("invalid backend grant `{grant}`"))?;
    }
    let executable_source = strip_shebang(source_text);
    let (program, plan, graph) = inspect_ordinary_source(executable_source)?;
    let static_plan = format!("{}\n{}", program.to_text(), graph.to_execution_text());
    let mut solved_graph = graph;
    crate::hgraph::solve::solve_types(&mut solved_graph)
        .context("failed to solve HGraph type and fidelity constraints")?;
    let execution_intent =
        ExecutionIntentV1::compile(source, &program, &plan, &solved_graph, Policy::Eager)
            .context("failed to compile ordinary O execution identity")?;

    let executor = select_ordinary_executor(options)?;
    let source_bytes_len = u64::try_from(source.len()).unwrap_or(u64::MAX);
    let identities = OrdinaryPlanIdentitiesV1 {
        source_sha256: execution_intent.source_sha256.clone(),
        oir_sha256: execution_intent.oir_sha256.clone(),
        plan_sha256: execution_intent.plan_sha256.clone(),
        analyzed_graph_sha256: execution_intent.analyzed_graph_sha256.clone(),
        execution_intent_sha256: execution_intent.execution_intent_sha256.clone(),
    };
    Ok(PreparedExecutionIntentV1::OrdinaryO(
        PreparedOrdinaryOExecutionV1 {
            input_path: input,
            source_bytes_len,
            program,
            identities,
            execution_intent,
            executor,
            parallel_auto: options.parallel_auto,
            local_workers: options.local_workers,
            backend_grants: std::mem::take(&mut options.backend_grants),
            shim_dir: std::mem::take(&mut options.shim_dir),
            static_plan,
        },
    ))
}

fn select_ordinary_executor(options: &PrepareExecutionOptionsV1) -> Result<LocalOExecutorV1> {
    if options.parallel_auto {
        if options.ordinary_executor == Some(LocalOExecutorV1::ForcedSerial) {
            bail!("--parallel auto conflicts with --executor serial");
        }
        return Ok(LocalOExecutorV1::ForcedGraph);
    }
    if let Some(executor) = options.ordinary_executor {
        return Ok(executor);
    }
    match std::env::var("O_EXECUTOR") {
        Ok(value) if value.eq_ignore_ascii_case("serial") => Ok(LocalOExecutorV1::ForcedSerial),
        Ok(value) if value.eq_ignore_ascii_case("graph") => Ok(LocalOExecutorV1::ConfiguredGraph),
        Ok(value) => bail!("unknown O_EXECUTOR value `{value}`; expected graph or serial"),
        Err(std::env::VarError::NotPresent) => Ok(LocalOExecutorV1::ConfiguredGraph),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("O_EXECUTOR is not valid Unicode; expected graph or serial")
        }
    }
}

fn prepare_project_bundle(
    kind: IntentInputKindV1,
    input_path: PathBuf,
    bundle: ProjectBundle,
    mut options: PrepareExecutionOptionsV1,
) -> Result<PreparedExecutionIntentV1> {
    if options.local_workers.is_some()
        || options.ordinary_executor.is_some()
        || !options.backend_grants.is_empty()
    {
        bail!(
            "ordinary evaluator flags (--workers, --executor, --backend-grant) are not valid for project input {}",
            input_path.display()
        );
    }
    if options.route_policy.is_some() && options.route.is_none() {
        bail!("--route-policy/--routes-policy requires --route");
    }
    if options.route.is_none()
        && options.route_policy.is_none()
        && bundle.resolved_default().is_none()
    {
        bail!(
            "no unambiguous default route; select one with --route <ID>\n{}",
            bundle.route_table()
        );
    }

    if options.parallel_auto && options.mesh.is_none() {
        options.mesh = Some(MeshExecutionConfig::default());
    }
    validate_mesh_preflight(options.mesh.as_ref())?;
    let executor = match options.mesh.as_ref().map(|mesh| mesh.requirement) {
        Some(MeshRequirement::Prefer) => ProjectExecutorV1::MeshPrefer,
        Some(MeshRequirement::Required) => ProjectExecutorV1::MeshRequired,
        None => match std::env::var_os(PROJECT_EXECUTOR_ENV) {
            None => ProjectExecutorV1::Compatibility,
            Some(value) if value == "hgraph" => ProjectExecutorV1::Hgraph,
            Some(value) => bail!(
                "unsupported {PROJECT_EXECUTOR_ENV} value `{}`; expected hgraph or an unset variable",
                value.to_string_lossy()
            ),
        },
    };

    let route_declaration_sha256 = options
        .route_declarations
        .iter()
        .map(|declaration| {
            format!(
                "sha256:{}",
                hex::encode(Sha256::digest(declaration.as_bytes()))
            )
        })
        .collect::<Vec<_>>();
    let project = build_project_hgraph(
        &bundle,
        options.route.as_deref(),
        options.route_policy.clone(),
    )
    .map_err(anyhow::Error::msg)
    .context("failed to build logical project HGraph")?;
    validate_project_executor_preflight(&bundle, &project, executor)?;
    let logical = project
        .logical_v1()
        .context("failed to normalize LogicalHGraphV1")?;
    let logical_digest = logical
        .digest()
        .context("failed to digest LogicalHGraphV1")?;
    let deployment = DeploymentPlanV1::hosted(&logical)
        .context("failed to construct hosted DeploymentPlanV1")?;
    let deployment_digest = deployment
        .digest()
        .context("failed to digest hosted DeploymentPlanV1")?;
    let bundle_sha256 = project.plan.bundle_digest.clone();
    let selected_target = project.plan.target.clone();
    let effective_policy = project.plan.policy.token();
    let static_plan = render_project_hgraph_static_plan(&project)?;
    let identities = ProjectPlanIdentitiesV1 {
        bundle_sha256,
        logical_hgraph_sha256: logical_digest.as_sha256().to_string(),
        deployment_plan_sha256: deployment_digest.as_sha256().to_string(),
    };
    Ok(PreparedExecutionIntentV1::Project(
        PreparedProjectExecutionV1 {
            input_kind: kind,
            input_path,
            bundle,
            identities,
            route: options.route,
            policy: options.route_policy,
            selected_target,
            effective_policy,
            route_declaration_sha256,
            parallel_auto: options.parallel_auto,
            executor,
            mesh: options.mesh,
            static_plan,
        },
    ))
}

fn validate_project_executor_preflight(
    bundle: &ProjectBundle,
    project: &crate::project::ProjectHGraph,
    executor: ProjectExecutorV1,
) -> Result<()> {
    let potential_route_executions =
        potential_route_execution_count(bundle, &project.plan.alternatives)?;
    RunOptions::default()
        .limits
        .validate_route_execution_set(potential_route_executions)?;

    if executor == ProjectExecutorV1::Hgraph {
        match &project.plan.policy {
            RoutePolicy::Explicit(_) | RoutePolicy::Default => {
                if project.plan.alternatives.len() != 1 {
                    bail!(
                        "project HGraph executor requires exactly one resolved alternative for policy `{}`, found {}",
                        project.plan.policy.token(),
                        project.plan.alternatives.len()
                    );
                }
            }
            RoutePolicy::Fallback | RoutePolicy::AnySuccess => {}
            policy => bail!(
                "project HGraph executor does not support policy `{}`; supported policies are explicit, default, fallback, and any_success",
                policy.token()
            ),
        }
    }
    Ok(())
}

fn validate_mesh_preflight(mesh: Option<&MeshExecutionConfig>) -> Result<()> {
    let Some(mesh) = mesh else {
        return Ok(());
    };
    if mesh.discovery_timeout.is_zero() || mesh.discovery_timeout.as_secs() > 60 {
        bail!("mesh discovery timeout must be between 1 ms and 60 seconds");
    }
    if mesh.max_retries > 64 {
        bail!("mesh retries may not exceed 64");
    }
    Ok(())
}

fn strip_shebang(source: &str) -> &str {
    if source.starts_with("#!") {
        source
            .find('\n')
            .map(|newline| &source[newline + 1..])
            .unwrap_or_default()
    } else {
        source
    }
}

/// Execute one already-preflighted ordinary-O intent in process.
pub fn execute_prepared_ordinary_o(
    prepared: &PreparedOrdinaryOExecutionV1,
) -> Result<OrdinaryOExecutionOutcomeV1> {
    let backends = BackendRegistry::global().registered_backend_tags();
    let mut evaluator =
        Evaluator::new(prepared.shim_dir.clone()).with_registered_backends(backends);
    if let Some(workers) = prepared.local_workers {
        evaluator = evaluator.with_local_worker_parallelism(workers);
    }
    let mut scope = HashMap::new();
    for grant in &prepared.backend_grants {
        evaluator
            .install_backend_grant(grant, &mut scope)
            .with_context(|| format!("failed to install backend grant `{grant}`"))?;
    }

    let started = Instant::now();
    let result = match prepared.executor {
        LocalOExecutorV1::ConfiguredGraph | LocalOExecutorV1::ForcedGraph => {
            evaluator.eval_ir_program_graph_with_scope(&prepared.program, &mut scope)
        }
        LocalOExecutorV1::ForcedSerial => {
            evaluator.eval_ir_program_serial_with_scope(&prepared.program, &mut scope)
        }
    };
    let elapsed_ns = started.elapsed().as_nanos();
    let trace = evaluator
        .last_execution_trace()
        .map(OExecutionTraceV1::from_execution_trace)
        .unwrap_or_else(|| OExecutionTraceV1 {
            schema: O_EXECUTION_TRACE_SCHEMA_V1.to_string(),
            events: Vec::new(),
        });
    trace
        .validate()
        .context("evaluator returned an invalid lifecycle trace projection")?;
    match result {
        Ok(value) => Ok(OrdinaryOExecutionOutcomeV1 {
            value,
            elapsed_ns,
            trace,
        }),
        Err(error) => Err(anyhow::Error::new(OrdinaryOExecutionErrorV1 {
            message: format!("{error:#}"),
            trace,
        })
        .context("failed to evaluate .O document")),
    }
}

/// Execute a preflighted project through its selected local executor.
/// Mesh execution is handled by [`execute_prepared_project`] after the
/// observed mesh API attaches placement/retry history.
fn execute_prepared_local_project(
    prepared: &PreparedProjectExecutionV1,
) -> Result<ProjectExecutionObservationV1> {
    let ConfiguredProjectExecution { results, trace } = execute_selection_with_configured_executor(
        &prepared.bundle,
        prepared.route.as_deref(),
        prepared.policy.clone(),
        &RunOptions::default(),
    )?;
    let trace_unavailable_reason = trace.is_none().then(|| {
        "compatibility project executor does not produce a Project HGraph attempt trace".to_string()
    });
    Ok(ProjectExecutionObservationV1 {
        results,
        project_trace: trace,
        mesh_trace: None,
        trace_unavailable_reason,
    })
}

/// Execute one preflighted project without changing the selected local/mesh
/// policy. The mesh branch never launches a node; it can use only the
/// authenticated peers returned by the existing client resolver and the
/// configured local fallback rules.
pub fn execute_prepared_project(
    prepared: &PreparedProjectExecutionV1,
) -> Result<ProjectExecutionObservationV1> {
    let Some(mesh) = prepared.mesh.as_ref() else {
        return execute_prepared_local_project(prepared);
    };
    let MeshExecutionOutcome { execution, trace } = execute_mesh_selection_observed(
        &prepared.bundle,
        prepared.route.as_deref(),
        prepared.policy.clone(),
        &RunOptions::default(),
        mesh,
    )?;
    Ok(ProjectExecutionObservationV1 {
        results: execution.results,
        project_trace: execution.trace,
        mesh_trace: Some(trace),
        trace_unavailable_reason: None,
    })
}

/// Execute any preflighted intent through its already-selected engine.
pub fn execute_prepared_intent(
    prepared: &PreparedExecutionIntentV1,
) -> Result<ExecutionObservationV1> {
    match prepared {
        PreparedExecutionIntentV1::OrdinaryO(ordinary) => {
            execute_prepared_ordinary_o(ordinary).map(ExecutionObservationV1::OrdinaryO)
        }
        PreparedExecutionIntentV1::Project(project) => {
            execute_prepared_project(project).map(ExecutionObservationV1::Project)
        }
    }
}

/// Produce the bounded live placement view for a preflighted plan.
///
/// Ordinary `.O` inputs deliberately stop at local runtime/worker readiness.
/// Project inputs call the mesh's separate read-only discovery path, which can
/// issue only authenticated profile/capacity reads and cannot enroll, upload,
/// probe routes, reserve, fence, submit, cancel, or execute.
pub fn live_placement_preview(prepared: &PreparedExecutionIntentV1) -> Result<PlacementPreviewV1> {
    let worker_count = match prepared {
        PreparedExecutionIntentV1::OrdinaryO(ordinary) => ordinary
            .local_workers
            .or_else(|| std::thread::available_parallelism().ok().map(usize::from)),
        PreparedExecutionIntentV1::Project(_) => None,
    }
    .and_then(|workers| u32::try_from(workers).ok());
    let local = match prepared {
        PreparedExecutionIntentV1::OrdinaryO(ordinary) => {
            ordinary_local_readiness(ordinary, worker_count)
        }
        PreparedExecutionIntentV1::Project(_) => LocalReadinessPreviewV1 {
            runtime_ready: true,
            worker_count: None,
            detail: "project bundle and logical HGraph are prepared; route guards and command availability remain execution-time checks"
                .to_string(),
        },
    };

    let mut candidates = Vec::new();
    let mut explanation = Vec::new();
    if let PreparedExecutionIntentV1::Project(project) = prepared {
        let config = project
            .mesh
            .as_ref()
            .map(MeshReadOnlyDiscoveryConfig::from)
            .unwrap_or_default();
        let observed = observe_mesh_peers_read_only(&config)
            .context("read-only mesh placement preview failed")?;
        observed.validate()?;
        if let Some(error) = observed.lan_discovery_error {
            explanation.push(format!(
                "LAN endpoint-hint discovery was incomplete: {error}"
            ));
        }
        for peer in observed.peers {
            let authenticated = peer.profile.is_some() && peer.capacity.is_some();
            let detail = peer.detail.unwrap_or_else(|| {
                if peer.eligible {
                    "authenticated profile/capacity observation is eligible".to_string()
                } else if peer.rejection.is_some() {
                    "authenticated peer currently has no available actor slot".to_string()
                } else {
                    "pinned peer could not provide a complete read-only observation".to_string()
                }
            });
            candidates.push(PlacementCandidatePreviewV1 {
                node_id: peer.node_id,
                endpoint_hint: peer.selected_endpoint,
                available_slots: peer.capacity.map(|capacity| capacity.available_slots),
                observed_latency_micros: peer.observed_latency_micros,
                authenticated,
                eligible: peer.eligible,
                detail,
            });
        }
    } else {
        explanation.push(
            "ordinary OIR placement is local-only in V1; discovery and remote RPCs were not performed"
                .to_string(),
        );
    }

    let selected_node_id = candidates
        .iter()
        .filter(|candidate| candidate.eligible)
        .min_by_key(|candidate| {
            (
                std::cmp::Reverse(candidate.available_slots.unwrap_or(0)),
                candidate.observed_latency_micros.unwrap_or(u64::MAX),
                candidate.node_id.as_str(),
            )
        })
        .map(|candidate| candidate.node_id.clone());
    if let Some(selected) = &selected_node_id {
        explanation.push(format!(
            "capacity-first read-only ranking currently prefers authenticated peer {selected}; this is not a reservation or admission"
        ));
    } else if matches!(prepared, PreparedExecutionIntentV1::Project(_)) {
        explanation.push(
            "no already-pinned authenticated peer currently reports an available actor slot"
                .to_string(),
        );
    }

    let preview = PlacementPreviewV1 {
        schema: PLACEMENT_PREVIEW_SCHEMA_V1.to_string(),
        input: prepared.run_input_identity(),
        plan: prepared.run_plan_identities(),
        mode: PlacementPlanningModeV1::LiveReadOnly,
        local,
        candidates,
        selected_node_id,
        explanation,
        integrity: RUN_RECORD_INTEGRITY_V1.to_string(),
    };
    preview.validate().map_err(anyhow::Error::msg)?;
    Ok(preview)
}

fn ordinary_local_readiness(
    ordinary: &PreparedOrdinaryOExecutionV1,
    worker_count: Option<u32>,
) -> LocalReadinessPreviewV1 {
    let registry = BackendRegistry::global();
    let mut required = BTreeMap::<String, BackendAdapterKind>::new();
    for node in ordinary.program.flatten_for_plan() {
        if let OIr::Exec { backend, .. } = node {
            if backend.execution == ExecutionMode::Shim {
                required
                    .entry(backend.canonical.clone())
                    .or_insert_with(|| registry.adapter_for(&backend.canonical));
            }
        }
    }

    let mut failures = Vec::new();
    let mut needs_python = false;
    for (backend, adapter) in &required {
        if *adapter == BackendAdapterKind::LegacyPythonShim {
            needs_python = true;
            let shim = registry.resolve_shim_path(&ordinary.shim_dir, backend);
            match fs::metadata(&shim) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => failures.push(format!(
                    "required `{backend}` shim is not a regular file: {}",
                    shim.display()
                )),
                Err(error) => failures.push(format!(
                    "required `{backend}` shim is unavailable at {}: {error}",
                    shim.display()
                )),
            }
        }
        if let Err(error) = crate::runtime_exec::resolve_backend_launch_selection(backend) {
            failures.push(format!(
                "required `{backend}` runtime executable set is unavailable: {error:#}"
            ));
        }
    }
    if needs_python {
        if let Err(error) = which::which("python3") {
            failures.push(format!(
                "required Python shim launcher `python3` is unavailable: {error}"
            ));
        }
    }

    if failures.is_empty() {
        let backends = if required.is_empty() {
            "no hosted process backends are required by this OIR".to_string()
        } else {
            format!(
                "required hosted backend adapters are ready for [{}]",
                required.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        };
        LocalReadinessPreviewV1 {
            runtime_ready: true,
            worker_count,
            detail: format!(
                "local evaluator and HGraph worker configuration are ready; {backends}; no peer discovery was performed"
            ),
        }
    } else {
        LocalReadinessPreviewV1 {
            runtime_ready: false,
            worker_count,
            detail: failures.join("; "),
        }
    }
}

/// Parse the only supported retained-run selector forms.
pub fn parse_run_selector(value: &str) -> Result<RunSelectorV1> {
    if value == "last-run" {
        return Ok(RunSelectorV1::LastRun);
    }
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("run selector must be `last-run` or a 64-character lowercase hexadecimal run ID");
    }
    Ok(RunSelectorV1::RunId(value.to_string()))
}

/// Render a human causal narrative only after validating the retained record
/// and optional content-addressed trace attachment.
pub fn explain_verified_run(
    record: &RunRecordV1,
    trace: Option<&RunTraceAttachmentV1>,
) -> Result<String> {
    record
        .validate()
        .map_err(anyhow::Error::msg)
        .context("retained run record failed validation")?;
    if let Some(trace) = trace {
        trace
            .validate_for_record(record)
            .map_err(anyhow::Error::msg)
            .context("retained run trace failed record-bound validation")?;
    }

    let mut output = String::new();
    output.push_str(&format!(
        "Run {} (sequence {}) finished as {:?}.\n",
        record.run_id, record.sequence, record.disposition
    ));
    output.push_str(&format!(
        "Input {:?} at {} was content-bound by sha256:{}; source and bundle payload bytes were not retained.\n",
        record.input.kind,
        record.input.path.display(),
        record.input.digest_sha256
    ));
    output.push_str(&format!(
        "Preflight selected engine `{}` with parallel policy `{}`",
        record.intent.engine, record.intent.parallel_policy
    ));
    if let Some(mesh) = &record.intent.mesh_mode {
        output.push_str(&format!(" and mesh mode `{mesh}`"));
    }
    output.push_str(".\n");
    if let Some(target) = &record.intent.target {
        output.push_str(&format!("Resolved target: {target}"));
        if let Some(policy) = &record.intent.route_policy {
            output.push_str(&format!(" under route policy `{policy}`"));
        }
        output.push_str(".\n");
    }
    for (label, digest) in [
        ("OIR", record.plan.oir_sha256.as_deref()),
        ("HGraph", record.plan.hgraph_sha256.as_deref()),
        ("deployment", record.plan.deployment_sha256.as_deref()),
    ] {
        if let Some(digest) = digest {
            output.push_str(&format!("{label} identity: sha256:{digest}.\n"));
        }
    }
    for result in &record.route_results {
        let status = match result.exit_code {
            Some(0) if result.artifact_capture.is_complete() => "succeeded".to_string(),
            Some(code) => format!("settled with exit code {code}"),
            None => "settled without a process exit code".to_string(),
        };
        output.push_str(&format!(
            "Route `{}` {status}; stdout retained {} of {} bytes and stderr retained {} of {} bytes.\n",
            result.route_id,
            result.stdout.capture.retained_bytes,
            result.stdout.capture.total_observed_bytes,
            result.stderr.capture.retained_bytes,
            result.stderr.capture.total_observed_bytes
        ));
    }
    if let Some(failure) = &record.failure {
        output.push_str(&format!(
            "Failure stage `{}` reported: {}\n",
            failure.stage, failure.message
        ));
    }

    match trace.map(|attachment| &attachment.payload) {
        Some(RunTracePayloadV1::Ordinary(trace)) => {
            output.push_str(&format!(
                "The verified local evaluator attachment contains {} lifecycle events; no peer placement occurred.\n",
                trace.events.len()
            ));
            for event in &trace.events {
                match event {
                    OrdinaryTraceEventV1::NodeFinished {
                        node,
                        value_type,
                        fingerprint,
                    } => output.push_str(&format!(
                        "Local node P{node} finished with value type `{value_type}`{}.\n",
                        fingerprint
                            .as_ref()
                            .map(|value| format!(" and fingerprint `{value}`"))
                            .unwrap_or_default()
                    )),
                    OrdinaryTraceEventV1::NodeFailed { node, message } => output.push_str(
                        &format!("Local node P{node} failed: {message}\n"),
                    ),
                    OrdinaryTraceEventV1::NodeDiscarded { node, reason } => output.push_str(
                        &format!("Local node P{node} was discarded: {reason}\n"),
                    ),
                    OrdinaryTraceEventV1::NodeReady { .. }
                    | OrdinaryTraceEventV1::NodeStarted { .. } => {}
                }
            }
        }
        Some(RunTracePayloadV1::ProjectHgraph(trace)) => {
            output.push_str(&format!(
                "The verified Project HGraph attachment contains {} scheduler lifecycle events.\n",
                trace.events.len()
            ));
            for event in trace.events.iter().filter(|event| event.state.is_terminal()) {
                let subject = event
                    .route_id
                    .as_ref()
                    .map(|route| format!("route `{route}`"))
                    .unwrap_or_else(|| format!("operation `{}`", event.operation_label));
                output.push_str(&format!(
                    "Project {subject} reached `{}` at P{}",
                    project_attempt_state_token(event.state),
                    event.plan_node.0
                ));
                if let Some(outcome) = &event.outcome {
                    output.push_str(&format!(" with exit {:?}", outcome.exit_code));
                }
                if let Some(failure) = &event.failure_sha256 {
                    output.push_str(&format!(" and failure sha256:{failure}"));
                }
                output.push_str(".\n");
            }
        }
        Some(RunTracePayloadV1::ProjectMesh(trace)) => {
            let dispatched = trace
                .events
                .iter()
                .filter(|event| matches!(event, MeshTraceEventV1::Dispatched { .. }))
                .count();
            let migrated = trace
                .events
                .iter()
                .filter(|event| matches!(event, MeshTraceEventV1::Migrated { .. }))
                .count();
            let fallbacks = trace
                .events
                .iter()
                .filter(|event| matches!(event, MeshTraceEventV1::LocalFallback { .. }))
                .count();
            output.push_str(&format!(
                "The verified mesh attachment observed {} candidates, {dispatched} dispatches, {migrated} migrations, and {fallbacks} local fallbacks.\n",
                trace.candidates.len()
            ));
            for candidate in &trace.candidates {
                output.push_str(&format!(
                    "Mesh candidate `{}` was eligible={} with {} available slots at {}us latency{}: {}\n",
                    candidate.node_id,
                    candidate.eligible,
                    candidate.available_slots,
                    candidate.observed_latency_micros,
                    candidate
                        .address
                        .as_ref()
                        .map(|address| format!(" via {address}"))
                        .unwrap_or_default(),
                    candidate.detail
                ));
            }
            for event in &trace.events {
                match event {
                    MeshTraceEventV1::Dispatched {
                        route_id,
                        actor_id,
                        generation,
                        node_id,
                    } => output.push_str(&format!(
                        "Mesh dispatched route `{route_id}` actor {actor_id} generation {generation} to node `{node_id}`.\n"
                    )),
                    MeshTraceEventV1::Settled {
                        route_id,
                        actor_id,
                        generation,
                        node_id,
                        succeeded,
                    } => output.push_str(&format!(
                        "Mesh route `{route_id}` actor {actor_id} generation {generation} settled on node `{node_id}` with succeeded={succeeded}.\n"
                    )),
                    MeshTraceEventV1::AttemptFailed {
                        route_id,
                        actor_id,
                        generation,
                        node_id,
                        submitted,
                        delivery,
                        replay_contract,
                        reason,
                    } => output.push_str(&format!(
                        "Mesh route `{route_id}` actor {actor_id} generation {generation} failed on node `{node_id}` (submitted={submitted}, delivery={delivery}, replay={replay_contract}): {reason}\n"
                    )),
                    MeshTraceEventV1::Migrated {
                        route_id,
                        actor_id,
                        from_generation,
                        to_generation,
                        from_node_id,
                        to_node_id,
                        replay_contract,
                    } => output.push_str(&format!(
                        "Mesh migrated route `{route_id}` actor {actor_id} from node `{from_node_id}` generation {from_generation} to node `{to_node_id}` generation {to_generation} under replay contract `{replay_contract}`.\n"
                    )),
                    MeshTraceEventV1::RetryDenied {
                        route_id,
                        actor_id,
                        generation,
                        reason,
                    } => output.push_str(&format!(
                        "Mesh denied retry for route `{route_id}` actor {actor_id} after generation {generation}: {reason}\n"
                    )),
                    MeshTraceEventV1::LocalFallback {
                        route_id,
                        actor_id,
                        after_generation,
                        replay_contract,
                        reason,
                    } => output.push_str(&format!(
                        "Mesh selected local fallback for route `{route_id}` actor {actor_id} after generation {after_generation} under replay contract `{replay_contract}`: {reason}\n"
                    )),
                }
            }
        }
        None => match &record.trace {
            RunTraceBindingV1::Unavailable { reason } => {
                output.push_str(&format!("Execution trace unavailable: {reason}.\n"));
            }
            RunTraceBindingV1::Attached { .. } => output.push_str(
                "A verified trace attachment exists; use `o inspect last-run --trace` or repeat explain with trace resolution.\n",
            ),
        },
    }
    output.push_str(
        "Integrity: unsigned_observation. This narrative is not admission, a signature, an OWRECEIPT, or World authority.\n",
    );
    Ok(output)
}

fn project_attempt_state_token(state: ProjectAttemptState) -> &'static str {
    match state {
        ProjectAttemptState::Ready => "ready",
        ProjectAttemptState::Started => "started",
        ProjectAttemptState::Finished => "finished",
        ProjectAttemptState::SettledSuccess => "settled_success",
        ProjectAttemptState::SettledFailure => "settled_failure",
        ProjectAttemptState::Skipped => "skipped",
        ProjectAttemptState::Aborted => "aborted",
    }
}

/// Render the non-colour evaluator stdout representation used by the unified
/// front door. Text/HTML remain raw, while structured values preserve the
/// historical deterministic formatting of the `O` binary.
pub fn render_ordinary_value_stdout(value: &OValue) -> Vec<u8> {
    render_ordinary_value_stdout_with_color(value, false)
}

/// Render the evaluator's exact interactive value presentation. The caller
/// supplies terminal capability so JSON/file output remains byte-stable.
pub fn render_ordinary_value_stdout_with_color(value: &OValue, color: bool) -> Vec<u8> {
    match value {
        OValue::Text { v } => v.utf8.as_bytes().to_vec(),
        OValue::Html { v } => v.as_bytes().to_vec(),
        other => format!("{}\n", format_ordinary_value(other, color, 0)).into_bytes(),
    }
}

/// Format one value at a structured nesting depth. This is shared with the
/// direct evaluator's REPL preview so `O` and the unified front door cannot
/// drift in their human output grammar.
pub fn format_ordinary_value(value: &OValue, color: bool, depth: usize) -> String {
    match value {
        OValue::Null => {
            if color {
                "\x1b[2mnull\x1b[0m".to_string()
            } else {
                "null".to_string()
            }
        }
        OValue::Bool { v } => colorize(v, "\x1b[33m", color),
        OValue::Text { v } => {
            if color {
                format!("\x1b[32m{:?}\x1b[0m", v.utf8)
            } else {
                format!("{:?}", v.utf8)
            }
        }
        OValue::Html { v } => {
            if color {
                format!("\x1b[32m{v:?}\x1b[0m")
            } else {
                format!("{v:?}")
            }
        }
        OValue::List { v } => format_ordinary_list(v, color, depth),
        OValue::Map { v } => format_ordinary_map(v, color, depth),
        other => {
            let value_type = other.type_name();
            if color {
                format!("\x1b[90m[{value_type}]\x1b[0m {other}")
            } else {
                format!("[{value_type}] {other}")
            }
        }
    }
}

fn colorize(value: &dyn fmt::Display, code: &str, color: bool) -> String {
    if color {
        format!("{code}{value}\x1b[0m")
    } else {
        value.to_string()
    }
}

fn format_ordinary_list(items: &[OValue], color: bool, depth: usize) -> String {
    if items.is_empty() {
        return if color {
            "\x1b[90m[]\x1b[0m".to_string()
        } else {
            "[]".to_string()
        };
    }
    let indent = "  ".repeat(depth + 1);
    let close = "  ".repeat(depth);
    let (open, close_bracket) = if color {
        ("\x1b[90m[\x1b[0m", "\x1b[90m]\x1b[0m")
    } else {
        ("[", "]")
    };
    let mut output = format!("{open}\n");
    for item in items {
        output.push_str(&indent);
        output.push_str(&format_ordinary_value(item, color, depth + 1));
        output.push_str(",\n");
    }
    output.push_str(&close);
    output.push_str(close_bracket);
    output
}

fn format_ordinary_map(map: &HashMap<String, OValue>, color: bool, depth: usize) -> String {
    if map.is_empty() {
        return if color {
            "\x1b[90m{}\x1b[0m".to_string()
        } else {
            "{}".to_string()
        };
    }
    let indent = "  ".repeat(depth + 1);
    let close = "  ".repeat(depth);
    let mut pairs = map.iter().collect::<Vec<_>>();
    pairs.sort_by_key(|(key, _)| key.as_str());
    let (open, close_brace) = if color {
        ("\x1b[90m{\x1b[0m", "\x1b[90m}\x1b[0m")
    } else {
        ("{", "}")
    };
    let mut output = format!("{open}\n");
    for (key, value) in pairs {
        output.push_str(&indent);
        if color {
            output.push_str(&format!("\x1b[35m\"{key}\"\x1b[0m: "));
        } else {
            output.push_str(&format!("{key:?}: "));
        }
        output.push_str(&format_ordinary_value(value, color, depth + 1));
        output.push_str(",\n");
    }
    output.push_str(&close);
    output.push_str(close_brace);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn options(shim_dir: &Path) -> PrepareExecutionOptionsV1 {
        PrepareExecutionOptionsV1 {
            shim_dir: shim_dir.to_path_buf(),
            ..PrepareExecutionOptionsV1::default()
        }
    }

    #[test]
    fn foreign_file_preflight_has_project_guidance() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("main.py");
        fs::write(&input, b"print(2)\n").unwrap();
        let error = prepare_execution_intent(&input, options(temp.path()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("o-link --project"), "{error}");
    }

    #[test]
    fn ordinary_parallel_auto_forces_graph_and_rejects_mesh() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("program.O");
        fs::write(&input, b"text^(ok)_text\n").unwrap();
        let mut graph_options = options(temp.path());
        graph_options.parallel_auto = true;
        let prepared = prepare_execution_intent(&input, graph_options).unwrap();
        let PreparedExecutionIntentV1::OrdinaryO(prepared) = prepared else {
            panic!("ordinary .O classified as a project")
        };
        assert_eq!(prepared.executor, LocalOExecutorV1::ForcedGraph);

        let mut mesh_options = options(temp.path());
        mesh_options.explicit_mesh = true;
        mesh_options.mesh = Some(MeshExecutionConfig::default());
        let error = prepare_execution_intent(&input, mesh_options)
            .unwrap_err()
            .to_string();
        assert!(error.contains("mesh flags are not valid"), "{error}");
    }

    #[test]
    fn invalid_trace_lifecycle_is_rejected() {
        let trace = OExecutionTraceV1 {
            schema: O_EXECUTION_TRACE_SCHEMA_V1.to_string(),
            events: vec![OExecutionTraceEventV1::NodeStarted { plan_node: 0 }],
        };
        assert!(trace.validate().is_err());
    }

    #[test]
    fn live_readiness_rejects_missing_legacy_python_shim() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("python.O");
        let shims = temp.path().join("empty-shims");
        fs::create_dir(&shims).unwrap();
        fs::write(&input, b"python^(\nprint(2)\n)_python\n").unwrap();
        let prepared = prepare_execution_intent(&input, options(&shims)).unwrap();
        let preview = live_placement_preview(&prepared).unwrap();
        assert!(!preview.local.runtime_ready);
        assert!(preview.local.detail.contains("python"));
    }

    #[test]
    fn live_readiness_uses_native_adapter_requirements_not_legacy_shims() {
        let temp = tempdir().unwrap();
        let input = temp.path().join("bash.O");
        let absent_shims = temp.path().join("does-not-exist");
        fs::write(&input, b"bash^(\necho ready\n)_bash\n").unwrap();
        let prepared = prepare_execution_intent(&input, options(&absent_shims)).unwrap();
        let preview = live_placement_preview(&prepared).unwrap();
        assert!(preview.local.runtime_ready, "{}", preview.local.detail);
        assert!(preview.local.detail.contains("bash"));
    }

    #[test]
    fn hgraph_policy_incompatibility_is_rejected_during_preflight() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("olang.project.toml"),
            r#"
[project]
name = "unsupported-hgraph-policy"

[[routes]]
id = "left"
command = ["sh", "-c", "true"]

[[routes]]
id = "right"
command = ["sh", "-c", "true"]

[[route_sets]]
provides = "both"
alternatives = ["left", "right"]
policy = "all"
"#,
        )
        .unwrap();
        let bundle = crate::project::assemble(temp.path(), "policy", &[]).unwrap();
        let project = build_project_hgraph(&bundle, Some("both"), None).unwrap();
        let error =
            validate_project_executor_preflight(&bundle, &project, ProjectExecutorV1::Hgraph)
                .unwrap_err()
                .to_string();
        assert!(error.contains("does not support policy `all`"), "{error}");
    }

    #[test]
    fn fixed_project_output_bound_is_rejected_during_preflight() {
        let temp = tempdir().unwrap();
        let mut manifest = String::from("[project]\nname = \"oversized-selection\"\n");
        let mut alternatives = Vec::new();
        for index in 0..17 {
            let route = format!("route-{index}");
            alternatives.push(format!("\"{route}\""));
            manifest.push_str(&format!(
                "\n[[routes]]\nid = \"{route}\"\ncommand = [\"sh\", \"-c\", \"true\"]\n"
            ));
        }
        manifest.push_str(&format!(
            "\n[[route_sets]]\nprovides = \"many\"\nalternatives = [{}]\npolicy = \"all\"\n",
            alternatives.join(", ")
        ));
        fs::write(temp.path().join("olang.project.toml"), manifest).unwrap();

        let mut options = options(temp.path());
        options.route = Some("many".to_string());
        let error = prepare_execution_intent(temp.path(), options)
            .unwrap_err()
            .to_string();
        assert!(error.contains("could retain"), "{error}");
        assert!(error.contains("configured maximum"), "{error}");
    }
}
