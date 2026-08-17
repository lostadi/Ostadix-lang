//! The graph-execution coordinator.
//!
//! The coordinator owns the mutable execution state for one plan evaluation and
//! drives a readiness-based event loop over the plan's operation hyperedges.
//! An operation becomes ready exactly when all ordinary and synthetic input
//! nodes are materialized. Data, source completion, resource state, and actor
//! state therefore share one directed producer/input dependency rule.
//!
//! Compiler-verified O scope reads and closed, attribute-free inline renderers
//! execute as owned tasks on one bounded worker pool reused for the entire
//! coordinator run. Each physical completion immediately reopens scheduling;
//! semantic settlement remains ordinal and deterministic. Every other
//! operation — anything that needs the evaluator's `!Send` process registry or
//! mutable state — runs on the coordinator thread. State/control inputs,
//! rather than commit order, preserve externally observable effect ordering.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};

use crate::effects::EffectSummary;
use crate::eval_core::{
    derive_policy_contexts, trace_fingerprint, ExecutionTrace, GraphEvalFrame, GraphEvaluationHost,
};
use crate::evidence::{AdmittedExecution, DispatchAdapterV1, DispatchLaneV1, FailureClassV1};
use crate::execution_contract::{validate_execution_metadata, Policy};
use crate::hgraph::{schedule::ReadySchedule, NodeId, ValueState};
use crate::ir::{ExecutionPlan, OIr, OIrProgram, PlanNodeId, PlanNodeKind};
use crate::value::OValue;

