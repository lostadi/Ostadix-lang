//! Hosted execution of a validated project HGraph.
//!
//! The graph governs workspace materialization, prerequisite/resource readiness,
//! concurrent route execution, cancellation, comparison, and final selection.
//! Explicit parallel policies retain unknown host effects and authorize ambient
//! overlap; they do not claim sandbox isolation. One coordinator owns all
//! lifecycle publication, and completed outcomes must pass semantic replay.
//! The default preserves legacy continuation through a bound compatibility
//! contract; explicitly selecting `hgraph` retains strict continuation.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::executor::CancellationToken;
use crate::hgraph::{
    ExecutableOp, HNodeKind, NodeId, ReadyInputPolicy, ReadyOp, ReadySchedule, ValueState,
};
use crate::ir::PlanNodeId;
use anyhow::{anyhow, bail, Context, Result};

use super::deployment::DeploymentPlanV1;
use super::launch::{HostedWorldCurrentV1, HostedWorldLaunchV1};
use super::materialize::{materialize_isolated, Workspace};
use super::model::{
    OExecutionResult, ProjectBundle, RouteFailureContinuation, RoutePolicy, RouteSpec,
    ValidatedSelectionReceiptV1,
};
use super::plan::{
    build_project_hgraph_with_contract, policy_runs_parallel, ProjectDependency,
    ProjectExecutionContract, ProjectHGraph, ProjectPlanOperation, RoutePlanFacts,
};
use super::runtime::{
    benchmark_validate_and_select, execute_route_in_workspace, is_cancellation_error,
    is_skipped_result, public_route_execution_diagnostic, verify_results_equivalent,
    MeasuredRouteExecution, RouteSelectionExecution, RunOptions,
    ValidatedSelectionCandidateProgressV1, ValidatedSelectionMeasurement,
    ValidatedSelectionProgressEventV1, ValidatedSelectionProgressObserverV1,
};
use super::trace::{
    project_deployment_digest, project_hosted_deployment_digest, project_logical_graph_digest,
    race_selected_settlement, race_selection_ready, race_trigger, ProjectAttemptIdentity,
    ProjectAttemptTrace, ProjectAttemptTraceHeader, ProjectContinuationDecision,
    ProjectContinuationEvidence, ProjectPolicyCandidate, ProjectPolicySelection,
    ProjectRouteOutcome,
};

/// Hosted project executor selector: unset is compatibility HGraph, `hgraph`
/// is strict HGraph, and `legacy` explicitly selects the previous runtime.
pub const PROJECT_EXECUTOR_ENV: &str = "O_PROJECT_EXECUTOR";

/// The selected route result together with its deterministic coordinator trace.
#[derive(Debug)]
pub struct ProjectExecutionOutcome {
    /// Result selected by the route policy.
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
    public_message: String,
    class: ProjectExecutionFailureClass,
    pub trace: ProjectAttemptTrace,
    settled_results: BTreeMap<NodeId, OExecutionResult>,
    materialized_outputs: BTreeSet<NodeId>,
    failed_outputs: BTreeSet<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectExecutionFailureClass {
    Semantic,
    Infrastructure,
}

impl ProjectExecutionError {
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Credential-safe diagnostic suitable for a durable observation.
    /// Direct executor Display remains source-compatible and may include the
    /// exact route argv; callers that persist errors must use this projection.
    pub fn public_message(&self) -> &str {
        &self.public_message
    }

    pub const fn class(&self) -> ProjectExecutionFailureClass {
        self.class
    }

    /// A valid route result published before the graph stalled, indexed by
    /// the producing operation's ordinary value-output node.
    pub fn settled_result(&self, output: NodeId) -> Option<&OExecutionResult> {
        self.settled_results.get(&output)
    }

    /// Iterate every ordinary route result published before the coordinator
    /// stalled, in ascending HGraph `NodeId` order.
    ///
    /// The stable order is part of this observation surface: callers may
    /// serialize a failed attempt without depending on hash-map iteration or
    /// rediscovering output nodes from the trace.
    pub fn settled_results(
        &self,
    ) -> impl DoubleEndedIterator<Item = (NodeId, &OExecutionResult)> + ExactSizeIterator {
        self.settled_results
            .iter()
            .map(|(output, result)| (*output, result))
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
    ComparedResults(RouteSelectionExecution),
    SelectedResult(Box<SelectedProjectResult>),
}

#[derive(Debug)]
struct SelectedProjectResult {
    result: OExecutionResult,
    attempted_results: Vec<OExecutionResult>,
    receipt: Option<ValidatedSelectionReceiptV1>,
    measurements: Option<Vec<ValidatedSelectionMeasurement>>,
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

#[derive(Clone)]
struct BranchRouteAssessment {
    route_id: String,
    executed: bool,
    failure_continuation: RouteFailureContinuation,
}

enum ObserverDelivery {
    Event(ValidatedSelectionProgressEventV1),
    Flush(mpsc::Sender<()>),
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
    pub validated_selection_receipt: Option<ValidatedSelectionReceiptV1>,
    pub validated_selection_measurements: Option<Vec<ValidatedSelectionMeasurement>>,
}

struct ProjectCoordinatorOutcome {
    result: OExecutionResult,
    attempted_results: Vec<OExecutionResult>,
    trace: ProjectAttemptTrace,
    receipt: Option<ValidatedSelectionReceiptV1>,
    measurements: Option<Vec<ValidatedSelectionMeasurement>>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct ProjectPolicyRejection(anyhow::Error);

/// Coordinator for validated project branches and policy selection.
///
/// Runtime values are indexed by the graph operation's ordinary value-output
/// `NodeId`; readiness and publication are tracked for every graph output,
/// including completion and resource-state nodes.
pub struct ProjectCoordinator<'a> {
    bundle: &'a ProjectBundle,
    project: &'a ProjectHGraph,
    opts: &'a RunOptions,
    schedule: ReadySchedule,
    input_policies: BTreeMap<PlanNodeId, ReadyInputPolicy>,
    launch_rank: BTreeMap<PlanNodeId, usize>,
    materialized: BTreeSet<NodeId>,
    failed_outputs: BTreeMap<NodeId, PlanNodeId>,
    values: BTreeMap<NodeId, ProjectRuntimeValue>,
    workspaces: BTreeMap<usize, Arc<Workspace>>,
    failures: BTreeMap<PlanNodeId, String>,
    public_failures: BTreeMap<PlanNodeId, String>,
    branch_assessments: BTreeMap<usize, Vec<BranchRouteAssessment>>,
    continuation_denied: bool,
    infrastructure_failure_observed: bool,
    /// Exact snapshot-derived deployment for the bounded World-hosted path.
    /// `None` retains the compatibility hosted-unbound trace contract.
    deployment: Option<&'a DeploymentPlanV1>,
    trace: ProjectAttemptTrace,
    cancel: CancellationToken,
    branch_tokens: Vec<CancellationToken>,
    branch_started: BTreeMap<usize, Instant>,
    branch_elapsed: BTreeMap<usize, u128>,
    observer: Option<&'a dyn ValidatedSelectionProgressObserverV1>,
    observer_delivery: Option<mpsc::Sender<ObserverDelivery>>,
}

impl<'a> ProjectCoordinator<'a> {
    /// Validate the exact bundle/plan/graph source and initialize coordinator
    /// state from every node already materialized in the supplied HGraph.
    pub fn new(
        bundle: &'a ProjectBundle,
        project: &'a ProjectHGraph,
        opts: &'a RunOptions,
    ) -> Result<Self> {
        let header = project_trace_header(project)?;
        Self::new_with_header(
            bundle,
            project,
            opts,
            None,
            header,
            ProjectExecutionContract::Strict,
        )
    }

