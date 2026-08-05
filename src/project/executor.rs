//! Hosted, single-branch execution of a validated project HGraph.
//!
//! This executor is intentionally narrower than the legacy route-policy
//! runtime. It accepts one already-resolved `Explicit` or `Default`
//! alternative and lets the graph govern workspace materialization, route
//! preparation, prerequisite readiness, route execution, and final selection.
//! Multipath policy, retries, placement, receipts, and native/O-core dispatch
//! are rejected rather than delegated to the legacy selection path.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::executor::CancellationToken;
use crate::hgraph::{ExecutableOp, HNodeKind, NodeId, ReadyOp, ReadySchedule, ValueState};
use crate::ir::PlanNodeId;

use super::materialize::{materialize_isolated, Workspace};
use super::model::{OExecutionResult, ProjectBundle, RoutePolicy, RouteSpec};
use super::plan::{
    build_project_hgraph, ProjectDependency, ProjectHGraph, ProjectPlanOperation, RoutePlanFacts,
};
use super::runtime::{execute_route_in_workspace, is_skipped_result, run_selection, RunOptions};
use super::trace::{
    ProjectAttemptIdentity, ProjectAttemptTrace, ProjectAttemptTraceHeader, ProjectRouteOutcome,
};

/// Opt-in selector for the hosted project HGraph executor.
pub const PROJECT_EXECUTOR_ENV: &str = "O_PROJECT_EXECUTOR";

/// The selected route result together with its deterministic coordinator trace.
#[derive(Debug)]
pub struct ProjectExecutionOutcome {
    pub result: OExecutionResult,
    pub trace: ProjectAttemptTrace,
}

/// A coordinator failure with the lifecycle trace retained for diagnosis.
///
/// [`execute_project_hgraph`] keeps the requested `anyhow::Result` API;
/// callers that need failed-attempt events can downcast its error to this type.
#[derive(Debug)]
pub struct ProjectExecutionError {
    message: String,
    pub trace: ProjectAttemptTrace,
    settled_results: BTreeMap<NodeId, OExecutionResult>,
    materialized_outputs: BTreeSet<NodeId>,
    failed_outputs: BTreeSet<NodeId>,
}

impl ProjectExecutionError {
    pub fn message(&self) -> &str {
        &self.message
    }

    /// A valid route result published before the graph stalled, indexed by
    /// the producing operation's ordinary value-output node.
    pub fn settled_result(&self, output: NodeId) -> Option<&OExecutionResult> {
        self.settled_results.get(&output)
    }

    /// Whether the coordinator materialized this graph output before stalling.
    pub fn is_materialized(&self, output: NodeId) -> bool {
        self.materialized_outputs.contains(&output)
    }

    /// Whether this graph output was deliberately withheld after an
    /// unsuccessful settlement or infrastructure abort.
    pub fn is_failed(&self, output: NodeId) -> bool {
        self.failed_outputs.contains(&output)
    }
}

impl fmt::Display for ProjectExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ProjectExecutionError {}

#[derive(Debug)]
struct PreparedRoute {
    branch: usize,
    route: RouteSpec,
}

#[derive(Debug)]
enum ProjectRuntimeValue {
    Workspace { branch: usize },
    PreparedRoute(PreparedRoute),
    RouteResult(OExecutionResult),
    SelectedResult(OExecutionResult),
}

/// A `RunRoute` operation that either produced a semantic result or aborted
/// before one could be committed.
enum RouteSettlement {
    Succeeded {
        result: OExecutionResult,
        outcome: ProjectRouteOutcome,
    },
    NonZero {
        result: OExecutionResult,
        outcome: ProjectRouteOutcome,
    },
    Skipped {
        result: OExecutionResult,
        outcome: ProjectRouteOutcome,
    },
    Aborted(anyhow::Error),
}

#[derive(Clone, Copy)]
enum SettledRouteStatus {
    Succeeded,
    NonZero,
    Skipped,
}

