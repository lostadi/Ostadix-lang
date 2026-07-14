//! The graph-execution coordinator.
//!
//! The coordinator owns the mutable execution state for one plan evaluation and
//! drives a readiness-based event loop over the plan's operation hyperedges.
//! An operation becomes ready exactly when all ordinary and synthetic input
//! nodes are materialized. Data, source completion, resource state, and actor
//! state therefore share one directed producer/input dependency rule.
//!
//! Operations that are provably pure and side-effect free (literal text and
//! attribute-free verified inline renderers) are executed on a worker-thread pool
//! via `std::thread::scope`; every other operation — anything that needs the
//! evaluator's `!Send` process registry or mutable state — runs on the
//! coordinator thread in stable ordinal order. Results are committed in the
//! plan's deterministic root order. State/control inputs, rather than commit
//! order, preserve externally observable effect ordering.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Result};

use crate::effects::EffectSummary;
use crate::eval::{derive_policy_contexts, Evaluator, ExecutionTrace, GraphEvalFrame, Policy};
use crate::hgraph::{schedule::ReadySchedule, HGraph, NodeId, ValueState};
use crate::ir::{ExecutionPlan, OIr, OIrProgram, PlanNodeId, PlanNodeKind};
use crate::value::OValue;

use super::parallel;
use super::trace::TraceSink;

/// One committed-or-pending operation the coordinator tracks.
struct OpState {
    plan_node: PlanNodeId,
    ordinal: u64,
    value_output: NodeId,
    inputs: Vec<NodeId>,
    outputs: Vec<NodeId>,
    effect: EffectSummary,
    completed: bool,
}

