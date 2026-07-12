//! The graph-execution coordinator.
//!
//! The coordinator owns the mutable execution state for one plan evaluation and
//! drives a readiness-based event loop over the plan's operation hyperedges.
//! An operation becomes ready once all of its blocking predecessors — data and
//! structural producers plus its same-actor serial predecessor — have
//! committed. Independent siblings therefore become ready together instead of
//! being forced into wall-clock serialization by blanket sibling sequencing.
//!
//! Operations that are provably pure and side-effect free (literal text and
//! attribute-free pure inline renderers) are executed on a worker-thread pool
//! via `std::thread::scope`; every other operation — anything that needs the
//! evaluator's `!Send` process registry or mutable state — runs on the
//! coordinator thread in stable ordinal order. Results are committed in the
//! plan's deterministic root order, so out-of-order completion never changes
//! observable output.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::eval::{derive_policy_contexts, Evaluator, ExecutionTrace, GraphEvalFrame, Policy};
use crate::hgraph::{schedule::ReadySchedule, HGraph};
use crate::ir::{ExecutionPlan, OIr, OIrProgram, PlanNodeId, PlanNodeKind};
use crate::value::OValue;

use super::actor::ActorTable;
use super::effects::{EffectDeclaration, EffectSummary};
use super::parallel;
use super::trace::TraceSink;

/// One committed-or-pending operation the coordinator tracks.
struct OpState {
    plan_node: PlanNodeId,
    ordinal: u64,
    blocked_by: Vec<usize>,
    completed: bool,
}

pub struct Coordinator<'a> {
    program: &'a OIrProgram,
    plan: &'a ExecutionPlan,
    flat: Vec<&'a OIr>,
    ops: Vec<OpState>,
    effects: Vec<EffectSummary>,
    actors: ActorTable,
    frame: GraphEvalFrame,
    trace: TraceSink,
    base_policy: Policy,
}

impl<'a> Coordinator<'a> {
    /// Build a coordinator for `plan`/`program`, using `hgraph` for the
    /// ready-operation schedule. `base_policy` is the evaluator's active policy.
    pub fn new(
        program: &'a OIrProgram,
        plan: &'a ExecutionPlan,
        hgraph: &HGraph,
        base_policy: Policy,
        generation_of: impl Fn(&str, u32) -> u64,
    ) -> Result<Self> {
        let flat = program.flatten_for_plan();
        if flat.len() != plan.nodes.len() {
            bail!(
                "OIR flatten produced {} nodes but plan has {} nodes",
                flat.len(),
                plan.nodes.len()
            );
        }

        let schedule = ReadySchedule::derive(hgraph).map_err(anyhow::Error::msg)?;
        let ops = schedule
            .ops
            .iter()
            .map(|op| OpState {
                plan_node: op.plan_node,
                ordinal: op.ordinal,
                blocked_by: op.blocked_by.clone(),
                completed: false,
            })
            .collect::<Vec<_>>();

        let node_policy = derive_policy_contexts(plan, &flat, base_policy)?;
        let actors = ActorTable::build(plan, generation_of);
        let effects = plan
            .nodes
            .iter()
            .map(|node| effect_summary_for(node.id, &node.kind, &actors))
            .collect::<Vec<_>>();

        let frame = GraphEvalFrame {
            values: vec![None; plan.nodes.len()],
            base_scope: std::collections::HashMap::new(),
            node_policy,
            trace: ExecutionTrace::new(),
        };

        Ok(Self {
            program,
            plan,
            flat,
            ops,
            effects,
            actors,
            frame,
            trace: TraceSink::new(),
            base_policy,
        })
    }

    /// Drive the plan to completion, committing store deltas and root results
    /// into `scope` in deterministic root order. Returns the last non-null,
    /// non-whitespace root value (the document value).
    pub fn run(
        &mut self,
        evaluator: &mut Evaluator,
        scope: &mut std::collections::HashMap<String, OValue>,
    ) -> Result<OValue> {
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
        loop {
            let ready = self.ready_ops();
            if ready.is_empty() {
                if self.ops.iter().all(|op| op.completed) {
                    return Ok(());
                }
                bail!(
                    "graph executor stalled: {} of {} operations never became ready \
                     (dependency cycle or unsatisfiable constraint)",
                    self.ops.iter().filter(|op| !op.completed).count(),
                    self.ops.len()
                );
            }

            // Partition the ready frontier into a parallel-safe batch and the
            // operations that must run on the coordinator thread.
            let (parallel, sequential): (Vec<usize>, Vec<usize>) = ready
                .into_iter()
                .partition(|&index| self.is_parallel_safe(index));

            if !parallel.is_empty() {
                self.run_parallel_batch(&parallel)?;
            }

            for index in sequential {
                self.run_coordinator_op(evaluator, index)?;
            }
        }
    }