enum OperationResult {
    Finished {
        value: ProjectRuntimeValue,
        workspace: Option<(usize, Workspace)>,
    },
    Route(RouteSettlement),
    Aborted(anyhow::Error),
}

/// Result plus optional trace returned by the compatibility/HGraph dispatcher.
/// Legacy execution has no project-coordinator trace; HGraph execution does.
#[derive(Debug)]
pub struct ConfiguredProjectExecution {
    pub results: Vec<OExecutionResult>,
    pub trace: Option<ProjectAttemptTrace>,
}

/// Deterministic coordinator for one validated project branch.
///
/// Runtime values are indexed by the graph operation's ordinary value-output
/// `NodeId`; readiness and publication are tracked for every graph output,
/// including completion and resource-state nodes.
pub struct ProjectCoordinator<'a> {
    bundle: &'a ProjectBundle,
    project: &'a ProjectHGraph,
    opts: &'a RunOptions,
    schedule: ReadySchedule,
    launch_rank: BTreeMap<PlanNodeId, usize>,
    materialized: BTreeSet<NodeId>,
    failed_outputs: BTreeMap<NodeId, PlanNodeId>,
    values: BTreeMap<NodeId, ProjectRuntimeValue>,
    workspaces: BTreeMap<usize, Workspace>,
    failures: BTreeMap<PlanNodeId, String>,
    trace: ProjectAttemptTrace,
    cancel: CancellationToken,
}

impl<'a> ProjectCoordinator<'a> {
    /// Validate the exact bundle/plan/graph source and initialize coordinator
    /// state from every node already materialized in the supplied HGraph.
    pub fn new(
        bundle: &'a ProjectBundle,
        project: &'a ProjectHGraph,
        opts: &'a RunOptions,
    ) -> Result<Self> {
        // Reconstructing with the plan's fully resolved selection retains the
        // exact target, alternative order, policy, bundle digest, and graph
        // projection. This happens before any workspace or command is created.
        project
            .validate_source(
                bundle,
                Some(&project.plan.target),
                Some(project.plan.policy.clone()),
            )
            .map_err(anyhow::Error::msg)
            .context("project HGraph source/projection validation failed")?;

        match &project.plan.policy {
            RoutePolicy::Explicit(_) | RoutePolicy::Default => {}
            policy => bail!(
                "project HGraph executor does not support policy `{}`; only explicit/default single-branch execution is supported",
                policy.token()
            ),
        }
        if project.plan.alternatives.len() != 1 {
            bail!(
                "project HGraph executor requires exactly one resolved alternative, found {}",
                project.plan.alternatives.len()
            );
        }

        let schedule = ReadySchedule::derive(&project.graph)
            .map_err(anyhow::Error::msg)
            .context("failed to derive project ReadySchedule")?;
        let launch_rank = schedule
            .launch_order()
            .map_err(anyhow::Error::msg)
            .context("failed to derive stable project launch order")?
            .into_iter()
            .enumerate()
            .map(|(rank, plan_node)| (plan_node, rank))
            .collect();
        let materialized = project
            .graph
            .nodes
            .iter()
            .filter_map(|(id, node)| (node.state == ValueState::Materialized).then_some(*id))
            .collect();
        let trace = ProjectAttemptTrace::new(project_trace_header(project)?)
            .context("failed to initialize project attempt trace")?;

        Ok(Self {
            bundle,
            project,
            opts,
            schedule,
            launch_rank,
            materialized,
            failed_outputs: BTreeMap::new(),
            values: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            failures: BTreeMap::new(),
            trace,
            cancel: CancellationToken::new(),
        })
    }

