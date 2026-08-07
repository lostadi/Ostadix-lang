//! The graph-execution coordinator.
//!
//! The coordinator owns the mutable execution state for one plan evaluation and
//! drives a readiness-based event loop over the plan's operation hyperedges.
//! An operation becomes ready exactly when all ordinary and synthetic input
//! nodes are materialized. Data, source completion, resource state, and actor
//! state therefore share one directed producer/input dependency rule.
//!
//! Compiler-verified O scope reads and attribute-free verified inline
//! renderers execute as prepared Send-only tasks via `std::thread::scope`;
//! every other operation — anything that needs the evaluator's `!Send`
//! process registry or mutable state — runs on the coordinator thread in
//! stable ordinal order. Results are settled by semantic ordinal and committed
//! in deterministic root order. State/control inputs, rather than commit
//! order, preserve externally observable effect ordering.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Result};

use crate::effects::{EffectSummary, Fallibility};
use crate::eval::{derive_policy_contexts, Evaluator, ExecutionTrace, GraphEvalFrame, Policy};
use crate::evidence::{AdmittedExecution, DispatchLaneV1};
use crate::hgraph::{schedule::ReadySchedule, NodeId, ValueState};
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
    dispatch_lane: DispatchLaneV1,
    completed: bool,
}

pub struct Coordinator<'a> {
    admitted: AdmittedExecution<'a>,
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
                let dispatch_lane = admitted_operations
                    .get(&op.plan_node)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "operation {} has no admitted dispatch contract",
                            op.plan_node.0
                        )
                    })?
                    .evidence
                    .dispatch_contract
                    .lane;
                Ok(OpState {
                    plan_node: op.plan_node,
                    ordinal: op.ordinal,
                    value_output: op.value_output,
                    inputs: op.inputs.clone(),
                    outputs: op.outputs.clone(),
                    effect,
                    dispatch_lane,
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
            admitted,
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

            if let Some(index) = ready.iter().copied().find(|&index| {
                self.ops[index].dispatch_lane == DispatchLaneV1::LocalWorker
                    && !self.is_parallel_safe(index)
            }) {
                bail!(
                    "operation {} cannot satisfy its admitted local-worker preparation contract",
                    self.ops[index].plan_node.0
                );
            }

            // Infallible, effect-free worker tasks may speculate anywhere in
            // the legal frontier: they cannot change strict failure or effect
            // selection.
            let infallible_parallel: Vec<usize> = ready
                .iter()
                .copied()
                .filter(|&index| {
                    self.is_parallel_safe(index)
                        && self.ops[index].effect.fallibility == Fallibility::Infallible
                })
                .collect();

            if !infallible_parallel.is_empty() {
                self.run_parallel_batch(&infallible_parallel)?;
                // A completed batch can expose an earlier-ordinal operation.
                // Always derive a fresh frontier before launching more work.
                continue;
            }

            // A pure fallible task may execute speculatively only as part of
            // the contiguous unfinished semantic prefix. This allows adjacent
            // loads to overlap while preventing a later missing binding from
            // preempting an earlier blocked/coordinator failure or effect.
            let ready_set = ready.iter().copied().collect::<HashSet<_>>();
            let mut unfinished = (0..self.ops.len())
                .filter(|&index| !self.ops[index].completed)
                .collect::<Vec<_>>();
            unfinished.sort_by_key(|&index| (self.ops[index].ordinal, self.ops[index].plan_node.0));
            let fallible_prefix = unfinished
                .into_iter()
                .take_while(|&index| {
                    ready_set.contains(&index)
                        && self.is_parallel_safe(index)
                        && self.ops[index].effect.fallibility == Fallibility::MayFail
                })
                .collect::<Vec<_>>();
            if !fallible_prefix.is_empty() {
                self.run_parallel_batch(&fallible_prefix)?;
                continue;
            }

            // Coordinator-thread work uses mutable evaluator state. Run only
            // the smallest currently legal operation, then recompute readiness
            // so a stale frontier can never launch a later effect first.
            let coordinator = ready
                .iter()
                .copied()
                .find(|&index| self.ops[index].dispatch_lane == DispatchLaneV1::Coordinator)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "admitted local-worker task cannot satisfy its preparation contract or is blocked by an earlier unfinished operation"
                    )
                })?;
            self.run_coordinator_op(evaluator, coordinator)?;
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

    /// Whether an operation may run on a worker thread: admission must select
    /// the local-worker lane, the hard effect/failure contract must be safe,
    /// and a Send-only preparation adapter must still be available. Source
    /// assertions cannot establish this class.
    fn is_parallel_safe(&self, index: usize) -> bool {
        let id = self.ops[index].plan_node;
        if self.ops[index].dispatch_lane != DispatchLaneV1::LocalWorker {
            return false;
        }
        if !parallel::effect_contract_worker_safe(&self.ops[index].effect, self.flat[id.0]) {
            return false;
        }
        // Classification is now the task builder/adapter-availability check;
        // the immutable admission lane above is the dispatch authority.
        parallel::classify(self.plan, self.flat[id.0], id).is_some()
            && parallel::render_inputs_pure(&self.frame, self.plan, id)
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

    /// Execute a batch of parallel-safe operations on worker threads, then
    /// commit their values into the frame in deterministic ordinal order. If
    /// any worker fails, the smallest-ordinal failure is selected.
    fn run_parallel_batch(&mut self, batch: &[usize]) -> Result<()> {
        // Build Send-only tasks on the coordinator thread from already
        // materialized inputs, then execute their immutable envelopes on
        // worker threads.
        let mut tasks: Vec<(usize, PlanNodeId, parallel::ParallelTask)> = Vec::new();
        for &index in batch {
            let id = self.ops[index].plan_node;
            let task = parallel::classify(self.plan, self.flat[id.0], id)
                .expect("parallel-safe op must classify");
            let built = parallel::build_task(&self.frame, self.plan, id, task)?;
            tasks.push((index, id, built));
        }

        // Preparation must succeed for the whole dispatch set before any
        // operation is observed as started. A broken Send-only adapter cannot
        // leave an unterminated lifecycle in the execution trace.
        for (_, id, _) in &tasks {
            self.trace.ready(*id);
            self.trace.started(*id);
        }

        let results = parallel::execute(tasks.iter().map(|(_, _, task)| task.clone()).collect());

        // Commit in ordinal order; select the smallest-ordinal failure.
        let mut completions: BTreeMap<(u64, usize), (usize, PlanNodeId, Result<OValue>)> =
            BTreeMap::new();
        for ((index, id, _), result) in tasks.into_iter().zip(results) {
            let ordinal = self.ops[index].ordinal;
            completions.insert((ordinal, id.0), (index, id, result));
        }

        let selected_failure = completions
            .iter()
            .find_map(|(order, (_, _, result))| result.is_err().then_some(*order));
        let mut selected_error = None;
        for (order, (index, id, result)) in completions {
            if selected_failure.is_some_and(|failure| order > failure) {
                self.trace.discarded(
                    id,
                    format!(
                        "strict fail-stop withheld result after operation {} failed",
                        selected_failure.expect("checked above").1
                    ),
                );
                continue;
            }
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
                    selected_error = Some(err);
                }
            }
        }
        match selected_error {
            Some(error) => Err(error),
            None => Ok(()),
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
    use crate::evidence::{admit_execution, analyze_execution};
    use crate::hgraph::from_oir::build_program;
    use crate::hgraph::solve::solve_types;
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
}
