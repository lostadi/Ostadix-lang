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
use crate::eval::{derive_policy_contexts, Evaluator, ExecutionTrace, GraphEvalFrame, Policy};
use crate::evidence::{AdmittedExecution, DispatchAdapterV1, DispatchLaneV1, FailureClassV1};
use crate::hgraph::{schedule::ReadySchedule, NodeId, ValueState};
use crate::ir::{ExecutionPlan, OIr, OIrProgram, PlanNodeId, PlanNodeKind};
use crate::value::OValue;

use super::parallel;
use super::pool::WorkerPool;
use super::task::{TaskCompletion, TaskOutcome, TaskSubmission, TaskToken};
use super::trace::TraceSink;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpRunState {
    Pending,
    InFlight,
    Buffered,
    Settled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerFailureKind {
    Semantic,
    Infrastructure,
}

struct WorkerFailure {
    index: usize,
    error: anyhow::Error,
    kind: WorkerFailureKind,
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
            frame,
            trace: TraceSink::new(),
            base_policy,
        })
    }

    /// Drive the plan to completion, committing store deltas and root results
    /// into `scope` in deterministic root order. Returns the last non-null,
    /// non-whitespace root value (the document value).
    pub fn run(
        mut self,
        evaluator: &mut Evaluator,
        scope: &mut std::collections::HashMap<String, OValue>,
    ) -> Result<OValue> {
        let current_runtime = evaluator.admission_runtime_binding(self.plan);
        self.admitted.verify_runtime(&current_runtime)?;
        evaluator.prevalidate_graph_execution(self.plan, &self.flat)?;
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
            self.trace.finished(
                id,
                value.type_name().to_string(),
                Evaluator::trace_fingerprint(&value),
            );
            self.frame.set_value(id, value)?;
        }
        Ok(())
    }

    /// The readiness-driven event loop.
    fn drive(&mut self, evaluator: &mut Evaluator) -> Result<()> {
        let worker_operations = self
            .ops
            .iter()
            .filter(|op| op.dispatch_lane == DispatchLaneV1::LocalWorker)
            .count();
        let worker_capacity = evaluator
            .local_worker_parallelism()
            .max(1)
            .min(worker_operations.max(1));
        #[cfg(test)]
        let worker_capacity = parallel::worker_count_hint()
            .unwrap_or(worker_capacity)
            .max(1)
            .min(worker_operations.max(1));
        let mut pool = (worker_operations > 0)
            .then(|| WorkerPool::new(worker_capacity))
            .transpose()?;

        loop {
            if let Some(pool) = pool.as_mut() {
                loop {
                    let completion = match pool.try_recv_completion() {
                        Ok(Some(completion)) => completion,
                        Ok(None) => break,
                        Err(error) => {
                            return Err(self.abort_after_worker_error(
                                Some(pool),
                                "local worker completion channel failed",
                                error,
                            ));
                        }
                    };
                    if let Err(error) = self.buffer_worker_completion(completion) {
                        return Err(self.abort_after_worker_error(
                            Some(pool),
                            "local worker returned an invalid completion",
                            error,
                        ));
                    }
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
                    if let Err(error) = self.dispatch_workers(worker_pool, &candidates) {
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
                let completion = match worker_pool.recv_completion() {
                    Ok(completion) => completion,
                    Err(error) => {
                        return Err(self.abort_after_worker_error(
                            Some(worker_pool),
                            "local worker completion channel failed",
                            error,
                        ));
                    }
                };
                if let Err(error) = self.buffer_worker_completion(completion) {
                    return Err(self.abort_after_worker_error(
                        Some(worker_pool),
                        "local worker returned an invalid completion",
                        error,
                    ));
                }
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
        parallel::adapter_matches(self.ops[index].dispatch_adapter, self.flat[id.0])
            && parallel::render_inputs_pure(&self.frame, self.plan, id)
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

    fn dispatch_workers(&mut self, pool: &mut WorkerPool, selected: &[usize]) -> Result<()> {
        let mut prepared = Vec::with_capacity(selected.len());
        for &index in selected {
            let id = self.ops[index].plan_node;
            let task = parallel::prepare(
                self.ops[index].dispatch_adapter,
                &self.frame,
                self.plan,
                id,
                self.flat[id.0],
            )?;
            prepared.push((index, id, task));
        }

        for (index, id, task) in prepared {
            pool.submit(TaskSubmission::new(TaskToken(index), task))?;
            self.ops[index].state = OpRunState::InFlight;
            self.trace.ready(id);
            self.trace.started(id);
        }
        Ok(())
    }

    fn buffer_worker_completion(&mut self, completion: TaskCompletion) -> Result<()> {
        let index = completion.token.0;
        let op = self
            .ops
            .get_mut(index)
            .ok_or_else(|| anyhow::anyhow!("local worker returned an unknown task token"))?;
        if op.state != OpRunState::InFlight {
            bail!(
                "local worker returned task token {} in invalid state {:?}",
                index,
                op.state
            );
        }
        op.state = OpRunState::Buffered;
        if self
            .worker_results
            .insert(index, completion.outcome)
            .is_some()
        {
            bail!("local worker returned task token {index} twice");
        }
        Ok(())
    }

    /// Settle physical completions only at the deterministic semantic frontier.
    /// This preserves trace and failure order while still letting each accepted
    /// completion expose a fresh dispatch frontier.
    fn settle_buffered_results(&mut self) -> Option<WorkerFailure> {
        loop {
            let Some(index) = self.lowest_unsettled() else {
                return None;
            };
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
                    let fingerprint = Evaluator::trace_fingerprint(&value);
                    if let Err(error) = self.frame.set_value(id, value) {
                        return Some(WorkerFailure {
                            index,
                            error,
                            kind: WorkerFailureKind::Infrastructure,
                        });
                    }
                    self.trace.finished(id, output_type, fingerprint);
                    self.materialize_success(index);
                }
                TaskOutcome::Completed(Err(error)) => {
                    self.trace.failed(id, error.to_string());
                    self.record_failure(index, &error.to_string());
                    return Some(WorkerFailure {
                        index,
                        error,
                        kind: WorkerFailureKind::Semantic,
                    });
                }
                TaskOutcome::InfrastructureAbort(error) => {
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
                match pool.recv_completion() {
                    Ok(completion) => {
                        let _ = self.buffer_worker_completion(completion);
                    }
                    Err(_) => break,
                }
            }
        }

        let mut started = (0..self.ops.len())
            .filter(|&index| {
                matches!(
                    self.ops[index].state,
                    OpRunState::InFlight | OpRunState::Buffered
                )
            })
            .collect::<Vec<_>>();
        started.sort_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0));
        for index in started {
            self.worker_results.remove(&index);
            self.trace
                .discarded(self.ops[index].plan_node, reason.to_string());
            self.ops[index].state = OpRunState::Settled;
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
                match pool.recv_completion() {
                    Ok(completion) => {
                        if let Err(error) = self.buffer_worker_completion(completion) {
                            drain_error = Some(error);
                            break;
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
    fn run_coordinator_op(&mut self, evaluator: &mut Evaluator, index: usize) -> Result<()> {
        let id = self.ops[index].plan_node;
        if self.ops[index].effect.unknown
            || matches!(self.plan.nodes[id.0].kind, PlanNodeKind::Exec { .. })
        {
            // Re-resolve backend artifacts and the environment immediately
            // before opaque/deferred work. A stale adapter must fail before
            // this operation emits Ready or Started, not merely at admission.
            let current_runtime = evaluator.admission_runtime_binding(self.plan);
            self.admitted.verify_runtime(&current_runtime)?;
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
                self.trace.finished(
                    id,
                    value.type_name().to_string(),
                    Evaluator::trace_fingerprint(&value),
                );
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
        debug_assert!(self.ops[index]
            .outputs
            .contains(&self.ops[index].value_output));
        for output in self.ops[index].outputs.clone() {
            self.materialized.insert(output);
        }
        self.ops[index].state = OpRunState::Settled;
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

    use super::*;
    use crate::evidence::{admit_execution, analyze_execution};
    use crate::executor::task::PreparedTask;
    use crate::hgraph::from_oir::build_program;
    use crate::hgraph::solve::solve_types;
    use crate::hgraph::HNodeKind;
    use crate::ir::BackendRegistry;

    struct PanicPreparedTask;

    impl PreparedTask for PanicPreparedTask {
        fn execute(self: Box<Self>) -> Result<OValue> {
            panic!("coordinator infrastructure test panic")
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
        coordinator.buffer_worker_completion(completion).unwrap();

        let failure = coordinator
            .settle_buffered_results()
            .expect("caught panic must become an infrastructure abort");
        assert_eq!(failure.kind, WorkerFailureKind::Infrastructure);
        assert!(failure.error.to_string().contains("panicked"));
        coordinator.discard_started_workers(None, "worker infrastructure abort");

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