    /// Execute under an explicit caller-owned contract, never a trace flag.
    pub fn new_with_contract(
        bundle: &'a ProjectBundle,
        project: &'a ProjectHGraph,
        opts: &'a RunOptions,
        expected_contract: ProjectExecutionContract,
    ) -> Result<Self> {
        let header = project_trace_header(project)?;
        Self::new_with_header(bundle, project, opts, None, header, expected_contract)
    }

    /// Enter the coordinator through one exact, current World-hosted launch.
    ///
    /// Trusted graph/deployment/snapshot equality and every current generation
    /// are fenced here before schedule derivation, workspace materialization,
    /// or child-process launch. The launch profile is descriptive and does not
    /// confer Governor admission, reservation, dispatch, or effect authority.
    pub fn new_world_bound(
        bundle: &'a ProjectBundle,
        project: &'a ProjectHGraph,
        opts: &'a RunOptions,
        deployment: &'a DeploymentPlanV1,
        snapshot: &super::deployment::PlacementSnapshotV1,
        launch: &HostedWorldLaunchV1,
        current: &HostedWorldCurrentV1,
    ) -> Result<Self> {
        // Rebuild the logical contract from the exact coordinator input rather
        // than trusting a separately supplied logical record.
        let logical = project
            .logical_v1()
            .context("failed to derive trusted logical HGraph at World launch")?;
        launch
            .validate_trusted(&logical, deployment, snapshot)
            .context("World-hosted project launch source validation failed")?;
        launch
            .validate_current(current)
            .context("World-hosted project launch freshness fence failed")?;
        let header = project_world_trace_header(project, deployment, launch)?;
        Self::new_with_header(
            bundle,
            project,
            opts,
            Some(deployment),
            header,
            ProjectExecutionContract::Strict,
        )
    }

    fn new_with_header(
        bundle: &'a ProjectBundle,
        project: &'a ProjectHGraph,
        opts: &'a RunOptions,
        deployment: Option<&'a DeploymentPlanV1>,
        header: ProjectAttemptTraceHeader,
        expected_contract: ProjectExecutionContract,
    ) -> Result<Self> {
        if project.plan.execution_contract != expected_contract {
            bail!(
                "project HGraph execution contract differs from the trusted coordinator contract"
            );
        }
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

        let potential_route_executions = project
            .plan
            .operations
            .iter()
            .filter(|operation| matches!(operation.op, ExecutableOp::RunRoute { .. }))
            .count();
        opts.limits
            .validate_route_execution_set(potential_route_executions)?;

        match &project.plan.policy {
            RoutePolicy::Explicit(_) | RoutePolicy::Default
                if project.plan.alternatives.len() != 1 =>
            {
                bail!("project HGraph executor requires exactly one resolved alternative for policy `{}`, found {}", project.plan.policy.token(), project.plan.alternatives.len());
            }
            _ => {}
        }

        let schedule = ReadySchedule::derive(&project.graph)
            .map_err(anyhow::Error::msg)
            .context("failed to derive project ReadySchedule")?;
        let input_policies = schedule
            .ops
            .iter()
            .map(|ready| {
                ready
                    .input_policy(&project.graph)
                    .map(|policy| (ready.plan_node, policy))
            })
            .collect::<std::result::Result<BTreeMap<_, _>, _>>()
            .map_err(anyhow::Error::msg)
            .context("failed to bind project ReadySchedule input policies")?;
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
        let trace = ProjectAttemptTrace::new(header)
            .context("failed to initialize project attempt trace")?;

        Ok(Self {
            bundle,
            project,
            opts,
            schedule,
            input_policies,
            launch_rank,
            materialized,
            failed_outputs: BTreeMap::new(),
            values: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            failures: BTreeMap::new(),
            public_failures: BTreeMap::new(),
            branch_assessments: BTreeMap::new(),
            continuation_denied: false,
            infrastructure_failure_observed: false,
            deployment,
            trace,
            cancel: CancellationToken::new(),
            branch_tokens: project
                .plan
                .alternatives
                .iter()
                .map(|_| CancellationToken::new())
                .collect(),
            branch_started: BTreeMap::new(),
            branch_elapsed: BTreeMap::new(),
            observer: None,
            observer_delivery: None,
        })
    }

    /// Dispatch graph-ready operations using stable launch rank. Parallel
    /// policies run independent routes concurrently; one coordinator records
    /// completion and drains race losers before publishing the selected root.
    pub fn execute(self) -> Result<ProjectExecutionOutcome> {
        let outcome = self.execute_with_attempts()?;
        Ok(ProjectExecutionOutcome {
            result: outcome.result,
            trace: outcome.trace,
        })
    }