use super::parallel;
use super::pool::WorkerPool;
use super::task::{
    TaskCallbackFailure, TaskCompletion, TaskEvalRequest, TaskOutcome, TaskSubmission, TaskToken,
    WorkerEvent,
};
use super::trace::TraceSink;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpRunState {
    Pending,
    InFlight,
    Buffered,
    /// A verified-pure, admitted-infallible result whose outputs may unlock
    /// more worker-only computation before its deterministic trace frontier.
    Published,
    Settled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerFailureKind {
    Semantic,
    Infrastructure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerCompletionDisposition {
    Continue,
    AbortInfrastructure,
}

struct WorkerFailure {
    index: usize,
    error: anyhow::Error,
    kind: WorkerFailureKind,
}

struct WorkerPublication {
    output_type: String,
    fingerprint: Option<String>,
}

/// One committed-or-pending operation the coordinator tracks.
struct OpState {
    plan_node: PlanNodeId,
    ordinal: u64,
    value_output: NodeId,
    inputs: Vec<NodeId>,
    outputs: Vec<NodeId>,
    effect: EffectSummary,
    dispatch_lane: DispatchLaneV1,
    dispatch_adapter: DispatchAdapterV1,
    failure_class: FailureClassV1,
    state: OpRunState,
}

pub struct Coordinator<'a> {
    admitted: AdmittedExecution<'a>,
    program: &'a OIrProgram,
    plan: &'a ExecutionPlan,
    flat: Vec<&'a OIr>,
    ops: Vec<OpState>,
    materialized: HashSet<NodeId>,
    failed_outputs: HashMap<NodeId, String>,
    worker_results: HashMap<usize, TaskOutcome>,
    worker_publications: HashMap<usize, WorkerPublication>,
    frame: GraphEvalFrame,
    trace: TraceSink,
    base_policy: Policy,
}

impl<'a> Coordinator<'a> {
    /// Build a coordinator only from a frozen, digest-checked admission. Raw
    /// HGraphs cannot cross this authority boundary.
    pub fn new(admitted: AdmittedExecution<'a>) -> Result<Self> {
        let program = admitted.program();
        let plan = admitted.plan();
        let hgraph = admitted.graph();
        let base_policy = admitted.admission().base_policy();
        let flat = program.flatten_for_plan();
        if flat.len() != plan.nodes.len() {
            bail!(
                "OIR flatten produced {} nodes but plan has {} nodes",
                flat.len(),
                plan.nodes.len()
            );
        }

        hgraph
            .validate_admitted_execution_graph()
            .map_err(anyhow::Error::msg)?;
        hgraph
            .validate_execution_source(program, plan)
            .map_err(anyhow::Error::msg)?;
        let admitted_operations = admitted
            .admission()
            .operations()
            .iter()
            .map(|operation| (operation.plan_node, operation))
            .collect::<HashMap<_, _>>();
        let schedule = ReadySchedule::derive(hgraph).map_err(anyhow::Error::msg)?;
        let ops = schedule
            .ops
            .iter()
            .map(|op| {
                let effect = hgraph
                    .effect_summary(op.plan_node)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "operation {} has no lowered effect summary",
                            op.plan_node.0
                        )
                    })?;
                let dispatch = &admitted_operations
                    .get(&op.plan_node)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "operation {} has no admitted dispatch contract",
                            op.plan_node.0
                        )
                    })?
                    .evidence;
                Ok(OpState {
                    plan_node: op.plan_node,
                    ordinal: op.ordinal,
                    value_output: op.value_output,
                    inputs: op.inputs.clone(),
                    outputs: op.outputs.clone(),
                    effect,
                    dispatch_lane: dispatch.dispatch_contract.lane,
                    dispatch_adapter: dispatch.dispatch_contract.adapter,
                    failure_class: dispatch.failure_contract.class,
                    state: OpRunState::Pending,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let node_policy = derive_policy_contexts(plan, &flat, base_policy)?;
        let materialized = hgraph
            .nodes
            .iter()
            .filter_map(|(id, node)| (node.state == ValueState::Materialized).then_some(*id))
            .collect();

        let frame = GraphEvalFrame {
            values: vec![None; plan.nodes.len()],
            base_scope: std::collections::HashMap::new(),
            node_policy,
            trace: ExecutionTrace::new(),
        };

        Ok(Self {
            admitted,
            program,
            plan,
            flat,
            ops,
            materialized,
            failed_outputs: HashMap::new(),
            worker_results: HashMap::new(),
            worker_publications: HashMap::new(),
            frame,
            trace: TraceSink::new(),
            base_policy,
        })
    }

    /// Drive the plan to completion, committing store deltas and root results
    /// into `scope` in deterministic root order. Returns the last non-null,
    /// non-whitespace root value (the document value).
    pub(crate) fn run_host(
        mut self,
        evaluator: &mut dyn GraphEvaluationHost,
        scope: &mut std::collections::HashMap<String, OValue>,
    ) -> Result<OValue> {
        evaluator.verify_admitted_runtime_context(&self.admitted)?;
        validate_execution_metadata(&self.flat)?;
        self.frame.base_scope = scope.clone();

        self.materialize_literals()?;

        if let Err(err) = self.drive(evaluator) {
            evaluator.install_execution_trace(std::mem::take(&mut self.trace).into_trace());
            return Err(err);
        }

        let last = self.commit(scope)?;

        let last = if self.base_policy == Policy::Autonomous {
            evaluator.flush_autonomous_buffer()?;
            evaluator.resolve_after_flush(last)?
        } else {
            last
        };

        evaluator.install_execution_trace(std::mem::take(&mut self.trace).into_trace());
        Ok(last)
    }

    /// Literal `Text` plan nodes carry no operation hyperedge; they start
    /// materialized. Emit their lifecycle events so every plan node is traced
    /// exactly once, matching the reference executor.
    fn materialize_literals(&mut self) -> Result<()> {
        let mut literals: Vec<(PlanNodeId, String)> = Vec::new();
        for node in &self.plan.nodes {
            if let PlanNodeKind::Text = node.kind {
                if let OIr::Text(text) = self.flat[node.id.0] {
                    literals.push((node.id, text.clone()));
                }
            }
        }
        for (id, text) in literals {
            let value = OValue::str_(text);
            self.trace.ready(id);
            self.trace.started(id);
            self.trace
                .finished(id, value.type_name().to_string(), trace_fingerprint(&value));
            self.frame.set_value(id, value)?;
        }
        Ok(())
    }

    /// The readiness-driven event loop.
    fn drive(&mut self, evaluator: &mut dyn GraphEvaluationHost) -> Result<()> {
        let worker_operations = self
            .ops
            .iter()
            .filter(|op| op.dispatch_lane == DispatchLaneV1::LocalWorker)
            .count();
        let worker_capacity = self
            .admitted
            .admission()
            .resolved_worker_count(evaluator.local_worker_parallelism_override());
        #[cfg(test)]
        let worker_capacity = parallel::worker_count_hint()
            .unwrap_or(worker_capacity)
            .max(1);
        let mut pool = (worker_operations > 0)
            .then(|| WorkerPool::new(worker_capacity))
            .transpose()?;

        loop {
            if let Some(pool) = pool.as_mut() {
                loop {
                    let event = match pool.try_recv_event() {
                        Ok(Some(event)) => event,
                        Ok(None) => break,
                        Err(error) => {
                            return Err(self.abort_after_worker_error(
                                Some(pool),
                                "local worker completion channel failed",
                                error,
                            ));
                        }
                    };
                    self.accept_worker_event_or_abort(evaluator, pool, event)?;
                }
            }

            if let Some(failure) = self.settle_buffered_results() {
                let failed_id = self.ops[failure.index].plan_node;
                let reason = match failure.kind {
                    WorkerFailureKind::Semantic => format!(
                        "strict fail-stop withheld result after operation {} failed",
                        failed_id.0
                    ),
                    WorkerFailureKind::Infrastructure => format!(
                        "local worker infrastructure aborted operation {}",
                        failed_id.0
                    ),
                };
                self.discard_started_workers(pool.as_mut(), &reason);
                return Err(failure.error);
            }

            if self.ops.iter().all(|op| op.state == OpRunState::Settled) {
                return Ok(());
            }

            let ready = self.ready_ops();
            if let Some(index) = self.lowest_unsettled().filter(|&index| {
                ready.contains(&index)
                    && self.ops[index].state == OpRunState::Pending
                    && self.ops[index].dispatch_lane == DispatchLaneV1::LocalWorker
                    && !self.is_worker_safe(index)
            }) {
                let error = anyhow::anyhow!(
                    "operation {} cannot satisfy its admitted local-worker preparation contract",
                    self.ops[index].plan_node.0
                );
                return Err(self.abort_after_worker_error(
                    pool.as_mut(),
                    "scheduler aborted after a local-worker preparation contract mismatch",
                    error,
                ));
            }

            let mut dispatched = false;
            if let Some(worker_pool) = pool.as_mut() {
                let candidates =
                    self.worker_dispatch_candidates(&ready, worker_pool.available_slots());
                if !candidates.is_empty() {
                    if let Err(error) = self.dispatch_workers(evaluator, worker_pool, &candidates) {
                        return Err(self.abort_after_worker_error(
                            Some(worker_pool),
                            "scheduler aborted while submitting prepared local-worker tasks",
                            error,
                        ));
                    }
                    dispatched = true;
                }
            }
            if dispatched {
                continue;
            }

            let workers_outstanding = pool.as_ref().map_or(0, WorkerPool::outstanding);
            if workers_outstanding == 0 {
                if let Some(coordinator) = self.coordinator_at_settlement_frontier(&ready) {
                    if let Err(error) = self.run_coordinator_op(evaluator, coordinator) {
                        self.discard_started_workers(
                            pool.as_mut(),
                            &format!(
                                "strict fail-stop withheld result after operation {} failed",
                                self.ops[coordinator].plan_node.0
                            ),
                        );
                        return Err(error);
                    }
                    continue;
                }
            }

            if let Some(worker_pool) = pool.as_mut().filter(|pool| pool.outstanding() > 0) {
                let event = match worker_pool.recv_event() {
                    Ok(event) => event,
                    Err(error) => {
                        return Err(self.abort_after_worker_error(
                            Some(worker_pool),
                            "local worker completion channel failed",
                            error,
                        ));
                    }
                };
                self.accept_worker_event_or_abort(evaluator, worker_pool, event)?;
                continue;
            }

            let remaining = self
                .ops
                .iter()
                .filter(|op| op.state != OpRunState::Settled)
                .count();
            self.discard_started_workers(
                pool.as_mut(),
                "scheduler stalled before a started local-worker result could settle",
            );
            bail!(
                "graph executor stalled: {remaining} of {} operations never became ready \
                 (dependency cycle, failed input, or unsatisfiable constraint; \
                 {} failed outputs)",
                self.ops.len(),
                self.failed_outputs.len()
            );
        }
    }

    /// Indices of operations for which every ordinary/state/control input has
    /// materialized successfully.
    fn ready_ops(&self) -> Vec<usize> {
        let mut ready: Vec<usize> = (0..self.ops.len())
            .filter(|&index| {
                let op = &self.ops[index];
                op.state == OpRunState::Pending
                    && op
                        .inputs
                        .iter()
                        .all(|input| self.materialized.contains(input))
            })
            .collect();
        ready.sort_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0));
        ready
    }

    /// Whether an operation may run on a worker thread: admission must select
    /// the local-worker lane, the hard effect/failure contract must be safe,
    /// and a Send-only preparation adapter must still be available. Source
    /// assertions cannot establish this class.
    fn is_worker_safe(&self, index: usize) -> bool {
        let id = self.ops[index].plan_node;
        if self.ops[index].dispatch_lane != DispatchLaneV1::LocalWorker {
            return false;
        }
        if !parallel::effect_contract_worker_safe(&self.ops[index].effect, self.flat[id.0]) {
            return false;
        }
        parallel::adapter_matches(
            self.ops[index].dispatch_adapter,
            self.plan,
            id,
            self.flat[id.0],
        ) && parallel::render_inputs_pure(&self.frame, self.plan, id)
    }

    fn lowest_unsettled(&self) -> Option<usize> {
        (0..self.ops.len())
            .filter(|&index| self.ops[index].state != OpRunState::Settled)
            .min_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0))
    }

    fn coordinator_at_settlement_frontier(&self, ready: &[usize]) -> Option<usize> {
        let index = self.lowest_unsettled()?;
        (self.ops[index].state == OpRunState::Pending
            && self.ops[index].dispatch_lane == DispatchLaneV1::Coordinator
            && ready.contains(&index))
        .then_some(index)
    }

    fn worker_dispatch_candidates(&self, ready: &[usize], slots: usize) -> Vec<usize> {
        if slots == 0 {
            return Vec::new();
        }
        if self.lowest_unsettled().is_some_and(|index| {
            self.ops[index].state == OpRunState::Pending
                && self.ops[index].dispatch_lane == DispatchLaneV1::Coordinator
                && ready.contains(&index)
        }) {
            // Do not let preparation of a later speculative task become an
            // observable error before a ready coordinator-frontier operation.
            return Vec::new();
        }

        // Unknown hosted effects may overlap only among direct members of the
        // same explicitly autonomous group, and only after every earlier
        // semantic operation outside that group has settled. This prevents an
        // autonomous region from leaking speculative effects backward across
        // its source-order boundary.
        let mut autonomous_ready = ready
            .iter()
            .copied()
            .filter_map(|index| {
                (self.ops[index].dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1
                    && self.is_worker_safe(index))
                .then(|| {
                    crate::dispatch_model::autonomous_ephemeral_group(
                        self.plan,
                        self.ops[index].plan_node,
                        self.flat[self.ops[index].plan_node.0],
                    )
                    .map(|group| (index, group))
                })
                .flatten()
            })
            .collect::<Vec<_>>();
        autonomous_ready
            .sort_by_key(|(index, _)| (self.ops[*index].ordinal, self.ops[*index].plan_node.0));
        if let Some((first, group)) = autonomous_ready.first().copied() {
            let boundary_clear = (0..self.ops.len()).all(|index| {
                self.ops[index].state == OpRunState::Settled
                    || self.ops[index].ordinal >= self.ops[first].ordinal
                    || crate::dispatch_model::autonomous_ephemeral_group(
                        self.plan,
                        self.ops[index].plan_node,
                        self.flat[self.ops[index].plan_node.0],
                    ) == Some(group)
            });
            if boundary_clear {
                return autonomous_ready
                    .into_iter()
                    .filter_map(|(index, candidate_group)| {
                        (candidate_group == group).then_some(index)
                    })
                    .take(slots)
                    .collect();
            }
        }

        // Reserve the front of the pool for the strict fallible prefix before
        // admitting broader infallible speculation. Otherwise a wide renderer
        // frontier could delay the very load whose settlement controls forward
        // progress and deterministic failure selection.
        let ready_set = ready.iter().copied().collect::<HashSet<_>>();
        let mut selected = Vec::new();
        let mut unfinished = (0..self.ops.len())
            .filter(|&index| self.ops[index].state != OpRunState::Settled)
            .collect::<Vec<_>>();
        unfinished.sort_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0));
        for index in unfinished {
            match self.ops[index].state {
                OpRunState::InFlight | OpRunState::Buffered
                    if self.ops[index].dispatch_lane == DispatchLaneV1::LocalWorker
                        && self.ops[index].failure_class
                            == FailureClassV1::MayFailNoExternalEffects =>
                {
                    continue;
                }
                OpRunState::Pending
                    if ready_set.contains(&index)
                        && self.is_worker_safe(index)
                        && self.ops[index].failure_class
                            == FailureClassV1::MayFailNoExternalEffects =>
                {
                    selected.push(index);
                    if selected.len() == slots {
                        break;
                    }
                }
                _ => break,
            }
        }
        if selected.len() == slots {
            return selected;
        }

        selected.extend(
            ready
                .iter()
                .copied()
                .filter(|&index| {
                    self.is_worker_safe(index)
                        && self.ops[index].failure_class == FailureClassV1::Infallible
                })
                .take(slots - selected.len()),
        );
        selected
    }

    fn dispatch_workers(
        &mut self,
        evaluator: &dyn GraphEvaluationHost,
        pool: &mut WorkerPool,
        selected: &[usize],
    ) -> Result<()> {
        if selected.iter().any(|&index| {
            self.ops[index].dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1
        }) {
            // Recheck mutable shim/environment context and the already-selected
            // executable identities immediately before preparation. Direct
            // commands are never re-resolved through PATH; drift after
            // admission must fail before Ready/Started is observable.
            evaluator.verify_admitted_runtime_context(&self.admitted)?;
            for &index in selected {
                if self.ops[index].dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1
                {
                    let OIr::Exec { backend, .. } = self.flat[self.ops[index].plan_node.0] else {
                        unreachable!("ephemeral shim adapter requires an Exec node")
                    };
                    self.admitted
                        .executable_leases()?
                        .verify_backend(&backend.canonical)?;
                }
            }
        }
        let mut prepared = Vec::with_capacity(selected.len());
        for &index in selected {
            let id = self.ops[index].plan_node;
            let task = parallel::prepare(
                self.ops[index].dispatch_adapter,
                &self.frame,
                self.plan,
                id,
                self.flat[id.0],
                match self.ops[index].dispatch_adapter {
                    DispatchAdapterV1::AutonomousEphemeralShimV1 => {
                        let OIr::Exec { backend, .. } = self.flat[id.0] else {
                            unreachable!("ephemeral shim adapter requires an Exec node")
                        };
                        let authority_scope = self.frame.scope_from_data_edges(id, self.plan)?;
                        let sandbox = evaluator
                            .authorize_autonomous_ephemeral_shim(backend, &authority_scope)?;
                        Some(parallel::EphemeralShimRuntime::new(
                            evaluator.shim_path(&backend.canonical),
                            sandbox,
                            self.admitted.executable_leases()?,
                        ))
                    }
                    _ => None,
                },
            )?;
            crate::process::lifecycle_trace(
                "coordinator.task_prepared",
                format!("token={index} plan_node={}", id.0),
            );
            prepared.push((index, id, task));
        }

        for (index, id, task) in prepared {
            pool.submit(TaskSubmission::new(TaskToken(index), task))?;
            crate::process::lifecycle_trace(
                "coordinator.task_submitted",
                format!("token={index} plan_node={}", id.0),
            );
            self.ops[index].state = OpRunState::InFlight;
            self.trace.ready(id);
            self.trace.started(id);
        }
        Ok(())
    }

    fn accept_worker_completion(
        &mut self,
        pool: &mut WorkerPool,
        completion: TaskCompletion,
    ) -> Result<()> {
        match self.buffer_worker_completion(completion) {
            Ok(WorkerCompletionDisposition::Continue) => Ok(()),
            Ok(WorkerCompletionDisposition::AbortInfrastructure) => Err(self
                .abort_after_worker_error(
                    Some(pool),
                    "local worker reported an infrastructure failure",
                    anyhow::anyhow!("local worker reported an infrastructure failure"),
                )),
            Err(error) => Err(self.abort_after_worker_error(
                Some(pool),
                "local worker returned an invalid completion",
                error,
            )),
        }
    }

    fn accept_worker_event(
        &mut self,
        evaluator: &mut dyn GraphEvaluationHost,
        pool: &mut WorkerPool,
        event: WorkerEvent,
    ) -> Result<()> {
        match event {
            WorkerEvent::Completion(completion) => {
                crate::process::lifecycle_trace(
                    "coordinator.completion_received",
                    format!("token={}", completion.token.0),
                );
                self.accept_worker_completion(pool, completion)
            }
            WorkerEvent::EvalRequest(request) => {
                self.handle_worker_eval_request(evaluator, request)
            }
        }
    }

    /// A malformed callback/completion is a scheduler-infrastructure fault.
    /// Drain every other started task before returning so a worker blocked on
    /// its callback reply cannot be stranded during pool destruction.
    fn accept_worker_event_or_abort(
        &mut self,
        evaluator: &mut dyn GraphEvaluationHost,
        pool: &mut WorkerPool,
        event: WorkerEvent,
    ) -> Result<()> {
        match self.accept_worker_event(evaluator, pool, event) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.abort_after_worker_error(
                Some(pool),
                "scheduler aborted while handling a local-worker event",
                error,
            )),
        }
    }

    fn handle_worker_eval_request(
        &mut self,
        evaluator: &mut dyn GraphEvaluationHost,
        request: TaskEvalRequest,
    ) -> Result<()> {
        let index = request.token.0;
        crate::process::lifecycle_trace("coordinator.callback_received", format!("token={index}"));
        let valid = self.ops.get(index).is_some_and(|op| {
            op.state == OpRunState::InFlight
                && op.dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1
        });
        if !valid {
            request.respond(Err(TaskCallbackFailure::Infrastructure(format!(
                "local worker requested O.eval for invalid task token {index}"
            ))))?;
            bail!("local worker requested O.eval for invalid task token {index}");
        }

        let policy = self.frame.node_policy[self.ops[index].plan_node.0];
        let saved = evaluator.set_policy(policy);
        let outcome = match evaluator.eval_source_with_scope_until(
            &request.src,
            &request.scope,
            request.deadline,
        ) {
            Ok(value) => Ok(value),
            Err(error) if crate::process::is_infrastructure_error(&error) => {
                Err(TaskCallbackFailure::Infrastructure(format!("{error:#}")))
            }
            Err(error) => Err(TaskCallbackFailure::Semantic(format!("{error:#}"))),
        };
        evaluator.set_policy(saved);
        let succeeded = outcome.is_ok();
        request.respond(outcome)?;
        crate::process::lifecycle_trace(
            "coordinator.callback_replied",
            format!("token={index} success={succeeded}"),
        );
        Ok(())
    }

    fn buffer_worker_completion(
        &mut self,
        completion: TaskCompletion,
    ) -> Result<WorkerCompletionDisposition> {
        let index = completion.token.0;
        let op = self
            .ops
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("local worker returned an unknown task token"))?;
        if op.state != OpRunState::InFlight {
            bail!(
                "local worker returned task token {} in invalid state {:?}",
                index,
                op.state
            );
        }

        let outcome = if self.ops[index].failure_class == FailureClassV1::Infallible {
            match completion.outcome {
                TaskOutcome::Completed(Ok(value)) => {
                    if !self.ops[index].effect.is_verified_pure_infallible() {
                        bail!(
                            "operation {} has an incoherent infallible worker contract",
                            self.ops[index].plan_node.0
                        );
                    }
                    let id = self.ops[index].plan_node;
                    let publication = WorkerPublication {
                        output_type: value.type_name().to_string(),
                        fingerprint: trace_fingerprint(&value),
                    };
                    self.frame.set_value(id, *value)?;
                    self.publish_outputs(index);
                    self.ops[index].state = OpRunState::Published;
                    if self
                        .worker_publications
                        .insert(index, publication)
                        .is_some()
                    {
                        bail!("local worker published task token {index} twice");
                    }
                    return Ok(WorkerCompletionDisposition::Continue);
                }
                outcome => outcome,
            }
        } else {
            completion.outcome
        };

        let disposition = if matches!(&outcome, TaskOutcome::InfrastructureAbort(_))
            || (self.ops[index].failure_class == FailureClassV1::Infallible
                && matches!(&outcome, TaskOutcome::Completed(Err(_))))
        {
            WorkerCompletionDisposition::AbortInfrastructure
        } else {
            WorkerCompletionDisposition::Continue
        };

        self.ops[index].state = OpRunState::Buffered;
        if self.worker_results.insert(index, outcome).is_some() {
            bail!("local worker returned task token {index} twice");
        }
        crate::process::lifecycle_trace(
            "coordinator.result_buffered",
            format!("token={index} plan_node={}", self.ops[index].plan_node.0),
        );
        Ok(disposition)
    }

    /// Settle physical completions only at the deterministic semantic frontier.
    /// This preserves trace and failure order while still letting each accepted
    /// completion expose a fresh dispatch frontier.
    fn settle_buffered_results(&mut self) -> Option<WorkerFailure> {
        loop {
            let index = self.lowest_unsettled()?;
            if self.ops[index].state == OpRunState::Published {
                let id = self.ops[index].plan_node;
                let publication = self
                    .worker_publications
                    .remove(&index)
                    .expect("published operation has one trace publication");
                self.trace
                    .finished(id, publication.output_type, publication.fingerprint);
                self.ops[index].state = OpRunState::Settled;
                crate::process::lifecycle_trace(
                    "coordinator.result_settled",
                    format!("token={index} plan_node={} outcome=success", id.0),
                );
                continue;
            }
            if self.ops[index].state != OpRunState::Buffered {
                return None;
            }
            let id = self.ops[index].plan_node;
            let outcome = self
                .worker_results
                .remove(&index)
                .expect("buffered operation has one worker result");
            match outcome {
                TaskOutcome::Completed(Ok(value)) => {
                    let output_type = value.type_name().to_string();
                    let fingerprint = trace_fingerprint(&value);
                    if let Err(error) = self.frame.set_value(id, *value) {
                        crate::process::lifecycle_trace(
                            "coordinator.result_settled",
                            format!(
                                "token={index} plan_node={} outcome=infrastructure_failure",
                                id.0
                            ),
                        );
                        return Some(WorkerFailure {
                            index,
                            error,
                            kind: WorkerFailureKind::Infrastructure,
                        });
                    }
                    self.trace.finished(id, output_type, fingerprint);
                    self.materialize_success(index);
                    crate::process::lifecycle_trace(
                        "coordinator.result_settled",
                        format!("token={index} plan_node={} outcome=success", id.0),
                    );
                }
                TaskOutcome::Completed(Err(error)) => {
                    if self.ops[index].failure_class == FailureClassV1::Infallible {
                        crate::process::lifecycle_trace(
                            "coordinator.result_settled",
                            format!(
                                "token={index} plan_node={} outcome=infrastructure_failure",
                                id.0
                            ),
                        );
                        return Some(WorkerFailure {
                            index,
                            error: error.context(format!(
                                "admitted infallible operation {} returned an error",
                                id.0
                            )),
                            kind: WorkerFailureKind::Infrastructure,
                        });
                    }
                    self.trace.failed(id, error.to_string());
                    self.record_failure(index, &error.to_string());
                    crate::process::lifecycle_trace(
                        "coordinator.result_settled",
                        format!("token={index} plan_node={} outcome=semantic_failure", id.0),
                    );
                    return Some(WorkerFailure {
                        index,
                        error,
                        kind: WorkerFailureKind::Semantic,
                    });
                }
                TaskOutcome::InfrastructureAbort(error) => {
                    crate::process::lifecycle_trace(
                        "coordinator.result_settled",
                        format!(
                            "token={index} plan_node={} outcome=infrastructure_failure",
                            id.0
                        ),
                    );
                    return Some(WorkerFailure {
                        index,
                        error,
                        kind: WorkerFailureKind::Infrastructure,
                    });
                }
            }
        }
    }

    /// Drain every physically started task and give it one deterministic
    /// terminal trace event before returning from an abort or semantic failure.
    fn discard_started_workers(&mut self, pool: Option<&mut WorkerPool>, reason: &str) {
        if let Some(pool) = pool {
            while pool.outstanding() > 0 {
                match pool.recv_event() {
                    Ok(WorkerEvent::Completion(completion)) => {
                        let _ = self.buffer_worker_completion(completion);
                    }
                    Ok(WorkerEvent::EvalRequest(request)) => {
                        let _ = request
                            .respond(Err(TaskCallbackFailure::Infrastructure(reason.to_string())));
                    }
                    Err(_) => break,
                }
            }
        }

        let mut started = (0..self.ops.len())
            .filter(|&index| {
                matches!(
                    self.ops[index].state,
                    OpRunState::InFlight | OpRunState::Buffered | OpRunState::Published
                )
            })
            .collect::<Vec<_>>();
        started.sort_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0));
        for index in started {
            self.worker_results.remove(&index);
            self.worker_publications.remove(&index);
            for output in &self.ops[index].outputs {
                self.materialized.remove(output);
            }
            self.frame.values[self.ops[index].plan_node.0] = None;
            self.trace
                .discarded(self.ops[index].plan_node, reason.to_string());
            self.ops[index].state = OpRunState::Settled;
            crate::process::lifecycle_trace(
                "coordinator.result_discarded",
                format!(
                    "token={index} plan_node={} reason={}",
                    self.ops[index].plan_node.0, reason
                ),
            );
        }
    }

    /// A later preparation or pool fault must not preempt an earlier semantic
    /// worker failure. Drain physically-started work, settle the deterministic
    /// prefix, and prefer an error discovered there over the scheduler fault.
    fn abort_after_worker_error(
        &mut self,
        pool: Option<&mut WorkerPool>,
        reason: &str,
        scheduler_error: anyhow::Error,
    ) -> anyhow::Error {
        let mut drain_error = None;
        if let Some(pool) = pool {
            while pool.outstanding() > 0 {
                match pool.recv_event() {
                    Ok(WorkerEvent::Completion(completion)) => {
                        if let Err(error) = self.buffer_worker_completion(completion) {
                            if drain_error.is_none() {
                                drain_error = Some(error);
                            }
                        }
                    }
                    Ok(WorkerEvent::EvalRequest(request)) => {
                        if let Err(error) = request
                            .respond(Err(TaskCallbackFailure::Infrastructure(reason.to_string())))
                        {
                            if drain_error.is_none() {
                                drain_error = Some(error);
                            }
                        }
                    }
                    Err(error) => {
                        drain_error = Some(error);
                        break;
                    }
                }
            }
        }

        let selected_error = self
            .settle_buffered_results()
            .map(|failure| failure.error)
            .unwrap_or_else(|| drain_error.unwrap_or(scheduler_error));
        self.discard_started_workers(None, reason);
        selected_error
    }

    /// Execute one operation on the coordinator thread, under its derived
    /// policy context, and commit its value into the frame.
    fn run_coordinator_op(
        &mut self,
        evaluator: &mut dyn GraphEvaluationHost,
        index: usize,
    ) -> Result<()> {
        let id = self.ops[index].plan_node;
        let launches_backend = matches!(
            self.flat[id.0],
            OIr::Exec { backend, .. }
                if backend.execution == crate::backend_catalog::ExecutionMode::Shim
        );
        if self.ops[index].effect.unknown || launches_backend {
            // Recheck mutable shim/environment context plus retained executable
            // identity immediately before opaque/deferred work or a real shim
            // launch. The admitted direct command is not re-resolved or
            // re-hashed here. Inline renderers execute in the already-bound
            // current process and consume no launch artifact at this boundary.
            evaluator.verify_admitted_runtime_context(&self.admitted)?;
            if let OIr::Exec { backend, .. } = self.flat[id.0] {
                if backend.execution == crate::backend_catalog::ExecutionMode::Shim {
                    self.admitted
                        .executable_leases()?
                        .verify_backend(&backend.canonical)?;
                }
            }
        }
        self.trace.ready(id);
        self.trace.started(id);

        let policy = self.frame.node_policy[id.0];
        let saved = evaluator.set_policy(policy);
        let outcome =
            evaluator.execute_ready_plan_node(id, self.flat[id.0], self.plan, &mut self.frame);
        evaluator.set_policy(saved);

        match outcome {
            Ok(value) => {
                self.trace
                    .finished(id, value.type_name().to_string(), trace_fingerprint(&value));
                self.frame.set_value(id, value)?;
                self.materialize_success(index);
                Ok(())
            }
            Err(err) => {
                self.trace.failed(id, err.to_string());
                self.record_failure(index, &err.to_string());
                Err(err)
            }
        }
    }

    /// Successful execution produces the ordinary value, completion token, and
    /// every successor resource/control version atomically from the scheduler's
    /// perspective. Effects have already happened by this point; deterministic
    /// commit order is not used as a substitute for their graph ordering.
    fn materialize_success(&mut self, index: usize) {
        self.publish_outputs(index);
        self.ops[index].state = OpRunState::Settled;
    }

    fn publish_outputs(&mut self, index: usize) {
        debug_assert!(self.ops[index]
            .outputs
            .contains(&self.ops[index].value_output));
        for output in self.ops[index].outputs.clone() {
            self.materialized.insert(output);
        }
    }

    fn record_failure(&mut self, index: usize, message: &str) {
        for output in self.ops[index].outputs.clone() {
            self.materialized.remove(&output);
            self.failed_outputs.insert(output, message.to_string());
        }
        self.ops[index].state = OpRunState::Settled;
    }

    /// Commit root values into `scope` in deterministic root order, returning
    /// the document value (the last non-null, non-whitespace root).
    fn commit(&self, scope: &mut std::collections::HashMap<String, OValue>) -> Result<OValue> {
        let mut last = OValue::null();
        for root_index in self.plan.root_schedule().map_err(anyhow::Error::msg)? {
            let node = &self.program.nodes[root_index];
            let node_id = self.plan.roots[root_index];
            let is_pure_whitespace_text = matches!(
                node,
                OIr::Text(text) if !text.is_empty() && text.chars().all(char::is_whitespace)
            );
            let value = self.frame.value(node_id)?.clone();
            if let OIr::Store { name, .. } = node {
                scope.insert(name.clone(), value.clone());
            }
            if !value.is_null() && !is_pure_whitespace_text {
                last = value;
            }
        }
        Ok(last)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use crate::backend_catalog::BackendRegistry;
    use crate::eval::Evaluator;
    use crate::evidence::{admit_execution, analyze_execution};
    use crate::executor::task::PreparedTask;
    use crate::hgraph::from_oir::build_program;
    use crate::hgraph::solve::solve_types;
    use crate::hgraph::HNodeKind;

    struct PanicPreparedTask;

    impl PreparedTask for PanicPreparedTask {
        fn execute(
            self: Box<Self>,
            _context: &crate::executor::task::TaskContext,
        ) -> Result<OValue> {
            panic!("coordinator infrastructure test panic")
        }
    }

    struct ErrorPreparedTask;

    impl PreparedTask for ErrorPreparedTask {
        fn execute(
            self: Box<Self>,
            _context: &crate::executor::task::TaskContext,
        ) -> Result<OValue> {
            bail!("infallible adapter contract violated")
        }
    }

    struct CallbackTask {
        delay: Duration,
    }

    impl PreparedTask for CallbackTask {
        fn execute(
            self: Box<Self>,
            context: &crate::executor::task::TaskContext,
        ) -> Result<OValue> {
            std::thread::sleep(self.delay);
            context.eval_o_source("text^(callback)_text".to_string(), HashMap::new())
        }
    }

    #[test]
    fn coordinator_rejects_hgraph_from_a_different_plan_or_program() {
        let graph_program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for("html"),
                body: vec![OIr::Text("pure".into())],
            }],
        };
        let mut graph = build_program(&graph_program);
        solve_types(&mut graph).unwrap();
        let different_plan_program = OIrProgram {
            nodes: vec![OIr::Store {
                name: "x".into(),
                expr: Box::new(OIr::Text("effect classification differs".into())),
            }],
        };
        let different_plan = different_plan_program.plan();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&different_plan);
        let evidence_runtime = evaluator.admission_runtime_binding(&graph_program.plan());
        let evidence = analyze_execution(
            &graph_program,
            &graph_program.plan(),
            &graph,
            evidence_runtime,
        )
        .unwrap();
        let error = admit_execution(
            &different_plan_program,
            &different_plan,
            graph,
            Policy::Eager,
            runtime,
            evidence,
        )
        .err()
        .expect("a graph cannot schedule unrelated OIR");
        assert!(
            format!("{error:#}").contains("does not match the HGraph source plan"),
            "{error:#}"
        );

        // Text content is absent from PlanNodeKind, so exact OIR provenance is
        // checked independently even when the two ExecutionPlans compare equal.
        let source = OIrProgram {
            nodes: vec![OIr::Text("source".into())],
        };
        let mut source_graph = build_program(&source);
        solve_types(&mut source_graph).unwrap();
        let different_text = OIrProgram {
            nodes: vec![OIr::Text("different".into())],
        };
        let same_shape_plan = different_text.plan();
        let runtime = evaluator.admission_runtime_binding(&same_shape_plan);
        let evidence = analyze_execution(
            &source,
            &source.plan(),
            &source_graph,
            evaluator.admission_runtime_binding(&source.plan()),
        )
        .unwrap();
        let error = admit_execution(
            &different_text,
            &same_shape_plan,
            source_graph,
            Policy::Eager,
            runtime,
            evidence,
        )
        .err()
        .expect("same-shaped OIR must still match graph provenance");
        assert!(
            format!("{error:#}").contains("does not match HGraph source provenance"),
            "{error:#}"
        );
    }

    #[test]
    fn failed_operation_produces_no_value_completion_or_resource_state() {
        let program = OIrProgram {
            nodes: vec![OIr::Load("missing".into())],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let load_id = plan.roots[0];
        let outputs = graph
            .op_for(load_id)
            .expect("load must lower to an operation")
            .outputs
            .clone();

        assert!(outputs.iter().any(|output| matches!(
            graph.node(*output).map(|node| &node.kind),
            Some(HNodeKind::Value)
        )));
        assert!(outputs.iter().any(|output| matches!(
            graph.node(*output).map(|node| &node.kind),
            Some(HNodeKind::Completion { plan_node }) if *plan_node == load_id
        )));
        assert!(outputs.iter().all(|output| !matches!(
            graph.node(*output).map(|node| &node.kind),
            Some(HNodeKind::ResourceState { .. })
        )));

        let mut evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let coordinator = Coordinator::new(admitted).expect("valid admitted graph");
        assert!(outputs
            .iter()
            .all(|output| !coordinator.materialized.contains(output)));

        let mut scope = HashMap::new();
        let error = coordinator
            .run(&mut evaluator, &mut scope)
            .expect_err("undefined load must fail");
        assert!(error.to_string().contains("Undefined variable: $missing"));

        assert!(scope.is_empty(), "a failed operation must not commit scope");
        let trace = evaluator
            .last_execution_trace()
            .expect("failed one-shot execution retains its trace");
        assert!(trace.events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeFailed { id, .. } if *id == load_id
        )));
        assert!(!trace.events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeFinished { id, .. } if *id == load_id
        )));
    }

    #[test]
    fn later_scheduler_fault_cannot_preempt_an_earlier_worker_failure() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Load("missing_first".into()),
                OIr::Load("missing_second".into()),
            ],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        let mut pool = WorkerPool::new(1).unwrap();
        let first = 0;
        let id = coordinator.ops[first].plan_node;
        let task = parallel::prepare(
            coordinator.ops[first].dispatch_adapter,
            &coordinator.frame,
            &plan,
            id,
            coordinator.flat[id.0],
            None,
        )
        .unwrap();
        pool.submit(TaskSubmission::new(TaskToken(first), task))
            .unwrap();
        coordinator.ops[first].state = OpRunState::InFlight;

        let error = coordinator.abort_after_worker_error(
            Some(&mut pool),
            "later scheduler fault",
            anyhow::anyhow!("later pool submission failed"),
        );

        assert!(error.to_string().contains("missing_first"), "{error:#}");
        assert!(
            !error.to_string().contains("later pool submission failed"),
            "{error:#}"
        );
    }

    #[test]
    fn malformed_callback_event_drains_other_callback_waiters() {
        let python = |value| OIr::Exec {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            backend: BackendRegistry::global().interface_for("python"),
            body: vec![OIr::Text(format!("__oval_result__ = {value}"))],
        };
        let program = OIrProgram {
            nodes: vec![OIr::Invoke {
                fn_name: "autonomous".into(),
                mode: crate::ir::InvokeMode::Autonomous,
                args: vec![OIr::Invoke {
                    fn_name: "batch".into(),
                    mode: crate::ir::InvokeMode::Group(crate::value::GroupMode::Batch),
                    args: vec![python(1), python(2)],
                }],
            }],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let mut evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        let valid = coordinator
            .ops
            .iter()
            .position(|op| op.dispatch_adapter == DispatchAdapterV1::AutonomousEphemeralShimV1)
            .expect("autonomous hosted operation");
        coordinator.ops[valid].state = OpRunState::InFlight;
        coordinator.trace.started(coordinator.ops[valid].plan_node);

        let mut pool = WorkerPool::new(2).unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(usize::MAX),
            Box::new(CallbackTask {
                delay: Duration::ZERO,
            }),
        ))
        .unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(valid),
            Box::new(CallbackTask {
                delay: Duration::from_millis(50),
            }),
        ))
        .unwrap();

        let event = pool.recv_event().unwrap();
        let error = coordinator
            .accept_worker_event_or_abort(&mut evaluator, &mut pool, event)
            .expect_err("an invalid callback token must abort the worker lane");

        assert_eq!(pool.outstanding(), 0, "every started worker was drained");
        assert_eq!(coordinator.ops[valid].state, OpRunState::Settled);
        assert!(
            error
                .to_string()
                .contains("scheduler aborted while handling a local-worker event"),
            "{error:#}"
        );
    }

    #[test]
    fn later_infrastructure_completion_stops_dispatch_without_preempting_earlier_failure() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Load("missing_first".into()),
                OIr::Load("missing_second".into()),
            ],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        let mut pool = WorkerPool::new(1).unwrap();
        let first = 0;
        let second = 1;
        let first_id = coordinator.ops[first].plan_node;
        let task = parallel::prepare(
            coordinator.ops[first].dispatch_adapter,
            &coordinator.frame,
            &plan,
            first_id,
            coordinator.flat[first_id.0],
            None,
        )
        .unwrap();
        pool.submit(TaskSubmission::new(TaskToken(first), task))
            .unwrap();
        coordinator.ops[first].state = OpRunState::InFlight;
        coordinator.ops[second].state = OpRunState::InFlight;

        let error = coordinator
            .accept_worker_completion(
                &mut pool,
                TaskCompletion::infrastructure_abort(
                    TaskToken(second),
                    anyhow::anyhow!("later worker panicked"),
                ),
            )
            .expect_err("infrastructure completion must immediately enter the abort path");

        assert!(error.to_string().contains("missing_first"), "{error:#}");
        assert!(
            !error.to_string().contains("later worker panicked"),
            "{error:#}"
        );
        assert_eq!(coordinator.ops[second].state, OpRunState::Settled);
    }

    #[test]
    fn caught_worker_panic_is_discarded_as_infrastructure_not_node_failure() {
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "text".into(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for("text"),
                body: vec![OIr::Text("infallible".into())],
            }],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        let id = coordinator.ops[0].plan_node;
        let mut pool = WorkerPool::new(1).unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(0),
            Box::new(PanicPreparedTask),
        ))
        .unwrap();
        coordinator.ops[0].state = OpRunState::InFlight;
        coordinator.trace.started(id);
        let completion = pool.recv_completion().unwrap();
        let error = coordinator
            .accept_worker_completion(&mut pool, completion)
            .expect_err("caught panic must immediately enter infrastructure abort");
        assert!(error.to_string().contains("panicked"), "{error:#}");

        let trace = std::mem::take(&mut coordinator.trace).into_trace();
        let events = &trace.events;
        assert!(events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeDiscarded { id: event_id, .. } if *event_id == id
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeFailed { id: event_id, .. } if *event_id == id
        )));
    }

    #[test]
    fn infallible_adapter_error_is_infrastructure_not_node_failure() {
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "text".into(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for("text"),
                body: vec![OIr::Text("infallible".into())],
            }],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        let id = coordinator.ops[0].plan_node;
        let mut pool = WorkerPool::new(1).unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(0),
            Box::new(ErrorPreparedTask),
        ))
        .unwrap();
        coordinator.ops[0].state = OpRunState::InFlight;
        coordinator.trace.started(id);
        let completion = pool.recv_completion().unwrap();
        let error = coordinator
            .accept_worker_completion(&mut pool, completion)
            .expect_err("an admitted-infallible error must immediately abort infrastructure");
        assert!(
            format!("{error:#}").contains("admitted infallible operation"),
            "{error:#}"
        );

        let trace = std::mem::take(&mut coordinator.trace).into_trace();
        assert!(trace.events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeDiscarded { id: event_id, .. } if *event_id == id
        )));
        assert!(!trace.events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeFailed { id: event_id, .. } if *event_id == id
        )));
    }

    #[test]
    fn provisional_infallible_publication_is_revoked_after_earlier_failure() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Load("missing".into()),
                OIr::Exec {
                    lang: "text".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: BackendRegistry::global().interface_for("text"),
                    body: vec![OIr::Text("speculative".into())],
                },
            ],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        let earlier = coordinator
            .ops
            .iter()
            .position(|op| op.plan_node == plan.roots[0])
            .unwrap();
        let later = coordinator
            .ops
            .iter()
            .position(|op| op.plan_node == plan.roots[1])
            .unwrap();
        let later_id = coordinator.ops[later].plan_node;
        let later_outputs = coordinator.ops[later].outputs.clone();

        coordinator.ops[later].state = OpRunState::InFlight;
        coordinator.trace.started(later_id);
        let disposition = coordinator
            .buffer_worker_completion(TaskCompletion::completed(
                TaskToken(later),
                Ok(OValue::str_("speculative")),
            ))
            .unwrap();
        assert_eq!(disposition, WorkerCompletionDisposition::Continue);
        assert_eq!(coordinator.ops[later].state, OpRunState::Published);
        assert!(later_outputs
            .iter()
            .all(|output| coordinator.materialized.contains(output)));
        assert!(coordinator.frame.values[later_id.0].is_some());

        coordinator.ops[earlier].state = OpRunState::Buffered;
        coordinator.worker_results.insert(
            earlier,
            TaskOutcome::Completed(Err(anyhow::anyhow!("earlier failure"))),
        );
        let failure = coordinator
            .settle_buffered_results()
            .expect("the earlier fallible result must select failure");
        assert_eq!(failure.kind, WorkerFailureKind::Semantic);
        coordinator.discard_started_workers(None, "earlier operation failed");

        assert!(later_outputs
            .iter()
            .all(|output| !coordinator.materialized.contains(output)));
        assert!(coordinator.frame.values[later_id.0].is_none());
        let trace = std::mem::take(&mut coordinator.trace).into_trace();
        assert!(trace.events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeDiscarded { id, .. } if *id == later_id
        )));
        assert!(!trace.events.iter().any(|event| matches!(
            event,
            crate::eval::TraceEvent::NodeFinished { id, .. } if *id == later_id
        )));
    }

    #[test]
    fn reverse_fallible_completion_order_still_selects_lowest_ordinal_failure() {
        let program = OIrProgram {
            nodes: vec![OIr::Load("first".into()), OIr::Load("second".into())],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        for index in [1, 0] {
            coordinator.ops[index].state = OpRunState::Buffered;
            coordinator.worker_results.insert(
                index,
                TaskOutcome::Completed(Err(anyhow::anyhow!(if index == 0 {
                    "first failure"
                } else {
                    "second failure"
                }))),
            );
        }

        let failure = coordinator
            .settle_buffered_results()
            .expect("the lowest semantic failure must be selected");

        assert_eq!(failure.kind, WorkerFailureKind::Semantic);
        assert_eq!(failure.index, 0);
        assert_eq!(failure.error.to_string(), "first failure");
        assert_eq!(coordinator.ops[1].state, OpRunState::Buffered);
    }

    #[test]
    fn strict_fallible_frontier_gets_pool_capacity_before_infallible_speculation() {
        let text = BackendRegistry::global().interface_for("text");
        let renderer = |body: &str| OIr::Exec {
            lang: "text".into(),
            env_id: u32::MAX,
            attr: None,
            backend: text.clone(),
            body: vec![OIr::Text(body.into())],
        };
        let program = OIrProgram {
            nodes: vec![
                OIr::Load("first".into()),
                OIr::Load("second".into()),
                renderer("speculative-one"),
                renderer("speculative-two"),
            ],
        };
        let plan = program.plan();
        let mut graph = build_program(&program);
        solve_types(&mut graph).unwrap();
        let evaluator = Evaluator::new("/tmp".into());
        let runtime = evaluator.admission_runtime_binding(&plan);
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let mut coordinator = Coordinator::new(admitted).unwrap();
        coordinator.materialize_literals().unwrap();
        let ready = coordinator.ready_ops();

        let selected = coordinator.worker_dispatch_candidates(&ready, 2);
        let selected_nodes = selected
            .iter()
            .map(|index| coordinator.ops[*index].plan_node)
            .collect::<Vec<_>>();

        assert_eq!(selected_nodes, plan.roots[..2]);
        assert!(selected.iter().all(|index| {
            coordinator.ops[*index].failure_class == FailureClassV1::MayFailNoExternalEffects
        }));
    }
}