    /// Run ready operations serially in stable `(ordinal, PlanNodeId)` order.
    pub fn execute(mut self) -> Result<ProjectExecutionOutcome> {
        let mut pending = (0..self.schedule.ops.len()).collect::<BTreeSet<_>>();

        while !pending.is_empty() {
            let next = pending
                .iter()
                .copied()
                .filter(|index| {
                    self.schedule.ops[*index]
                        .inputs
                        .iter()
                        .all(|input| self.materialized.contains(input))
                })
                .min_by_key(|index| {
                    let ready = &self.schedule.ops[*index];
                    (
                        self.launch_rank
                            .get(&ready.plan_node)
                            .copied()
                            .unwrap_or(usize::MAX),
                        ready.ordinal,
                        ready.plan_node.0,
                        *index,
                    )
                });

            let Some(index) = next else {
                return Err(self.stall_error(&pending));
            };
            pending.remove(&index);
            let ready = self.schedule.ops[index].clone();
            let operation = self.operation(ready.plan_node)?.clone();
            let identity = ProjectAttemptIdentity::from_operation(&operation)?;
            self.trace.record_ready(&identity)?;
            self.trace.record_started(&identity)?;

            match self.execute_operation(&ready, &operation) {
                OperationResult::Finished { value, workspace } => {
                    self.commit_finished(&ready, &identity, value, workspace)?;
                }
                OperationResult::Route(settlement) => {
                    self.commit_route_settlement(&ready, &identity, settlement)?;
                }
                OperationResult::Aborted(error) => {
                    self.commit_abort(&ready, &identity, &error, None)?;
                }
            }
        }

        let root = self
            .project
            .graph
            .root_nodes
            .first()
            .copied()
            .context("project HGraph has no result root")?;
        let result = match self.values.remove(&root) {
            Some(ProjectRuntimeValue::SelectedResult(result)) => result,
            Some(_) => bail!("project HGraph root does not contain a selected route result"),
            None => return Err(self.stall_error(&BTreeSet::new())),
        };
        Ok(ProjectExecutionOutcome {
            result,
            trace: self.trace,
        })
    }

    fn execute_operation(
        &mut self,
        ready: &ReadyOp,
        operation: &ProjectPlanOperation,
    ) -> OperationResult {
        match &operation.op {
            ExecutableOp::MaterializeProject => {
                let Some(branch) = operation.branch else {
                    return OperationResult::Aborted(anyhow!(
                        "materialize operation p{} has no branch",
                        operation.id.0
                    ));
                };
                if self.workspaces.contains_key(&branch) {
                    return OperationResult::Aborted(anyhow!(
                        "project branch {branch} already owns a workspace"
                    ));
                }
                match materialize_isolated(self.bundle)
                    .context("failed to materialize project branch in an isolated workspace")
                {
                    Ok(workspace) => OperationResult::Finished {
                        value: ProjectRuntimeValue::Workspace { branch },
                        workspace: Some((branch, workspace)),
                    },
                    Err(error) => OperationResult::Aborted(error),
                }
            }
            ExecutableOp::BuildRoute { route_id } => {
                match self.prepare_route(operation, route_id) {
                    Ok(prepared) => OperationResult::Finished {
                        value: ProjectRuntimeValue::PreparedRoute(prepared),
                        workspace: None,
                    },
                    Err(error) => OperationResult::Aborted(error),
                }
            }
            ExecutableOp::RunRoute { route_id } => {
                match self.execute_prepared_route(ready, operation, route_id) {
                    Ok(result) => match ProjectRouteOutcome::from_result(&result) {
                        Ok(outcome) if is_skipped_result(&result) => {
                            OperationResult::Route(RouteSettlement::Skipped { result, outcome })
                        }
                        Ok(outcome) if result.succeeded() => {
                            OperationResult::Route(RouteSettlement::Succeeded { result, outcome })
                        }
                        Ok(outcome) => {
                            OperationResult::Route(RouteSettlement::NonZero { result, outcome })
                        }
                        Err(error) => {
                            OperationResult::Route(RouteSettlement::Aborted(error.into()))
                        }
                    },
                    Err(error) => OperationResult::Route(RouteSettlement::Aborted(error)),
                }
            }
            ExecutableOp::SelectRoute { .. } => match self.select_sole_result(operation) {
                Ok(result) => OperationResult::Finished {
                    value: ProjectRuntimeValue::SelectedResult(result),
                    workspace: None,
                },
                Err(error) => OperationResult::Aborted(error),
            },
            ExecutableOp::CompareRouteResults => OperationResult::Aborted(anyhow!(
                "CompareRouteResults is unsupported by the single-branch project HGraph executor"
            )),
            other => OperationResult::Aborted(anyhow!(
                "non-project operation {other:?} reached the project HGraph executor"
            )),
        }
    }