    /// Indices of operations whose blocking predecessors have all completed.
    fn ready_ops(&self) -> Vec<usize> {
        let mut ready: Vec<usize> = (0..self.ops.len())
            .filter(|&index| {
                let op = &self.ops[index];
                !op.completed && op.blocked_by.iter().all(|&dep| self.ops[dep].completed)
            })
            .collect();
        ready.sort_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0));
        ready
    }

    /// Whether an operation may run on a worker thread: it must be a pure,
    /// deterministic, side-effect-free renderer (literal-style) whose effect
    /// summary carries no writes and no unknown/host-global footprint.
    fn is_parallel_safe(&self, index: usize) -> bool {
        let id = self.ops[index].plan_node;
        let summary = &self.effects[id.0];
        if summary.unknown || !summary.deterministic || !summary.writes.is_empty() {
            return false;
        }
        parallel::classify(self.plan, self.flat[id.0], id).is_some()
            && parallel::render_inputs_pure(&self.frame, self.plan, id)
    }

    /// Execute one operation on the coordinator thread, under its derived
    /// policy context, and commit its value into the frame.
    fn run_coordinator_op(&mut self, evaluator: &mut Evaluator, index: usize) -> Result<()> {
        let id = self.ops[index].plan_node;
        self.trace.ready(id);
        self.trace.started(id);

        let policy = self.frame.node_policy[id.0];
        let saved = evaluator.set_policy(policy);
        let outcome = evaluator.execute_ready_plan_node(id, self.flat[id.0], self.plan, &mut self.frame);
        evaluator.set_policy(saved);

        match outcome {
            Ok(value) => {
                self.trace.finished(
                    id,
                    value.type_name().to_string(),
                    Evaluator::trace_fingerprint(&value),
                );
                self.frame.set_value(id, value)?;
                self.ops[index].completed = true;
                Ok(())
            }
            Err(err) => {
                self.trace.failed(id, err.to_string());
                Err(err)
            }
        }
    }

    /// Execute a batch of parallel-safe operations on worker threads, then
    /// commit their values into the frame in deterministic ordinal order. If
    /// any worker fails, the smallest-ordinal failure is selected.
    fn run_parallel_batch(&mut self, batch: &[usize]) -> Result<()> {
        // Build Send-only tasks on the coordinator thread from already
        // materialized inputs, then compute renders on worker threads.
        let mut tasks: Vec<(usize, PlanNodeId, parallel::ParallelTask)> = Vec::new();
        for &index in batch {
            let id = self.ops[index].plan_node;
            self.trace.ready(id);
            self.trace.started(id);
            let task = parallel::classify(self.plan, self.flat[id.0], id)
                .expect("parallel-safe op must classify");
            let built = parallel::build_task(&self.frame, self.plan, id, task)?;
            tasks.push((index, id, built));
        }

        let results = parallel::execute(tasks.iter().map(|(_, _, task)| task.clone()).collect());

        // Commit in ordinal order; select the smallest-ordinal failure.
        let mut completions: BTreeMap<(u64, usize), (usize, PlanNodeId, Result<OValue>)> =
            BTreeMap::new();
        for ((index, id, _), result) in tasks.into_iter().zip(results.into_iter()) {
            let ordinal = self.ops[index].ordinal;
            completions.insert((ordinal, id.0), (index, id, result));
        }

        for (_, (index, id, result)) in completions {
            match result {
                Ok(value) => {
                    self.trace.finished(
                        id,
                        value.type_name().to_string(),
                        Evaluator::trace_fingerprint(&value),
                    );
                    self.frame.set_value(id, value)?;
                    self.ops[index].completed = true;
                }
                Err(err) => {
                    self.trace.failed(id, err.to_string());
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    /// Commit root values into `scope` in deterministic root order, returning
    /// the document value (the last non-null, non-whitespace root).
    fn commit(
        &self,
        scope: &mut std::collections::HashMap<String, OValue>,
    ) -> Result<OValue> {
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

    /// Read-only view of the actor table (used by tests).
    pub fn actor_table(&self) -> &ActorTable {
        &self.actors
    }

    /// Read-only view of the derived per-node effect summaries (used by tests).
    pub fn effect_summaries(&self) -> &[EffectSummary] {
        &self.effects
    }
}

/// Derive the effect summary for a single plan node.
pub fn effect_summary_for(
    id: PlanNodeId,
    kind: &PlanNodeKind,
    actors: &ActorTable,
) -> EffectSummary {
    match kind {
        PlanNodeKind::Text
        | PlanNodeKind::Load { .. }
        | PlanNodeKind::Store { .. }
        | PlanNodeKind::Group { .. } => EffectSummary::pure(),
        // Builtins and schedule points may touch evaluator state; keep them on
        // the coordinator thread.
        PlanNodeKind::Call { .. }
        | PlanNodeKind::Request { .. }
        | PlanNodeKind::Schedule { .. } => EffectSummary::unknown(),
        PlanNodeKind::Exec {
            lang,
            env_id,
            attr,
            backend,
        } => {
            let declaration = EffectDeclaration::parse(attr.as_deref());
            // quote/O structural inline backends and pure inline value/thunk
            // backends are pure by default; shim backends are unknown/impure.
            let base = if backend.pure {
                EffectSummary::pure()
            } else {
                let mut summary = EffectSummary::unknown();
                if let Some(actor) = actors.actor_for(id) {
                    if !actor.is_ephemeral() {
                        summary = summary.with_actor_state(actor.clone());
                    }
                }
                summary
            };
            let _ = (lang, env_id);
            declaration.apply(base)
        }
    }
}