pub struct Coordinator<'a> {
    program: &'a OIrProgram,
    plan: &'a ExecutionPlan,
    flat: Vec<&'a OIr>,
    ops: Vec<OpState>,
    materialized: HashSet<NodeId>,
    failed_outputs: HashMap<NodeId, String>,
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
    ) -> Result<Self> {
        let flat = program.flatten_for_plan();
        if flat.len() != plan.nodes.len() {
            bail!(
                "OIR flatten produced {} nodes but plan has {} nodes",
                flat.len(),
                plan.nodes.len()
            );
        }

        hgraph
            .validate_execution_source(program, plan)
            .map_err(anyhow::Error::msg)?;
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
                Ok(OpState {
                    plan_node: op.plan_node,
                    ordinal: op.ordinal,
                    value_output: op.value_output,
                    inputs: op.inputs.clone(),
                    outputs: op.outputs.clone(),
                    effect,
                    completed: false,
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
            program,
            plan,
            flat,
            ops,
            materialized,
            failed_outputs: HashMap::new(),
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
                     (dependency cycle, failed input, or unsatisfiable constraint; \
                     {} failed outputs)",
                    self.ops.iter().filter(|op| !op.completed).count(),
                    self.ops.len(),
                    self.failed_outputs.len()
                );
            }

            let parallel: Vec<usize> = ready
                .iter()
                .copied()
                .filter(|&index| self.is_parallel_safe(index))
                .collect();

            if !parallel.is_empty() {
                self.run_parallel_batch(&parallel)?;
                // A completed batch can expose an earlier-ordinal operation.
                // Always derive a fresh frontier before launching more work.
                continue;
            }

            // Coordinator-thread work uses mutable evaluator state. Run only
            // the smallest currently legal operation, then recompute readiness
            // so a stale frontier can never launch a later effect first.
            self.run_coordinator_op(evaluator, ready[0])?;
        }
    }

    /// Indices of operations for which every ordinary/state/control input has
    /// materialized successfully.
    fn ready_ops(&self) -> Vec<usize> {
        let mut ready: Vec<usize> = (0..self.ops.len())
            .filter(|&index| {
                let op = &self.ops[index];
                !op.completed
                    && op
                        .inputs
                        .iter()
                        .all(|input| self.materialized.contains(input))
            })
            .collect();
        ready.sort_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0));
        ready
    }

    /// Whether an operation may run on a worker thread: it must have a verified,
    /// deterministic, infallible, resource-free summary and a Send-only inline
    /// renderer implementation. Source assertions cannot establish this class.
    fn is_parallel_safe(&self, index: usize) -> bool {
        let id = self.ops[index].plan_node;
        if !self.ops[index].effect.is_verified_pure_infallible() {
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
        for ((index, id, _), result) in tasks.into_iter().zip(results) {
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
                    self.materialize_success(index);
                }
                Err(err) => {
                    self.trace.failed(id, err.to_string());
                    self.record_failure(index, &err.to_string());
                    return Err(err);
                }
            }
        }
        Ok(())
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
        self.ops[index].completed = true;
    }

    fn record_failure(&mut self, index: usize, message: &str) {
        for output in self.ops[index].outputs.clone() {
            self.materialized.remove(&output);
            self.failed_outputs.insert(output, message.to_string());
        }
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
    use crate::hgraph::from_oir::build_program;
    use crate::hgraph::HNodeKind;
    use crate::ir::BackendRegistry;

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
        let graph = build_program(&graph_program);
        let different_plan_program = OIrProgram {
            nodes: vec![OIr::Store {
                name: "x".into(),
                expr: Box::new(OIr::Text("effect classification differs".into())),
            }],
        };
        let different_plan = different_plan_program.plan();
        let error = Coordinator::new(
            &different_plan_program,
            &different_plan,
            &graph,
            Policy::Eager,
        )
        .err()
        .expect("a graph cannot schedule unrelated OIR");
        assert!(error
            .to_string()
            .contains("does not match the HGraph source plan"));

        // Text content is absent from PlanNodeKind, so exact OIR provenance is
        // checked independently even when the two ExecutionPlans compare equal.
        let source = OIrProgram {
            nodes: vec![OIr::Text("source".into())],
        };
        let source_graph = build_program(&source);
        let different_text = OIrProgram {
            nodes: vec![OIr::Text("different".into())],
        };
        let same_shape_plan = different_text.plan();
        let error = Coordinator::new(
            &different_text,
            &same_shape_plan,
            &source_graph,
            Policy::Eager,
        )
        .err()
        .expect("same-shaped OIR must still match graph provenance");
        assert!(error
            .to_string()
            .contains("does not match HGraph source provenance"));
    }

    #[test]
    fn failed_operation_produces_no_value_completion_or_resource_state() {
        let program = OIrProgram {
            nodes: vec![OIr::Load("missing".into())],
        };
        let plan = program.plan();
        let graph = build_program(&program);
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
        assert!(outputs.iter().any(|output| matches!(
            graph.node(*output).map(|node| &node.kind),
            Some(HNodeKind::ResourceState {
                resource: crate::effects::ResourceKey::ScopeBinding(name),
                version: 1,
            }) if name == "missing"
        )));

        let mut coordinator =
            Coordinator::new(&program, &plan, &graph, Policy::Eager).expect("valid graph");
        assert!(outputs
            .iter()
            .all(|output| !coordinator.materialized.contains(output)));

        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        let error = coordinator
            .run(&mut evaluator, &mut scope)
            .expect_err("undefined load must fail");
        assert!(error.to_string().contains("Undefined variable: $missing"));

        assert!(
            coordinator.frame.values[load_id.0].is_none(),
            "a failed operation must not publish its ordinary value"
        );
        assert!(
            !coordinator.ops[0].completed,
            "failure must not be recorded as successful completion"
        );
        for output in outputs {
            assert!(
                !coordinator.materialized.contains(&output),
                "failed output {output:?} became materialized"
            );
            assert_eq!(
                coordinator.failed_outputs.get(&output).map(String::as_str),
                Some("Undefined variable: $missing"),
                "every ordinary, completion, and state output must carry failure"
            );
        }
    }
}