    fn prepare_route(
        &self,
        operation: &ProjectPlanOperation,
        route_id: &str,
    ) -> Result<PreparedRoute> {
        let branch = operation
            .branch
            .with_context(|| format!("build route `{route_id}` has no branch"))?;
        let dependency = operation.dependencies.first().copied().with_context(|| {
            format!("build route `{route_id}` has no MaterializeProject dependency")
        })?;
        let ProjectDependency::Value(materialize) = dependency else {
            bail!("build route `{route_id}` does not consume a workspace value");
        };
        match self.value_for_operation(materialize)? {
            ProjectRuntimeValue::Workspace {
                branch: workspace_branch,
            } if *workspace_branch == branch => {}
            ProjectRuntimeValue::Workspace {
                branch: workspace_branch,
            } => bail!(
                "build route `{route_id}` branch {branch} received workspace branch {workspace_branch}"
            ),
            _ => bail!("build route `{route_id}` did not receive a workspace value"),
        }
        if !self.workspaces.contains_key(&branch) {
            bail!("build route `{route_id}` has no materialized branch workspace");
        }
        let route = self
            .bundle
            .route(route_id)
            .cloned()
            .with_context(|| format!("build route `{route_id}` is absent from the bundle"))?;
        if route.id != route_id {
            bail!(
                "build route id `{route_id}` disagrees with route `{}`",
                route.id
            );
        }
        if route.command.is_empty() {
            bail!("route `{route_id}` has an empty command");
        }
        let expected = operation
            .route_facts
            .as_ref()
            .with_context(|| format!("build route `{route_id}` lacks RoutePlanFacts"))?;
        let actual = route_plan_facts(&route);
        if expected != &actual {
            bail!("build route `{route_id}` RoutePlanFacts do not match the bundle route");
        }
        Ok(PreparedRoute { branch, route })
    }

    fn execute_prepared_route(
        &self,
        _ready: &ReadyOp,
        operation: &ProjectPlanOperation,
        route_id: &str,
    ) -> Result<OExecutionResult> {
        let dependency = operation
            .dependencies
            .first()
            .copied()
            .with_context(|| format!("run route `{route_id}` has no BuildRoute dependency"))?;
        let ProjectDependency::Value(build) = dependency else {
            bail!("run route `{route_id}` does not consume a PreparedRoute value");
        };
        let prepared = match self.value_for_operation(build)? {
            ProjectRuntimeValue::PreparedRoute(prepared) => prepared,
            _ => bail!("run route `{route_id}` did not receive a PreparedRoute value"),
        };
        let branch = operation
            .branch
            .with_context(|| format!("run route `{route_id}` has no branch"))?;
        if prepared.branch != branch || prepared.route.id != route_id {
            bail!("run route `{route_id}` received a mismatched PreparedRoute");
        }
        let workspace = self
            .workspaces
            .get(&branch)
            .with_context(|| format!("run route `{route_id}` has no branch workspace"))?;

        // This is deliberately the single-route primitive. Prerequisite order
        // is already represented by this RunRoute operation's graph inputs.
        execute_route_in_workspace(&prepared.route, workspace, self.opts, &self.cancel)
    }