    fn execute_with_attempts(mut self) -> Result<ProjectCoordinatorOutcome> {
        let mut pending = (0..self.schedule.ops.len()).collect::<BTreeSet<_>>();

        if self.project.plan.policy == RoutePolicy::BenchmarkValidateAndSelect {
            self.observe(ValidatedSelectionProgressEventV1::SelectionStarted {
                reference_route_id: self.project.plan.alternatives[0].clone(),
                candidate_count: self.project.plan.alternatives.len(),
            })?;
        }
        // Presentation callbacks complete before any candidate window opens.
        // Finish callbacks use a separate delivery worker so they cannot hold
        // up another branch's prerequisite dispatch or completion accounting.
        for (branch, route_id) in self.project.plan.alternatives.iter().enumerate() {
            self.observe(ValidatedSelectionProgressEventV1::CandidateStarted {
                declaration_index: branch,
                route_id: route_id.clone(),
                candidate_count: self.project.plan.alternatives.len(),
            })?;
        }
        let parallel = policy_runs_parallel(&self.project.plan.policy);
        let (sender, receiver) = mpsc::channel();
        std::thread::scope(|scope| -> Result<()> {
            if let Some(observer) = self
                .observer
                .filter(|_| self.project.plan.policy == RoutePolicy::BenchmarkValidateAndSelect)
            {
                let (delivery, events) = mpsc::channel();
                self.observer_delivery = Some(delivery);
                scope.spawn(move || {
                    while let Ok(message) = events.recv() {
                        match message {
                            ObserverDelivery::Event(event) => {
                                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    observer.observe(event)
                                }))
                                .is_err()
                                {
                                    return;
                                }
                            }
                            ObserverDelivery::Flush(acknowledge) => {
                                let _ = acknowledge.send(());
                            }
                        }
                    }
                });
            }
            let execution =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let mut running = BTreeMap::<usize, (ReadyOp, ProjectAttemptIdentity)>::new();
                    loop {
                        let next = pending
                            .iter()
                            .copied()
                            .filter(|index| self.operation_is_ready(&self.schedule.ops[*index]))
                            .min_by_key(|index| {
                                let ready = &self.schedule.ops[*index];
                                (
                                    self.operation_priority(ready),
                                    self.launch_rank
                                        .get(&ready.plan_node)
                                        .copied()
                                        .unwrap_or(usize::MAX),
                                    ready.ordinal,
                                    ready.plan_node.0,
                                    *index,
                                )
                            });
                        if let Some(index) = next {
                            pending.remove(&index);
                            let ready = self.schedule.ops[index].clone();
                            let operation = self.operation(ready.plan_node)?.clone();
                            let identity = ProjectAttemptIdentity::from_operation(&operation)?;
                            self.trace.record_ready(&identity)?;
                            self.trace.record_started(&identity)?;
                            if matches!(operation.op, ExecutableOp::MaterializeProject) {
                                if let Some(branch) = operation.branch {
                                    self.branch_started.insert(branch, Instant::now());
                                }
                            }
                            if parallel && matches!(operation.op, ExecutableOp::RunRoute { .. }) {
                                let (route, workspace, token) =
                                    match self.prepared_route_worker(&operation) {
                                        Ok(worker) => worker,
                                        Err(error) => {
                                            self.commit_abort(&ready, &identity, &error, None)?;
                                            continue;
                                        }
                                    };
                                let sender = sender.clone();
                                let opts = self.opts;
                                running.insert(index, (ready, identity));
                                scope.spawn(move || {
                                    let outcome = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            execute_route_in_workspace(
                                                &route, &workspace, opts, &token,
                                            )
                                        }),
                                    )
                                    .unwrap_or_else(|_| {
                                        Err(anyhow!("project route worker panicked"))
                                    });
                                    let _ = sender.send((index, outcome, Instant::now()));
                                });
                                // Launch every graph-ready alternative before waiting;
                                // prerequisites and explicit shared-resource leases are
                                // still enforced by ordinary graph readiness.
                                continue;
                            }
                            match self.execute_operation(&ready, &operation) {
                                OperationResult::Finished { value, workspace } => {
                                    self.commit_finished(&ready, &identity, value, workspace)?
                                }
                                OperationResult::Route(settlement) => self
                                    .commit_route_settlement(
                                        &ready,
                                        &identity,
                                        settlement,
                                        Instant::now(),
                                    )?,
                                OperationResult::Aborted(error) => {
                                    self.commit_abort(&ready, &identity, &error, None)?
                                }
                            }
                            self.cancel_race_losers();
                            if self.root_is_materialized() {
                                break;
                            }
                            continue;
                        }
                        if !running.is_empty() {
                            let (index, result, completed) = receiver
                                .recv()
                                .context("project route workers disconnected before settlement")?;
                            let (ready, identity) = running
                                .remove(&index)
                                .context("route worker reported an unregistered settlement")?;
                            self.commit_route_settlement(
                                &ready,
                                &identity,
                                route_settlement(result),
                                completed,
                            )?;
                            self.cancel_race_losers();
                            continue;
                        }
                        if self.root_is_materialized() {
                            break;
                        }
                        return Err(self.stall_error(&pending));
                    }
                    Ok(())
                }));
            // Close before scoped-thread join on success, error, or unwind.
            // A sender retained outside this scope would deadlock its observer
            // receiver while scope waited to join that worker.
            self.observer_delivery.take();
            match execution {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        })?;

        let root = self
            .project
            .graph
            .root_nodes
            .first()
            .copied()
            .context("project HGraph has no result root")?;
        let (result, attempted_results, receipt, measurements) = match self.values.remove(&root) {
            Some(ProjectRuntimeValue::SelectedResult(selected)) => {
                let SelectedProjectResult {
                    result,
                    attempted_results,
                    receipt,
                    measurements,
                } = *selected;
                (result, attempted_results, receipt, measurements)
            }
            Some(_) => bail!("project HGraph root does not contain a selected route result"),
            None => return Err(self.stall_error(&BTreeSet::new())),
        };
        let header = self.trace.header().clone();
        let events = self.trace.events().to_vec();
        let trace = match self.deployment {
            Some(deployment) => ProjectAttemptTrace::try_from_project_events_with_deployment(
                self.project,
                deployment,
                header,
                events,
            ),
            None => ProjectAttemptTrace::try_from_project_events(self.project, header, events),
        }
        .context("coordinator-produced project trace failed semantic replay")?;
        Ok(ProjectCoordinatorOutcome {
            result,
            attempted_results,
            trace,
            receipt,
            measurements,
        })
    }

    fn operation_is_ready(&self, ready: &ReadyOp) -> bool {
        let Some(operation) = self
            .project
            .plan
            .operations
            .get(ready.plan_node.0)
            .filter(|operation| operation.id == ready.plan_node)
        else {
            return false;
        };

        if let Some(trigger) = race_trigger(self.project, self.trace.events()) {
            if operation.branch.is_some() && operation.branch != trigger.branch {
                return false;
            }
            if matches!(operation.op, ExecutableOp::SelectRoute { .. }) {
                return race_selection_ready(self.project, self.trace.events());
            }
        }

        let Some(input_policy) = self.input_policies.get(&ready.plan_node).copied() else {
            return false;
        };
        match input_policy {
            ReadyInputPolicy::All => {}
            ReadyInputPolicy::OrderedFirstSuccess => {
                return self.uses_ordered_first_success()
                    && matches!(operation.op, ExecutableOp::SelectRoute { .. })
                    && (self.continuation_denied || self.ordered_selection_is_ready(ready));
            }
        }

        ready
            .inputs
            .iter()
            .all(|input| self.materialized.contains(input))
    }

    fn operation_priority(&self, ready: &ReadyOp) -> u8 {
        let selection_must_settle = self.input_policies.get(&ready.plan_node).copied()
            == Some(ReadyInputPolicy::OrderedFirstSuccess)
            && self.uses_ordered_first_success()
            && self
                .project
                .plan
                .operations
                .get(ready.plan_node.0)
                .filter(|operation| operation.id == ready.plan_node)
                .is_some_and(|operation| {
                    matches!(operation.op, ExecutableOp::SelectRoute { .. })
                        && (self.continuation_denied || self.selection_has_success(ready))
                });
        if selection_must_settle {
            0
        } else {
            1
        }
    }

    fn uses_ordered_first_success(&self) -> bool {
        matches!(
            self.project.plan.policy,
            RoutePolicy::Fallback | RoutePolicy::AnySuccess
        )
    }

    fn ordered_selection_is_ready(&self, ready: &ReadyOp) -> bool {
        let mut saw_result = false;
        for input in &ready.inputs {
            let Some(result) = self.route_result_for_node(*input) else {
                return false;
            };
            saw_result = true;
            if result.succeeded() {
                return true;
            }
        }
        saw_result
    }

    fn selection_has_success(&self, ready: &ReadyOp) -> bool {
        ready.inputs.iter().any(|input| {
            self.route_result_for_node(*input)
                .is_some_and(OExecutionResult::succeeded)
        })
    }

    fn route_result_for_node(&self, node: NodeId) -> Option<&OExecutionResult> {
        match self.values.get(&node)? {
            ProjectRuntimeValue::RouteResult(result) => Some(result),
            _ => None,
        }
    }

    fn root_is_materialized(&self) -> bool {
        self.project
            .graph
            .root_nodes
            .first()
            .is_some_and(|root| self.materialized.contains(root))
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
            ExecutableOp::SelectRoute { .. } => match self.select_results(ready) {
                Ok(selection) => {
                    let result = selection
                        .results
                        .last()
                        .cloned()
                        .expect("selection contains a result");
                    OperationResult::Finished {
                        value: ProjectRuntimeValue::SelectedResult(Box::new(
                            SelectedProjectResult {
                                result,
                                attempted_results: selection.results,
                                receipt: selection.validated_selection_receipt,
                                measurements: selection.validated_selection_measurements,
                            },
                        )),
                        workspace: None,
                    }
                }
                Err(error) => OperationResult::Aborted(error),
            },
            ExecutableOp::CompareRouteResults => match self.compare_results(ready) {
                Ok(selection) => OperationResult::Finished {
                    value: ProjectRuntimeValue::ComparedResults(selection),
                    workspace: None,
                },
                Err(error) => OperationResult::Aborted(error),
            },
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

    fn cancel_race_losers(&self) {
        if let Some(trigger) = race_trigger(self.project, self.trace.events()) {
            for (branch, token) in self.branch_tokens.iter().enumerate() {
                if Some(branch) != trigger.branch {
                    token.cancel();
                }
            }
        }
    }

    fn observe(&self, event: ValidatedSelectionProgressEventV1) -> Result<()> {
        if self.project.plan.policy == RoutePolicy::BenchmarkValidateAndSelect {
            if let Some(delivery) = &self.observer_delivery {
                let _ = delivery.send(ObserverDelivery::Event(event));
            } else if let Some(observer) = self.observer {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer.observe(event)))
                    .map_err(|_| anyhow!("validated-selection progress observer panicked"))?;
            }
        }
        Ok(())
    }

    fn flush_observer(&self) -> Result<()> {
        if let Some(delivery) = &self.observer_delivery {
            let (acknowledge, completed) = mpsc::channel();
            delivery
                .send(ObserverDelivery::Flush(acknowledge))
                .context("validated-selection progress observer stopped")?;
            completed
                .recv()
                .context("validated-selection progress observer panicked")?;
        }
        Ok(())
    }

    fn prepared_route_worker(
        &self,
        operation: &ProjectPlanOperation,
    ) -> Result<(RouteSpec, Arc<Workspace>, CancellationToken)> {
        let Some(ProjectDependency::Value(build)) = operation.dependencies.first() else {
            bail!("RunRoute lacks prepared-route input");
        };
        let ProjectRuntimeValue::PreparedRoute(prepared) = self.value_for_operation(*build)? else {
            bail!("RunRoute input is not a prepared route");
        };
        let branch = operation.branch.context("RunRoute lacks branch")?;
        if prepared.branch != branch
            || !matches!(&operation.op, ExecutableOp::RunRoute { route_id } if *route_id == prepared.route.id)
        {
            bail!("RunRoute received mismatched preparation");
        }
        Ok((
            prepared.route.clone(),
            self.workspaces
                .get(&branch)
                .context("RunRoute lacks workspace")?
                .clone(),
            self.branch_tokens[branch].clone(),
        ))
    }

    fn compare_results(&self, ready: &ReadyOp) -> Result<RouteSelectionExecution> {
        self.flush_observer()?;
        let results = ready
            .inputs
            .iter()
            .map(|input| {
                self.route_result_for_node(*input)
                    .cloned()
                    .context("comparison input has no settled route result")
            })
            .collect::<Result<Vec<_>>>()?;
        if self.project.plan.policy == RoutePolicy::BenchmarkValidateAndSelect {
            let measured = results
                .into_iter()
                .enumerate()
                .map(|(branch, result)| {
                    Ok(MeasuredRouteExecution {
                        result,
                        branch_elapsed_ns: *self
                            .branch_elapsed
                            .get(&branch)
                            .context("candidate lacks complete branch measurement")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                benchmark_validate_and_select(
                    self.bundle,
                    &self.project.plan.target,
                    &self.project.plan.alternatives,
                    measured,
                    self.observer,
                )
            }))
            .map_err(|_| {
                anyhow!("validated-selection progress observer panicked during validation")
            })?
            .map_err(|error| anyhow::Error::new(ProjectPolicyRejection(error)))
        } else {
            if results.iter().any(|result| !result.succeeded()) {
                return Err(anyhow::Error::new(ProjectPolicyRejection(anyhow!(
                    "verify_equivalent requires every alternative to succeed"
                ))));
            }
            verify_results_equivalent(&results)
                .map_err(|error| anyhow::Error::new(ProjectPolicyRejection(error)))?;
            Ok(RouteSelectionExecution::plain(results))
        }
    }

    fn select_results(&self, ready: &ReadyOp) -> Result<RouteSelectionExecution> {
        if self
            .project
            .plan
            .policy
            .requires_declared_output_validation()
        {
            let Some(ProjectRuntimeValue::ComparedResults(selection)) = ready
                .inputs
                .first()
                .and_then(|input| self.values.get(input))
            else {
                bail!("SelectRoute lacks compared results");
            };
            return Ok(RouteSelectionExecution {
                results: selection.results.clone(),
                validated_selection_receipt: selection.validated_selection_receipt.clone(),
                validated_selection_measurements: selection
                    .validated_selection_measurements
                    .clone(),
            });
        }
        if matches!(
            self.project.plan.policy,
            RoutePolicy::All
                | RoutePolicy::BenchmarkAndSelect
                | RoutePolicy::RaceSuccess
                | RoutePolicy::RaceSettle
        ) {
            let mut indexed = ready
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(branch, input)| {
                    self.route_result_for_node(*input)
                        .cloned()
                        .map(|result| (branch, result))
                })
                .collect::<Vec<_>>();
            let winner = match self.project.plan.policy {
                RoutePolicy::BenchmarkAndSelect => indexed
                    .iter()
                    .filter(|(_, result)| result.succeeded())
                    .min_by_key(|(branch, result)| (result.duration_ns, *branch))
                    .map(|(branch, _)| *branch)
                    .context("benchmark_and_select: no alternative succeeded")?,
                RoutePolicy::RaceSuccess | RoutePolicy::RaceSettle => {
                    if let Some(winner) =
                        race_selected_settlement(self.project, self.trace.events())
                    {
                        if winner.state == super::trace::ProjectAttemptState::Aborted
                            || winner
                                .branch
                                .and_then(|branch| self.project.plan.alternatives.get(branch))
                                != winner.route_id.as_ref()
                        {
                            bail!(
                                "race: selected alternative `{}` settled with an error",
                                winner
                                    .branch
                                    .and_then(|branch| self.project.plan.alternatives.get(branch))
                                    .map(String::as_str)
                                    .unwrap_or("<unknown>")
                            );
                        }
                        winner
                            .branch
                            .context("selected race settlement has no branch")?
                    } else {
                        indexed
                            .last()
                            .map(|(branch, _)| *branch)
                            .context("race: no alternative settled")?
                    }
                }
                _ => indexed
                    .last()
                    .map(|(branch, _)| *branch)
                    .context("all: no alternative settled")?,
            };
            let position = indexed
                .iter()
                .position(|(branch, _)| *branch == winner)
                .context("selected result is absent")?;
            let selected = indexed.remove(position).1;
            let mut results = indexed
                .into_iter()
                .map(|(_, result)| result)
                .collect::<Vec<_>>();
            results.push(selected);
            return Ok(RouteSelectionExecution::plain(results));
        }
        let ordered_first_success = self.uses_ordered_first_success();
        if !ordered_first_success
            && !matches!(
                self.project.plan.policy,
                RoutePolicy::Explicit(_) | RoutePolicy::Default
            )
        {
            bail!(
                "SelectRoute policy `{}` is unsupported by the ordered executor",
                self.project.plan.policy.token()
            );
        }
        if !ordered_first_success && ready.inputs.len() != 1 {
            bail!(
                "SelectRoute requires exactly one materialized route result for policy `{}`, found {}",
                self.project.plan.policy.token(),
                ready.inputs.len()
            );
        }

        let mut attempted_results = Vec::new();
        for input in &ready.inputs {
            let Some(result) = self.route_result_for_node(*input) else {
                if ordered_first_success
                    && (self.continuation_denied
                        || attempted_results.iter().any(OExecutionResult::succeeded))
                {
                    break;
                }
                bail!(
                    "SelectRoute reached an unresolved alternative before a successful short circuit"
                );
            };
            attempted_results.push(result.clone());
            if ordered_first_success && result.succeeded() {
                break;
            }
        }

        let selected = attempted_results
            .iter()
            .find(|result| ordered_first_success && result.succeeded())
            .or_else(|| attempted_results.last())
            .cloned()
            .context("SelectRoute has no materialized alternative result")?;
        let _ = selected;
        Ok(RouteSelectionExecution::plain(attempted_results))
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
        if let ProjectRuntimeValue::SelectedResult(selected) = &value {
            let SelectedProjectResult {
                result,
                attempted_results,
                receipt,
                ..
            } = selected.as_ref();
            if !matches!(
                self.project.plan.policy,
                RoutePolicy::Explicit(_)
                    | RoutePolicy::Default
                    | RoutePolicy::Fallback
                    | RoutePolicy::AnySuccess
            ) {
                let candidates = self
                    .project
                    .plan
                    .alternatives
                    .iter()
                    .enumerate()
                    .filter_map(|(branch, route)| {
                        attempted_results
                            .iter()
                            .find(|result| &result.route_id == route)
                            .map(|result| (branch, result))
                    })
                    .map(|(branch, result)| {
                        Ok(ProjectPolicyCandidate {
                            route_id: result.route_id.clone(),
                            outcome: ProjectRouteOutcome::from_result(result)?,
                            terminal_elapsed_ns: result.duration_ns.to_string(),
                            branch_elapsed_ns: self
                                .branch_elapsed
                                .get(&branch)
                                .copied()
                                .unwrap_or(result.duration_ns)
                                .to_string(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                self.trace.record_selected(
                    identity,
                    ProjectPolicySelection {
                        selected_route_id: result.route_id.clone(),
                        candidates,
                        validated_receipt: receipt.clone(),
                    },
                )?;
            } else {
                self.trace.record_finished(identity)?;
            }
        } else {
            self.trace.record_finished(identity)?;
        }
        self.values.insert(ready.value_output, value);
        if let Some((branch, workspace)) = workspace {
            self.workspaces.insert(branch, Arc::new(workspace));
        }
        self.materialized.extend(ready.outputs.iter().copied());
        Ok(())
    }

    fn commit_route_settlement(
        &mut self,
        ready: &ReadyOp,
        identity: &ProjectAttemptIdentity,
        settlement: RouteSettlement,
        completed: Instant,
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
                if is_cancellation_error(&error) {
                    if let Some(trigger) = race_trigger(self.project, self.trace.events()) {
                        let ordinal = trigger.coordinator_ordinal;
                        self.ensure_outputs_unpublished(ready)?;
                        self.trace.record_cancelled(identity, ordinal)?;
                        for output in &ready.outputs {
                            self.failed_outputs.insert(*output, ready.plan_node);
                        }
                        return Ok(());
                    }
                }
                self.commit_abort(ready, identity, &error, None)?;
                return Ok(());
            }
        };

        let branch = identity.branch.with_context(|| {
            format!(
                "route `{}` has no alternative branch",
                identity.route_id.as_deref().unwrap_or("<unknown>")
            )
        })?;
        let route_id = identity
            .route_id
            .clone()
            .context("route settlement has no route identity")?;
        let terminal_branch = self.project.plan.alternatives.get(branch) == Some(&route_id);
        if terminal_branch {
            let started = *self
                .branch_started
                .get(&branch)
                .context("terminal branch was never started")?;
            let elapsed = completed.saturating_duration_since(started).as_nanos();
            self.branch_elapsed.insert(branch, elapsed);
            self.observe(ValidatedSelectionProgressEventV1::CandidateFinished {
                declaration_index: branch,
                route_id: route_id.clone(),
                candidate_count: self.project.plan.alternatives.len(),
                branch_elapsed_ns: elapsed,
                outcome: if result.succeeded() {
                    ValidatedSelectionCandidateProgressV1::Succeeded
                } else {
                    ValidatedSelectionCandidateProgressV1::SettledUnsuccessful {
                        exit_code: result.exit_code,
                    }
                },
            })?;
        }
        let operation = self.operation(ready.plan_node)?;
        let failure_continuation = operation
            .route_facts
            .as_ref()
            .with_context(|| format!("route `{route_id}` has no RoutePlanFacts"))?
            .failure_continuation;
        let mut assessments = self
            .branch_assessments
            .get(&branch)
            .cloned()
            .unwrap_or_default();
        assessments.push(BranchRouteAssessment {
            route_id: route_id.clone(),
            executed: !matches!(status, SettledRouteStatus::Skipped),
            failure_continuation,
        });
        let continuation = if !matches!(status, SettledRouteStatus::Succeeded)
            && self.uses_ordered_first_success()
            && self
                .project
                .plan
                .alternatives
                .get(branch)
                .is_some_and(|alternative| alternative == &route_id)
        {
            self.project
                .plan
                .alternatives
                .get(branch + 1)
                .map(|next_route| {
                    let evidence = if assessments.iter().all(|entry| !entry.executed) {
                        ProjectContinuationEvidence::NoExecution
                    } else if assessments
                        .iter()
                        .filter(|entry| entry.executed)
                        .all(|entry| {
                            entry.failure_continuation
                                == RouteFailureContinuation::DeclaredIdempotent
                        })
                    {
                        ProjectContinuationEvidence::DeclaredIdempotent
                    } else if self.project.plan.execution_contract
                        == ProjectExecutionContract::LegacyCompatibility
                    {
                        ProjectContinuationEvidence::LegacyUnchecked
                    } else {
                        ProjectContinuationEvidence::UnprovenEffects
                    };
                    ProjectContinuationDecision::new(
                        next_route.clone(),
                        assessments
                            .iter()
                            .map(|entry| entry.route_id.clone())
                            .collect(),
                        evidence,
                    )
                })
                .transpose()?
        } else {
            None
        };
        let continuation_denied = continuation
            .as_ref()
            .is_some_and(|decision| !decision.admitted);

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
                // Admission evidence is an initially materialized input, never
                // a route-produced settlement output.
                HNodeKind::AdmissionEvidence { .. } => false,
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
            SettledRouteStatus::NonZero => self.trace.record_settled_failure_with_continuation(
                identity,
                outcome,
                continuation.clone(),
            )?,
            SettledRouteStatus::Skipped => self.trace.record_skipped_with_continuation(
                identity,
                outcome,
                continuation.clone(),
            )?,
        }
        let exit_code = result.exit_code;
        self.values
            .insert(ready.value_output, ProjectRuntimeValue::RouteResult(result));
        self.branch_assessments.insert(branch, assessments);
        self.materialized.extend(publish);
        for output in withhold {
            self.failed_outputs.insert(output, ready.plan_node);
        }
        if matches!(status, SettledRouteStatus::NonZero) {
            let description = format!(
                "route `{}` settled unsuccessfully with exit code {exit_code:?}",
                identity.route_id.as_deref().unwrap_or("<unknown>")
            );
            self.failures.insert(ready.plan_node, description.clone());
            self.public_failures.insert(ready.plan_node, description);
        }
        if continuation_denied {
            self.continuation_denied = true;
            let description = format!(
                "route `{route_id}` settled unsuccessfully, but continuation to `{}` was denied because branch {branch} contains executed routes without a declared_idempotent contract",
                continuation
                    .as_ref()
                    .expect("denied continuation exists")
                    .next_route_id
            );
            self.failures.insert(ready.plan_node, description.clone());
            self.public_failures.insert(ready.plan_node, description);
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
        if !error.is::<ProjectPolicyRejection>() {
            self.infrastructure_failure_observed = true;
        }
        self.ensure_outputs_unpublished(ready)?;
        let description = error.to_string();
        let public_description = public_route_execution_diagnostic(error);

        // An abort has no operation value. Its terminal trace event and the
        // withholding of every graph output form the local commit.
        self.trace
            .record_aborted(identity, outcome, description.as_bytes())?;
        for output in &ready.outputs {
            self.failed_outputs.insert(*output, ready.plan_node);
        }
        self.failures.insert(ready.plan_node, description);
        self.public_failures
            .insert(ready.plan_node, public_description);
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
        let mut public_details = details
            .iter()
            .take(details.len().saturating_sub(self.failures.len()))
            .cloned()
            .collect::<Vec<_>>();
        for (plan_node, failure) in &self.public_failures {
            public_details.push(format!("p{} failed: {failure}", plan_node.0));
        }
        let message = if details.is_empty() {
            "project HGraph stalled without a materialized selected-result root".to_string()
        } else {
            format!("project HGraph stalled: {}", details.join("; "))
        };
        let public_message = if public_details.is_empty() {
            "project HGraph stalled without a materialized selected-result root".to_string()
        } else {
            format!("project HGraph stalled: {}", public_details.join("; "))
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
            public_message,
            class: if self.infrastructure_failure_observed || self.failures.is_empty() {
                ProjectExecutionFailureClass::Infrastructure
            } else {
                ProjectExecutionFailureClass::Semantic
            },
            trace: self.trace.clone(),
            settled_results,
            materialized_outputs: self.materialized.clone(),
            failed_outputs: self.failed_outputs.keys().copied().collect(),
        })
    }
}

/// Execute one validated hosted project HGraph under a supported route policy.
pub fn execute_project_hgraph(
    bundle: &ProjectBundle,
    project: &ProjectHGraph,
    opts: &RunOptions,
) -> Result<ProjectExecutionOutcome> {
    ProjectCoordinator::new(bundle, project, opts)?.execute()
}

/// Execute through the bounded World-hosted coordinator entry point.
///
/// The supplied launch/current view is checked before any project execution.
/// A successful result remains a residual `HostWorld` observation until the
/// higher-level receipt adapter emits an explicitly uncommitted OWRECEIPT.
#[allow(clippy::too_many_arguments)]
pub fn execute_project_hgraph_world_bound(
    bundle: &ProjectBundle,
    project: &ProjectHGraph,
    opts: &RunOptions,
    deployment: &DeploymentPlanV1,
    snapshot: &super::deployment::PlacementSnapshotV1,
    launch: &HostedWorldLaunchV1,
    current: &HostedWorldCurrentV1,
) -> Result<ProjectExecutionOutcome> {
    ProjectCoordinator::new_world_bound(
        bundle, project, opts, deployment, snapshot, launch, current,
    )?
    .execute()
}

/// Execute one validated hosted project HGraph while retaining the exact
/// ordered alternative-result prefix expected by route-selection callers.
/// Prerequisite route results remain in the trace and are not alternatives.
pub fn execute_project_hgraph_selection(
    bundle: &ProjectBundle,
    project: &ProjectHGraph,
    opts: &RunOptions,
) -> Result<ConfiguredProjectExecution> {
    execute_project_hgraph_selection_with_contract_and_progress(
        bundle,
        project,
        opts,
        ProjectExecutionContract::Strict,
        None,
    )
}

/// Execute a preflighted graph using the caller-frozen continuation contract
/// and optional presentation-safe progress observer. No environment is read.
pub fn execute_project_hgraph_selection_with_contract_and_progress(
    bundle: &ProjectBundle,
    project: &ProjectHGraph,
    opts: &RunOptions,
    expected_contract: ProjectExecutionContract,
    observer: Option<&dyn ValidatedSelectionProgressObserverV1>,
) -> Result<ConfiguredProjectExecution> {
    let mut coordinator =
        ProjectCoordinator::new_with_contract(bundle, project, opts, expected_contract)?;
    coordinator.observer = observer;
    let outcome = coordinator.execute_with_attempts()?;
    Ok(ConfiguredProjectExecution {
        results: outcome.attempted_results,
        trace: Some(outcome.trace),
        validated_selection_receipt: outcome.receipt,
        validated_selection_measurements: outcome.measurements,
    })
}

fn project_trace_header(project: &ProjectHGraph) -> Result<ProjectAttemptTraceHeader> {
    let logical_graph_digest = project_logical_graph_digest(project)?;
    let deployment_plan_digest = project_hosted_deployment_digest(project)?;

    let mut attempt = [0_u8; 32];
    getrandom::fill(&mut attempt)
        .context("failed to obtain entropy for project execution attempt identity")?;

    Ok(ProjectAttemptTraceHeader::new(
        project.plan.project_name.clone(),
        project.plan.bundle_digest.clone(),
        project.plan.target.clone(),
        project.plan.policy.token(),
        super::logical::LOGICAL_HGRAPH_SCHEMA_V1,
        logical_graph_digest,
        super::deployment::DEPLOYMENT_PLAN_SCHEMA_V1,
        deployment_plan_digest,
        hex::encode(attempt),
    ))
}

fn project_world_trace_header(
    project: &ProjectHGraph,
    deployment: &DeploymentPlanV1,
    launch: &HostedWorldLaunchV1,
) -> Result<ProjectAttemptTraceHeader> {
    let logical_graph_digest = project_logical_graph_digest(project)?;
    let deployment_plan_digest = project_deployment_digest(deployment)?;
    let execution_attempt_id = launch.coordinator_attempt().to_string();

    Ok(ProjectAttemptTraceHeader::new(
        project.plan.project_name.clone(),
        project.plan.bundle_digest.clone(),
        project.plan.target.clone(),
        project.plan.policy.token(),
        super::logical::LOGICAL_HGRAPH_SCHEMA_V1,
        logical_graph_digest,
        super::deployment::DEPLOYMENT_PLAN_SCHEMA_V1,
        deployment_plan_digest,
        execution_attempt_id,
    ))
}

/// Dispatch project selection through the explicitly configured runtime.
///
/// Unset selects the compatibility HGraph contract; `hgraph` retains strict
/// continuation, and `legacy` explicitly selects the previous runtime. Graph
/// errors are returned directly and never trigger a legacy fallback.
pub fn execute_selection_with_configured_executor(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
) -> Result<ConfiguredProjectExecution> {
    execute_selection_with_configured_executor_inner(bundle, target, policy_override, opts, None)
}

/// Dispatch project selection through the configured runtime while reporting
/// presentation-safe validated-selection progress for both HGraph and explicit
/// legacy execution of `benchmark_validate_and_select`.
pub fn execute_selection_with_configured_executor_with_progress(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
    observer: &dyn ValidatedSelectionProgressObserverV1,
) -> Result<ConfiguredProjectExecution> {
    execute_selection_with_configured_executor_inner(
        bundle,
        target,
        policy_override,
        opts,
        Some(observer),
    )
}

fn execute_selection_with_configured_executor_inner(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
    observer: Option<&dyn ValidatedSelectionProgressObserverV1>,
) -> Result<ConfiguredProjectExecution> {
    let configured = std::env::var_os(PROJECT_EXECUTOR_ENV);
    match configured.as_deref() {
        Some(value) if value == "legacy" => {
            let execution = match observer {
                Some(observer) => super::runtime::run_selection_observed_with_progress(
                    bundle,
                    target,
                    policy_override,
                    opts,
                    observer,
                )?,
                None => {
                    super::runtime::run_selection_observed(bundle, target, policy_override, opts)?
                }
            };
            Ok(ConfiguredProjectExecution {
                results: execution.results,
                trace: None,
                validated_selection_receipt: execution.validated_selection_receipt,
                validated_selection_measurements: execution.validated_selection_measurements,
            })
        }
        None | Some(_) if configured.is_none() || configured.as_deref().is_some_and(|value| value == "hgraph") => {
            let contract = if configured.is_none() { ProjectExecutionContract::LegacyCompatibility } else { ProjectExecutionContract::Strict };
            let project = build_project_hgraph_with_contract(bundle, target, policy_override, contract)
                .map_err(anyhow::Error::msg)
                .context("failed to build project HGraph for execution")?;
            let mut coordinator = ProjectCoordinator::new_with_contract(bundle, &project, opts, contract)?;
            coordinator.observer = observer;
            let outcome = coordinator.execute_with_attempts()?;
            Ok(ConfiguredProjectExecution { results: outcome.attempted_results, trace: Some(outcome.trace), validated_selection_receipt: outcome.receipt, validated_selection_measurements: outcome.measurements })
        }
        Some(value) => bail!(
            "unsupported {PROJECT_EXECUTOR_ENV} value `{}`; expected `hgraph`, `legacy`, or an unset variable",
            value.to_string_lossy()
        ),
        None => unreachable!("unset executor selects compatibility HGraph"),
    }
}

/// Compatibility wrapper retaining the pre-ProjectExec-A result-only API.
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
        executable: route.command.first().cloned(),
        evaluator: route.evaluator.clone(),
        entrypoint: route.entrypoint.clone(),
        prerequisites: route.prerequisites.clone(),
        guards: route.guards.clone(),
        environment_keys: route.environment.keys().cloned().collect(),
        inputs: route.inputs.clone(),
        outputs: route.outputs.clone(),
        declared_reads: route.effects.reads.clone(),
        declared_writes: route.effects.writes.clone(),
        declared_pure: route.effects.pure,
        failure_continuation: route.failure_continuation,
    }
}

