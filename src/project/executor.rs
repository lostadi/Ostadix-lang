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

use anyhow::{anyhow, bail, Context, Result};

use crate::executor::CancellationToken;
use crate::hgraph::{ExecutableOp, NodeId, ReadyOp, ReadySchedule, ValueState};
use crate::ir::PlanNodeId;

use super::materialize::{materialize_isolated, Workspace};
use super::model::{OExecutionResult, ProjectBundle, RoutePolicy, RouteSpec};
use super::plan::{build_project_hgraph, ProjectHGraph, ProjectPlanOperation, RoutePlanFacts};
use super::runtime::{execute_route_in_workspace, is_skipped_result, run_selection, RunOptions};
use super::trace::{ProjectAttemptIdentity, ProjectAttemptTrace, ProjectRouteOutcome};

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
}

impl ProjectExecutionError {
    pub fn message(&self) -> &str {
        &self.message
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

enum OperationResult {
    Success {
        value: ProjectRuntimeValue,
        outcome: Option<ProjectRouteOutcome>,
        workspace: Option<(usize, Workspace)>,
    },
    Failed {
        error: anyhow::Error,
        outcome: Option<ProjectRouteOutcome>,
    },
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
            trace: ProjectAttemptTrace::new(),
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
                OperationResult::Success {
                    value,
                    outcome,
                    workspace,
                } => {
                    self.commit_success(&ready, &identity, value, outcome, workspace)?;
                }
                OperationResult::Failed { error, outcome } => {
                    self.commit_failure(&ready, &identity, &error, outcome)?;
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
                    return OperationResult::failed(anyhow!(
                        "materialize operation p{} has no branch",
                        operation.id.0
                    ));
                };
                if self.workspaces.contains_key(&branch) {
                    return OperationResult::failed(anyhow!(
                        "project branch {branch} already owns a workspace"
                    ));
                }
                match materialize_isolated(self.bundle)
                    .context("failed to materialize project branch in an isolated workspace")
                {
                    Ok(workspace) => OperationResult::Success {
                        value: ProjectRuntimeValue::Workspace { branch },
                        outcome: None,
                        workspace: Some((branch, workspace)),
                    },
                    Err(error) => OperationResult::failed(error),
                }
            }
            ExecutableOp::BuildRoute { route_id } => {
                match self.prepare_route(operation, route_id) {
                    Ok(prepared) => OperationResult::Success {
                        value: ProjectRuntimeValue::PreparedRoute(prepared),
                        outcome: None,
                        workspace: None,
                    },
                    Err(error) => OperationResult::failed(error),
                }
            }
            ExecutableOp::RunRoute { route_id } => {
                match self.execute_prepared_route(ready, operation, route_id) {
                    Ok(result) => match ProjectRouteOutcome::from_result(&result) {
                        Ok(outcome) if result.succeeded() || is_skipped_result(&result) => {
                            OperationResult::Success {
                                value: ProjectRuntimeValue::RouteResult(result),
                                outcome: Some(outcome),
                                workspace: None,
                            }
                        }
                        Ok(outcome) => OperationResult::Failed {
                            error: anyhow!(
                                "route `{route_id}` failed with exit code {:?}",
                                result.exit_code
                            ),
                            outcome: Some(outcome),
                        },
                        Err(error) => OperationResult::failed(error.into()),
                    },
                    Err(error) => OperationResult::failed(error),
                }
            }
            ExecutableOp::SelectRoute { .. } => match self.select_sole_result(operation) {
                Ok(result) => OperationResult::Success {
                    value: ProjectRuntimeValue::SelectedResult(result),
                    outcome: None,
                    workspace: None,
                },
                Err(error) => OperationResult::failed(error),
            },
            ExecutableOp::CompareRouteResults => OperationResult::failed(anyhow!(
                "CompareRouteResults is unsupported by the single-branch project HGraph executor"
            )),
            other => OperationResult::failed(anyhow!(
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
        let materialize = operation.dependencies.first().copied().with_context(|| {
            format!("build route `{route_id}` has no MaterializeProject dependency")
        })?;
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
        let build = operation
            .dependencies
            .first()
            .copied()
            .with_context(|| format!("run route `{route_id}` has no BuildRoute dependency"))?;
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
        match self.value_for_operation(operation.dependencies[0])? {
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

    fn commit_success(
        &mut self,
        ready: &ReadyOp,
        identity: &ProjectAttemptIdentity,
        value: ProjectRuntimeValue,
        outcome: Option<ProjectRouteOutcome>,
        workspace: Option<(usize, Workspace)>,
    ) -> Result<()> {
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
        if let Some((branch, _)) = &workspace {
            if self.workspaces.contains_key(branch) {
                bail!("project branch {branch} attempted to publish two workspaces");
            }
        }

        // Local linearization point: once trace validation succeeds, the
        // remaining map/set updates are infallible and form one coordinator
        // transition. This does not make command-side external effects exact-once.
        self.trace.record_finished(identity, outcome)?;
        self.values.insert(ready.value_output, value);
        if let Some((branch, workspace)) = workspace {
            self.workspaces.insert(branch, workspace);
        }
        self.materialized.extend(ready.outputs.iter().copied());
        Ok(())
    }

    fn commit_failure(
        &mut self,
        ready: &ReadyOp,
        identity: &ProjectAttemptIdentity,
        error: &anyhow::Error,
        outcome: Option<ProjectRouteOutcome>,
    ) -> Result<()> {
        let description = error.to_string();
        self.trace
            .record_failed(identity, outcome, description.as_bytes())?;
        for output in &ready.outputs {
            self.failed_outputs.insert(*output, ready.plan_node);
        }
        self.failures.insert(ready.plan_node, description);
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
        anyhow::Error::new(ProjectExecutionError {
            message,
            trace: self.trace.clone(),
        })
    }
}

impl OperationResult {
    fn failed(error: anyhow::Error) -> Self {
        Self::Failed {
            error,
            outcome: None,
        }
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

/// Dispatch project selection through the explicitly configured runtime.
///
/// With `O_PROJECT_EXECUTOR=hgraph`, planning or execution errors are returned
/// directly and never fall back to `run_selection`. With the variable unset,
/// the existing project runtime remains the compatibility default.
pub fn run_selection_with_configured_executor(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
) -> Result<Vec<OExecutionResult>> {
    match std::env::var_os(PROJECT_EXECUTOR_ENV) {
        None => run_selection(bundle, target, policy_override, opts),
        Some(value) if value == "hgraph" => {
            let project = build_project_hgraph(bundle, target, policy_override)
                .map_err(anyhow::Error::msg)
                .context("failed to build project HGraph for execution")?;
            let outcome = execute_project_hgraph(bundle, &project, opts)?;
            Ok(vec![outcome.result])
        }
        Some(value) => bail!(
            "unsupported {PROJECT_EXECUTOR_ENV} value `{}`; expected `hgraph` or an unset variable",
            value.to_string_lossy()
        ),
    }
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