    fn select_sole_result(&self, operation: &ProjectPlanOperation) -> Result<OExecutionResult> {
        if !matches!(
            self.project.plan.policy,
            RoutePolicy::Explicit(_) | RoutePolicy::Default
        ) {
            bail!(
                "SelectRoute policy `{}` is unsupported by the single-branch executor",
                self.project.plan.policy.token()
            );
        }
        if operation.dependencies.len() != 1 {
            bail!(
                "SelectRoute requires exactly one materialized route result, found {}",
                operation.dependencies.len()
            );
        }
        let ProjectDependency::Value(result_operation) = operation.dependencies[0] else {
            bail!("SelectRoute dependency is not an ordinary route-result value");
        };
        match self.value_for_operation(result_operation)? {
            ProjectRuntimeValue::RouteResult(result) => Ok(result.clone()),
            _ => bail!("SelectRoute dependency is not a route result"),
        }
    }

    fn operation(&self, plan_node: PlanNodeId) -> Result<&ProjectPlanOperation> {
        self.project
            .plan
            .operations
            .get(plan_node.0)
            .filter(|operation| operation.id == plan_node)
            .with_context(|| {
                format!(
                    "ReadySchedule references missing plan node p{}",
                    plan_node.0
                )
            })
    }

    fn value_for_operation(&self, plan_node: PlanNodeId) -> Result<&ProjectRuntimeValue> {
        let output = self
            .project
            .graph
            .op_for(plan_node)
            .with_context(|| format!("plan node p{} has no HGraph operation", plan_node.0))?
            .value_output;
        self.values
            .get(&output)
            .with_context(|| format!("plan node p{} has no stored runtime value", plan_node.0))
    }

    fn commit_finished(
        &mut self,
        ready: &ReadyOp,
        identity: &ProjectAttemptIdentity,
        value: ProjectRuntimeValue,
        workspace: Option<(usize, Workspace)>,
    ) -> Result<()> {
        self.ensure_outputs_unpublished(ready)?;
        if let Some((branch, _)) = &workspace {
            if self.workspaces.contains_key(branch) {
                bail!("project branch {branch} attempted to publish two workspaces");
            }
        }

        // Local linearization point: once trace validation succeeds, the
        // remaining map/set updates are infallible and form one coordinator
        // transition. This does not make command-side external effects exact-once.
        self.trace.record_finished(identity)?;
        self.values.insert(ready.value_output, value);
        if let Some((branch, workspace)) = workspace {
            self.workspaces.insert(branch, workspace);
        }
        self.materialized.extend(ready.outputs.iter().copied());
        Ok(())
    }

    fn commit_route_settlement(
        &mut self,
        ready: &ReadyOp,
        identity: &ProjectAttemptIdentity,
        settlement: RouteSettlement,
    ) -> Result<()> {
        let (status, result, outcome) = match settlement {
            RouteSettlement::Succeeded { result, outcome } => {
                (SettledRouteStatus::Succeeded, result, outcome)
            }
            RouteSettlement::NonZero { result, outcome } => {
                (SettledRouteStatus::NonZero, result, outcome)
            }
            RouteSettlement::Skipped { result, outcome } => {
                (SettledRouteStatus::Skipped, result, outcome)
            }
            RouteSettlement::Aborted(error) => {
                return self.commit_abort(ready, identity, &error, None)
            }
        };

        self.ensure_outputs_unpublished(ready)?;
        let mut publish = Vec::new();
        let mut withhold = Vec::new();
        for output in &ready.outputs {
            let node = self.project.graph.node(*output).with_context(|| {
                format!(
                    "project operation p{} references missing output N{}",
                    ready.plan_node.0, output.0
                )
            })?;
            let should_publish = match &node.kind {
                HNodeKind::Value => true,
                HNodeKind::Completion { .. } => {
                    matches!(
                        status,
                        SettledRouteStatus::Succeeded | SettledRouteStatus::Skipped
                    )
                }
                // A started command may have changed host state before
                // reporting nonzero. Conservatively advance every declared
                // resource successor for all valid route settlements.
                HNodeKind::ResourceState { .. } => true,
                HNodeKind::BranchControl { .. } => {
                    matches!(
                        status,
                        SettledRouteStatus::Succeeded | SettledRouteStatus::Skipped
                    )
                }
            };
            if should_publish {
                publish.push(*output);
            } else {
                withhold.push(*output);
            }
        }

        // Route linearization point: the terminal event, ordinary result, and
        // settlement-specific output publication are one infallible local
        // transition after trace validation. This says nothing about
        // exactly-once command-side effects.
        match status {
            SettledRouteStatus::Succeeded => {
                self.trace.record_settled_success(identity, outcome)?
            }
            SettledRouteStatus::NonZero => self.trace.record_settled_failure(identity, outcome)?,
            SettledRouteStatus::Skipped => self.trace.record_skipped(identity, outcome)?,
        }
        let exit_code = result.exit_code;
        self.values
            .insert(ready.value_output, ProjectRuntimeValue::RouteResult(result));
        self.materialized.extend(publish);
        for output in withhold {
            self.failed_outputs.insert(output, ready.plan_node);
        }
        if matches!(status, SettledRouteStatus::NonZero) {
            self.failures.insert(
                ready.plan_node,
                format!(
                    "route `{}` settled unsuccessfully with exit code {exit_code:?}",
                    identity.route_id.as_deref().unwrap_or("<unknown>")
                ),
            );
        }
        Ok(())
    }