fn route_settlement(result: Result<OExecutionResult>) -> RouteSettlement {
    match result {
        Ok(result) => match ProjectRouteOutcome::from_result(&result) {
            Ok(outcome) if is_skipped_result(&result) => {
                RouteSettlement::Skipped { result, outcome }
            }
            Ok(outcome) if result.succeeded() => RouteSettlement::Succeeded { result, outcome },
            Ok(outcome) => RouteSettlement::NonZero { result, outcome },
            Err(error) => RouteSettlement::Aborted(error.into()),
        },
        Err(error) => RouteSettlement::Aborted(error),
    }
}

#[cfg(test)]
mod policy_settlement_tests {
    use super::*;
    use crate::project::{build_project_hgraph, RouteProvenance, RouteSet};

    #[test]
    fn race_settle_post_drain_tie_break_includes_real_errors_in_declaration_order() {
        for earlier_branch_succeeds in [true, false] {
            let mut bundle = ProjectBundle::empty("injected-race-settlement-order");
            for branch in 0..2 {
                let mut route =
                    RouteSpec::new(format!("branch-{branch}"), RouteProvenance::CliOverride);
                route.command = if (branch == 0) == earlier_branch_succeeds {
                    vec!["sh".into(), "-c".into(), "printf settled".into()]
                } else {
                    vec!["/ostadix-test/nonexistent-race-executable".into()]
                };
                bundle.routes.push(route);
            }
            bundle.route_sets.push(RouteSet {
                provides: "race".into(),
                alternatives: vec!["branch-0".into(), "branch-1".into()],
                policy: RoutePolicy::RaceSettle,
            });
            let project = build_project_hgraph(&bundle, Some("race"), None).unwrap();
            let opts = RunOptions::default();
            let mut coordinator = ProjectCoordinator::new(&bundle, &project, &opts).unwrap();
            let mut settled = BTreeMap::new();
            // Produce both actual outcomes before delivering either terminal
            // event. This deterministically models already-returned workers.
            for operation in &project.plan.operations {
                if matches!(operation.op, ExecutableOp::SelectRoute { .. }) {
                    continue;
                }
                let ready = coordinator
                    .schedule
                    .ops
                    .iter()
                    .find(|ready| ready.plan_node == operation.id)
                    .unwrap()
                    .clone();
                assert!(coordinator.operation_is_ready(&ready));
                let identity = ProjectAttemptIdentity::from_operation(operation).unwrap();
                coordinator.trace.record_ready(&identity).unwrap();
                coordinator.trace.record_started(&identity).unwrap();
                if matches!(operation.op, ExecutableOp::MaterializeProject) {
                    coordinator
                        .branch_started
                        .insert(operation.branch.unwrap(), Instant::now());
                }
                match coordinator.execute_operation(&ready, operation) {
                    OperationResult::Finished { value, workspace } => coordinator
                        .commit_finished(&ready, &identity, value, workspace)
                        .unwrap(),
                    OperationResult::Route(outcome) => {
                        settled.insert(
                            operation.branch.unwrap(),
                            (ready, identity, outcome, Instant::now()),
                        );
                    }
                    OperationResult::Aborted(error) => {
                        panic!("unexpected preparation error: {error}")
                    }
                }
            }
            for branch in [1, 0] {
                let (ready, identity, outcome, completed) = settled.remove(&branch).unwrap();
                coordinator
                    .commit_route_settlement(&ready, &identity, outcome, completed)
                    .unwrap();
                coordinator.cancel_race_losers();
            }
            assert_eq!(
                race_trigger(&project, coordinator.trace.events())
                    .unwrap()
                    .branch,
                Some(1)
            );
            assert_eq!(
                race_selected_settlement(&project, coordinator.trace.events())
                    .unwrap()
                    .branch,
                Some(0)
            );
            let operation = project.plan.operations.last().unwrap();
            let ready = coordinator
                .schedule
                .ops
                .iter()
                .find(|ready| ready.plan_node == operation.id)
                .unwrap()
                .clone();
            assert!(coordinator.operation_is_ready(&ready));
            let identity = ProjectAttemptIdentity::from_operation(operation).unwrap();
            coordinator.trace.record_ready(&identity).unwrap();
            coordinator.trace.record_started(&identity).unwrap();
            match coordinator.execute_operation(&ready, operation) {
                OperationResult::Finished { value, workspace } => {
                    assert!(earlier_branch_succeeds);
                    let ProjectRuntimeValue::SelectedResult(selected) = &value else {
                        panic!("missing selected result")
                    };
                    assert_eq!(selected.result.route_id, "branch-0");
                    coordinator
                        .commit_finished(&ready, &identity, value, workspace)
                        .unwrap();
                }
                OperationResult::Aborted(error) => {
                    assert!(!earlier_branch_succeeds);
                    assert!(error.to_string().contains("branch-0"));
                    let result = coordinator
                        .values
                        .values()
                        .find_map(|value| match value {
                            ProjectRuntimeValue::RouteResult(result) => Some(result),
                            _ => None,
                        })
                        .unwrap();
                    let mut forged = coordinator.trace.clone();
                    forged
                        .record_selected(
                            &identity,
                            ProjectPolicySelection {
                                selected_route_id: result.route_id.clone(),
                                candidates: vec![ProjectPolicyCandidate {
                                    route_id: result.route_id.clone(),
                                    outcome: ProjectRouteOutcome::from_result(result).unwrap(),
                                    terminal_elapsed_ns: result.duration_ns.to_string(),
                                    branch_elapsed_ns: "0".into(),
                                }],
                                validated_receipt: None,
                            },
                        )
                        .unwrap();
                    assert!(ProjectAttemptTrace::try_from_project_events(
                        &project,
                        forged.header().clone(),
                        forged.events().to_vec()
                    )
                    .is_err());
                    coordinator
                        .commit_abort(&ready, &identity, &error, None)
                        .unwrap();
                }
                OperationResult::Route(_) => panic!("selector dispatched a route"),
            }
            ProjectAttemptTrace::try_from_project_events(
                &project,
                coordinator.trace.header().clone(),
                coordinator.trace.events().to_vec(),
            )
            .unwrap();
        }
    }
}