    fn commit_abort(
        &mut self,
        ready: &ReadyOp,
        identity: &ProjectAttemptIdentity,
        error: &anyhow::Error,
        outcome: Option<ProjectRouteOutcome>,
    ) -> Result<()> {
        self.ensure_outputs_unpublished(ready)?;
        let description = error.to_string();

        // An abort has no operation value. Its terminal trace event and the
        // withholding of every graph output form the local commit.
        self.trace
            .record_aborted(identity, outcome, description.as_bytes())?;
        for output in &ready.outputs {
            self.failed_outputs.insert(*output, ready.plan_node);
        }
        self.failures.insert(ready.plan_node, description);
        Ok(())
    }

    fn ensure_outputs_unpublished(&self, ready: &ReadyOp) -> Result<()> {
        if self.values.contains_key(&ready.value_output) {
            bail!(
                "project operation p{} attempted to publish its value twice",
                ready.plan_node.0
            );
        }
        if ready.outputs.iter().any(|output| {
            self.materialized.contains(output) || self.failed_outputs.contains_key(output)
        }) {
            bail!(
                "project operation p{} attempted to publish an already-terminal output",
                ready.plan_node.0
            );
        }
        Ok(())
    }

    fn stall_error(&self, pending: &BTreeSet<usize>) -> anyhow::Error {
        let mut details = Vec::new();
        for index in pending {
            let ready = &self.schedule.ops[*index];
            let label = self
                .operation(ready.plan_node)
                .ok()
                .and_then(|operation| ProjectAttemptIdentity::from_operation(operation).ok())
                .map(|identity| identity.operation_label)
                .unwrap_or_else(|| format!("p{}", ready.plan_node.0));
            let unresolved = ready
                .inputs
                .iter()
                .filter(|input| !self.materialized.contains(input))
                .map(|input| match self.failed_outputs.get(input) {
                    Some(producer) => format!("N{} failed at p{}", input.0, producer.0),
                    None => format!("N{} unresolved", input.0),
                })
                .collect::<Vec<_>>();
            details.push(format!(
                "p{} {label} waits on [{}]",
                ready.plan_node.0,
                unresolved.join(", ")
            ));
        }
        for (plan_node, failure) in &self.failures {
            details.push(format!("p{} failed: {failure}", plan_node.0));
        }
        let message = if details.is_empty() {
            "project HGraph stalled without a materialized selected-result root".to_string()
        } else {
            format!("project HGraph stalled: {}", details.join("; "))
        };
        let settled_results = self
            .values
            .iter()
            .filter_map(|(output, value)| match value {
                ProjectRuntimeValue::RouteResult(result) => Some((*output, result.clone())),
                _ => None,
            })
            .collect();
        anyhow::Error::new(ProjectExecutionError {
            message,
            trace: self.trace.clone(),
            settled_results,
            materialized_outputs: self.materialized.clone(),
            failed_outputs: self.failed_outputs.keys().copied().collect(),
        })
    }
}

/// Execute one validated, single-branch hosted project HGraph.
pub fn execute_project_hgraph(
    bundle: &ProjectBundle,
    project: &ProjectHGraph,
    opts: &RunOptions,
) -> Result<ProjectExecutionOutcome> {
    ProjectCoordinator::new(bundle, project, opts)?.execute()
}

fn project_trace_header(project: &ProjectHGraph) -> Result<ProjectAttemptTraceHeader> {
    const GRAPH_DIGEST_DOMAIN: &[u8] = b"ostadix.project-hgraph.logical/v1\0";

    let logical = project.to_text();
    let mut hasher = Sha256::new();
    hasher.update(GRAPH_DIGEST_DOMAIN);
    hasher.update(logical.as_bytes());
    let logical_graph_digest = hex::encode(hasher.finalize());

    let mut attempt = [0_u8; 32];
    getrandom::fill(&mut attempt)
        .context("failed to obtain entropy for project execution attempt identity")?;

    Ok(ProjectAttemptTraceHeader::new(
        project.plan.project_name.clone(),
        project.plan.bundle_digest.clone(),
        project.plan.target.clone(),
        project.plan.policy.token(),
        logical_graph_digest,
        hex::encode(attempt),
    ))
}

/// Dispatch project selection through the explicitly configured runtime.
///
/// With `O_PROJECT_EXECUTOR=hgraph`, planning or execution errors are returned
/// directly and never fall back to `run_selection`. With the variable unset,
/// the existing project runtime remains the compatibility default.
pub fn execute_selection_with_configured_executor(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
) -> Result<ConfiguredProjectExecution> {
    match std::env::var_os(PROJECT_EXECUTOR_ENV) {
        None => Ok(ConfiguredProjectExecution {
            results: run_selection(bundle, target, policy_override, opts)?,
            trace: None,
        }),
        Some(value) if value == "hgraph" => {
            let project = build_project_hgraph(bundle, target, policy_override)
                .map_err(anyhow::Error::msg)
                .context("failed to build project HGraph for execution")?;
            let outcome = execute_project_hgraph(bundle, &project, opts)?;
            Ok(ConfiguredProjectExecution {
                results: vec![outcome.result],
                trace: Some(outcome.trace),
            })
        }
        Some(value) => bail!(
            "unsupported {PROJECT_EXECUTOR_ENV} value `{}`; expected `hgraph` or an unset variable",
            value.to_string_lossy()
        ),
    }
}

/// Compatibility wrapper retaining the pre-PR8A result-only API.
pub fn run_selection_with_configured_executor(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
) -> Result<Vec<OExecutionResult>> {
    Ok(execute_selection_with_configured_executor(bundle, target, policy_override, opts)?.results)
}

/// Store one unsigned diagnostic project-attempt trace as formatted JSON.
///
/// This is an observability surface, not an OWRECEIPT or attestation.
pub fn write_project_attempt_trace(path: &Path, trace: &ProjectAttemptTrace) -> Result<()> {
    let mut encoded =
        serde_json::to_vec_pretty(trace).context("failed to serialize project attempt trace")?;
    encoded.push(b'\n');
    std::fs::write(path, encoded).with_context(|| {
        format!(
            "failed to write project attempt trace to {}",
            path.display()
        )
    })
}

fn route_plan_facts(route: &RouteSpec) -> RoutePlanFacts {
    RoutePlanFacts {
        kind: route.kind,
        prerequisites: route.prerequisites.clone(),
        guards: route.guards.clone(),
        environment_keys: route.environment.keys().cloned().collect(),
        inputs: route.inputs.clone(),
        outputs: route.outputs.clone(),
        declared_reads: route.effects.reads.clone(),
        declared_writes: route.effects.writes.clone(),
        declared_pure: route.effects.pure,
    }
}
