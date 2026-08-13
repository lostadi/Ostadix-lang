//! Parallel worker pool for provably-safe operations.
//!
//! Compiler-verified O-level loads, pure inline renderers, and direct ephemeral
//! members of an explicitly autonomous coordination group are dispatched here.
//! The hosted path is deliberately non-strict: explicit O-value dependencies
//! are preserved, but hidden host effects among already-started members are
//! unordered. Ordinary ephemeral shims remain coordinator-owned and strict.
//! Every task receives owned materialized inputs, so no reference to the
//! `!Send` evaluator crosses a thread boundary. Results and failures still
//! settle through the coordinator in deterministic O order.

use anyhow::{bail, Result};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(test)]
use std::sync::{Condvar, Mutex, MutexGuard};
#[cfg(test)]
use std::time::Duration;

use crate::capability::BackendSandboxPolicy;
use crate::effects::{EffectConfidence, EffectSummary, Fallibility, ResourceKey};
use crate::eval::{render_with, GraphEvalFrame};
use crate::evidence::DispatchAdapterV1;
use crate::ir::{ExecutionMode, ExecutionPlan, OIr, PlanNodeId, PlanNodeKind, SpliceRenderer};
use crate::process::run_ephemeral_with_eval_callback;
use crate::value::OValue;

use super::task::{PreparedTask, TaskContext};

/// A statically-determined parallel task classification for a plan node.
#[derive(Clone, Debug)]
pub enum TaskKind {
    Renderer {
        renderer: SpliceRenderer,
        canonical: String,
    },
    Load {
        name: String,
    },
    EphemeralShim {
        language: String,
        renderer: SpliceRenderer,
    },
}

impl TaskKind {
    pub(crate) const fn adapter(&self) -> DispatchAdapterV1 {
        match self {
            Self::Renderer { .. } => DispatchAdapterV1::TrustedInlineRendererV1,
            Self::Load { .. } => DispatchAdapterV1::OScopeLoadV1,
            Self::EphemeralShim { .. } => DispatchAdapterV1::AutonomousEphemeralShimV1,
        }
    }
}

/// A fully-built, Send-only render task.
#[derive(Clone, Debug)]
struct ParallelTask {
    body: ParallelTaskBody,
    #[cfg(test)]
    overlap_probe: Option<Arc<TestOverlapProbe>>,
    #[cfg(test)]
    pipeline_probe: Option<Arc<TestPipelineProbe>>,
    #[cfg(test)]
    plan_node: PlanNodeId,
}

#[derive(Clone, Debug)]
enum ParallelTaskBody {
    Render {
        renderer: SpliceRenderer,
        canonical: String,
        parts: Vec<RenderPart>,
    },
    Load {
        name: String,
        scope: Arc<HashMap<String, OValue>>,
    },
    EphemeralShim {
        language: String,
        code: String,
        bindings: HashMap<String, OValue>,
        shim: PathBuf,
        sandbox: BackendSandboxPolicy,
        executable_leases: Arc<crate::runtime_exec::ExecutableLeaseSet>,
    },
}

/// Live runtime data captured only after the coordinator validates the backend
/// artifact and resolves the process-local capability bearer.
#[derive(Clone, Debug)]
pub(crate) struct EphemeralShimRuntime {
    shim: PathBuf,
    sandbox: BackendSandboxPolicy,
    executable_leases: Arc<crate::runtime_exec::ExecutableLeaseSet>,
}

impl EphemeralShimRuntime {
    pub(crate) fn new(
        shim: PathBuf,
        sandbox: BackendSandboxPolicy,
        executable_leases: Arc<crate::runtime_exec::ExecutableLeaseSet>,
    ) -> Self {
        Self {
            shim,
            sandbox,
            executable_leases,
        }
    }
}

#[derive(Clone, Debug)]
enum RenderPart {
    /// Literal text spliced verbatim (a `Text` child).
    Verbatim(String),
    /// A materialized value spliced through the backend renderer.
    Splice(Box<OValue>),
}

/// Test-only rendezvous that records how many local workers are executing at
/// the same time. Early workers wait for the expected batch, but the wait is
/// bounded so a regression to sequential execution fails instead of hanging.
#[cfg(test)]
#[derive(Debug)]
struct TestOverlapProbe {
    expected: usize,
    owner: std::thread::ThreadId,
    state: Mutex<TestOverlapState>,
    changed: Condvar,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestOverlapState {
    active: usize,
    arrived: usize,
    peak: usize,
    ran_off_owner: bool,
}

#[cfg(test)]
static TEST_OBSERVATION_LOCK: Mutex<()> = Mutex::new(());
#[cfg(test)]
static ACTIVE_TEST_PROBE: Mutex<Option<(std::thread::ThreadId, Arc<TestOverlapProbe>)>> =
    Mutex::new(None);

#[cfg(test)]
#[derive(Debug)]
struct TestPipelineProbe {
    blocked: PlanNodeId,
    downstream: PlanNodeId,
    state: Mutex<TestPipelineState>,
    changed: Condvar,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestPipelineState {
    downstream_started: bool,
    downstream_started_before_blocked_finished: bool,
}

#[cfg(test)]
static ACTIVE_PIPELINE_PROBE: Mutex<Option<(std::thread::ThreadId, Arc<TestPipelineProbe>)>> =
    Mutex::new(None);

/// Exclusive, thread-scoped observation of tasks built by one graph
/// coordinator test. Matching the builder thread prevents unrelated parallel
/// tests from contributing to the overlap witness.
#[cfg(test)]
pub(crate) struct TestOverlapSession {
    probe: Arc<TestOverlapProbe>,
    _guard: MutexGuard<'static, ()>,
}

/// Test-only gate proving that newly-ready work can enter the persistent pool
/// while an unrelated task from the prior frontier remains in flight.
#[cfg(test)]
pub(crate) struct TestPipelineSession {
    probe: Arc<TestPipelineProbe>,
    _guard: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl TestOverlapSession {
    pub(crate) fn begin(expected: usize) -> Self {
        let guard = TEST_OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let probe = Arc::new(TestOverlapProbe::new(expected, std::thread::current().id()));
        *ACTIVE_TEST_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((std::thread::current().id(), Arc::clone(&probe)));
        Self {
            probe,
            _guard: guard,
        }
    }

    pub(crate) fn peak(&self) -> usize {
        self.probe.peak()
    }

    fn ran_off_owner(&self) -> bool {
        self.probe.ran_off_owner()
    }
}

#[cfg(test)]
impl TestPipelineSession {
    pub(crate) fn begin(blocked: PlanNodeId, downstream: PlanNodeId) -> Self {
        let guard = TEST_OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let probe = Arc::new(TestPipelineProbe {
            blocked,
            downstream,
            state: Mutex::new(TestPipelineState::default()),
            changed: Condvar::new(),
        });
        *ACTIVE_PIPELINE_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((std::thread::current().id(), Arc::clone(&probe)));
        Self {
            probe,
            _guard: guard,
        }
    }

    pub(crate) fn downstream_started_before_blocked_finished(&self) -> bool {
        self.probe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .downstream_started_before_blocked_finished
    }
}

#[cfg(test)]
impl Drop for TestOverlapSession {
    fn drop(&mut self) {
        let mut active = ACTIVE_TEST_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active
            .as_ref()
            .is_some_and(|(_, probe)| Arc::ptr_eq(probe, &self.probe))
        {
            *active = None;
        }
    }
}

#[cfg(test)]
impl Drop for TestPipelineSession {
    fn drop(&mut self) {
        let mut active = ACTIVE_PIPELINE_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active
            .as_ref()
            .is_some_and(|(_, probe)| Arc::ptr_eq(probe, &self.probe))
        {
            *active = None;
        }
    }
}

#[cfg(test)]
fn active_test_probe() -> Option<Arc<TestOverlapProbe>> {
    ACTIVE_TEST_PROBE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|(owner, _)| *owner == std::thread::current().id())
        .map(|(_, probe)| Arc::clone(probe))
}

#[cfg(test)]
fn active_pipeline_probe() -> Option<Arc<TestPipelineProbe>> {
    ACTIVE_PIPELINE_PROBE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|(owner, _)| *owner == std::thread::current().id())
        .map(|(_, probe)| Arc::clone(probe))
}

#[cfg(test)]
pub(crate) fn worker_count_hint() -> Option<usize> {
    active_test_probe()
        .map(|probe| probe.expected)
        .or_else(|| active_pipeline_probe().map(|_| 2))
}

#[cfg(test)]
impl TestOverlapProbe {
    fn new(expected: usize, owner: std::thread::ThreadId) -> Self {
        Self {
            expected,
            owner,
            state: Mutex::new(TestOverlapState::default()),
            changed: Condvar::new(),
        }
    }

    fn enter(self: &Arc<Self>) -> TestOverlapGuard {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active += 1;
        state.arrived += 1;
        state.peak = state.peak.max(state.active);
        state.ran_off_owner |= std::thread::current().id() != self.owner;
        self.changed.notify_all();

        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| {
                state.arrived < self.expected
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(state);

        TestOverlapGuard {
            probe: Arc::clone(self),
        }
    }

    fn peak(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .peak
    }

    fn ran_off_owner(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ran_off_owner
    }
}

#[cfg(test)]
impl TestPipelineProbe {
    fn enter(&self, plan_node: PlanNodeId) {
        if plan_node == self.downstream {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.downstream_started = true;
            self.changed.notify_all();
            return;
        }

        if plan_node == self.blocked {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (mut state, _) = self
                .changed
                .wait_timeout_while(state, Duration::from_secs(5), |state| {
                    !state.downstream_started
                })
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.downstream_started_before_blocked_finished = state.downstream_started;
        }
    }
}

#[cfg(test)]
struct TestOverlapGuard {
    probe: Arc<TestOverlapProbe>,
}

#[cfg(test)]
impl Drop for TestOverlapGuard {
    fn drop(&mut self) {
        let mut state = self
            .probe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active -= 1;
        self.probe.changed.notify_all();
    }
}

/// Classify a plan node when a local Send-only task adapter is available.
pub(crate) fn classify(plan: &ExecutionPlan, oir: &OIr, id: PlanNodeId) -> Option<TaskKind> {
    match oir {
        OIr::Load(name) => Some(TaskKind::Load { name: name.clone() }),
        OIr::Exec { attr, backend, .. }
            if attr.is_none()
                && backend.pure
                && backend.execution == ExecutionMode::InlineValue
                && renderer_inputs_statically_preparable(oir) =>
        {
            match backend.canonical.as_str() {
                "html" | "markdown" | "text" | "latex" => Some(TaskKind::Renderer {
                    renderer: backend.renderer,
                    canonical: backend.canonical.clone(),
                }),
                _ => None,
            }
        }
        OIr::Exec { backend, .. }
            if crate::hgraph::from_oir::autonomous_ephemeral_group(plan, id, oir).is_some() =>
        {
            Some(TaskKind::EphemeralShim {
                language: backend.canonical.clone(),
                renderer: backend.renderer,
            })
        }
        _ => None,
    }
}

/// Validate that the exact adapter selected by admission still matches the
/// admitted OIR shape. This is a consistency check, not a second adapter
/// selection step.
pub(crate) fn adapter_matches(
    adapter: DispatchAdapterV1,
    plan: &ExecutionPlan,
    id: PlanNodeId,
    oir: &OIr,
) -> bool {
    match adapter {
        DispatchAdapterV1::OScopeLoadV1 => matches!(oir, OIr::Load(_)),
        DispatchAdapterV1::TrustedInlineRendererV1 => renderer_inputs_statically_preparable(oir),
        DispatchAdapterV1::AutonomousEphemeralShimV1 => {
            crate::hgraph::from_oir::autonomous_ephemeral_group(plan, id, oir).is_some()
        }
        DispatchAdapterV1::CoordinatorV1 => false,
    }
}

/// Admission may claim an exact local-worker lane only when preparation is
/// source-proven. Arbitrary materialized values can contain a lazy Eval
/// request that needs the mutable evaluator to force, so v1 admits only the
/// closed renderer tree already trusted by automatic sequence relaxation.
fn renderer_inputs_statically_preparable(oir: &OIr) -> bool {
    let OIr::Exec {
        attr,
        backend,
        body,
        ..
    } = oir
    else {
        return false;
    };
    attr.is_none()
        && backend.pure
        && backend.execution == ExecutionMode::InlineValue
        && matches!(
            backend.canonical.as_str(),
            "html" | "markdown" | "text" | "latex"
        )
        && body.iter().all(|child| match child {
            OIr::Text(_) | OIr::Store { .. } => true,
            OIr::Exec { .. } => renderer_inputs_statically_preparable(child),
            OIr::Load(_) | OIr::Invoke { .. } => false,
        })
}

/// Hard effect/failure predicate for worker preparation. Fallible loads are
/// admitted because they can only read an O scope binding and their outcome is
/// buffered; hosted or user-declared reads never enter this class.
pub(crate) fn effect_contract_worker_safe(summary: &EffectSummary, oir: &OIr) -> bool {
    match oir {
        OIr::Exec {
            env_id, backend, ..
        } if *env_id == u32::MAX && backend.execution == ExecutionMode::Shim => {
            summary.unknown
                && summary.fallibility == Fallibility::MayFail
                && summary.actor_state.is_none()
        }
        OIr::Load(_) => {
            summary.confidence == EffectConfidence::Verified
                && summary.deterministic
                && summary.fallibility == Fallibility::MayFail
                && !summary.unknown
                && summary.actor_state.is_none()
                && summary.writes.is_empty()
                && summary
                    .reads
                    .iter()
                    .all(|resource| matches!(resource, ResourceKey::ScopeBinding(_)))
                && !summary.network
                && !summary.spawn
                && !summary.clock
        }
        _ => summary.is_verified_pure_infallible(),
    }
}

/// Whether an inline block's already-materialized splice inputs are safe to
/// render off-thread: none of them are unforced `Eval` requests (which would
/// need the evaluator to force).
pub(crate) fn render_inputs_pure(
    frame: &GraphEvalFrame,
    plan: &ExecutionPlan,
    id: PlanNodeId,
) -> bool {
    if matches!(plan.nodes[id.0].kind, PlanNodeKind::Load { .. }) {
        return true;
    }
    let Ok(children) = plan.child_schedule(id) else {
        return false;
    };
    for child in children {
        if matches!(plan.nodes[child.0].kind, PlanNodeKind::Store { .. }) {
            continue;
        }
        if matches!(plan.nodes[child.0].kind, PlanNodeKind::Text) {
            continue;
        }
        match frame.value(child) {
            Ok(OValue::Request {
                kind: crate::value::RequestKind::Eval { .. },
                ..
            }) => return false,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    true
}

/// Build a Send-only task from the frame's already-materialized inputs.
fn build_task(
    frame: &GraphEvalFrame,
    plan: &ExecutionPlan,
    id: PlanNodeId,
    kind: TaskKind,
    shim_runtime: Option<EphemeralShimRuntime>,
) -> Result<ParallelTask> {
    let body = match kind {
        TaskKind::Load { name } => {
            // Preparation freezes the operation's admitted scope. The lookup
            // itself remains worker work, so two reads of one version execute
            // concurrently without sharing the mutable evaluator frame.
            let scope = Arc::new(frame.scope_from_data_edges(id, plan)?);
            ParallelTaskBody::Load { name, scope }
        }
        TaskKind::EphemeralShim { language, renderer } => {
            let EphemeralShimRuntime {
                shim,
                sandbox,
                executable_leases,
            } = shim_runtime.ok_or_else(|| {
                anyhow::anyhow!("ephemeral shim adapter requires an authorized runtime binding")
            })?;
            let children = plan.child_schedule(id).map_err(anyhow::Error::msg)?;
            let bindings = frame.exec_scope(id, plan)?;
            let mut code = String::new();
            for child in children {
                match &plan.nodes[child.0].kind {
                    PlanNodeKind::Store { .. } => {}
                    PlanNodeKind::Text => {
                        if let OValue::Text { v } = frame.value(child)? {
                            code.push_str(&v.utf8);
                        } else {
                            bail!("text plan node {} did not materialize a string", child.0);
                        }
                    }
                    _ => code.push_str(&render_with(renderer, frame.value(child)?)),
                }
            }
            ParallelTaskBody::EphemeralShim {
                language,
                code,
                bindings,
                shim,
                sandbox,
                executable_leases,
            }
        }
        TaskKind::Renderer {
            renderer,
            canonical,
        } => {
            let children = plan.child_schedule(id).map_err(anyhow::Error::msg)?;
            let mut parts = Vec::new();
            for child in children {
                match &plan.nodes[child.0].kind {
                    // Store children contribute only to the (unused for inline
                    // value output) local scope.
                    PlanNodeKind::Store { .. } => {}
                    PlanNodeKind::Text => {
                        if let OValue::Text { v } = frame.value(child)? {
                            parts.push(RenderPart::Verbatim(v.utf8.clone()));
                        } else {
                            bail!("text plan node {} did not materialize a string", child.0);
                        }
                    }
                    _ => {
                        parts.push(RenderPart::Splice(Box::new(frame.value(child)?.clone())));
                    }
                }
            }
            ParallelTaskBody::Render {
                renderer,
                canonical,
                parts,
            }
        }
    };
    Ok(ParallelTask {
        body,
        #[cfg(test)]
        overlap_probe: active_test_probe(),
        #[cfg(test)]
        pipeline_probe: active_pipeline_probe(),
        #[cfg(test)]
        plan_node: id,
    })
}

/// Prepare the exact adapter named by the hard dispatch evidence. Runtime code
/// may reject a stale/mismatched adapter, but it never reclassifies the node to
/// choose a different execution lane.
pub(crate) fn prepare(
    adapter: DispatchAdapterV1,
    frame: &GraphEvalFrame,
    plan: &ExecutionPlan,
    id: PlanNodeId,
    oir: &OIr,
    shim_runtime: Option<EphemeralShimRuntime>,
) -> Result<Box<dyn PreparedTask>> {
    let kind = match adapter {
        DispatchAdapterV1::OScopeLoadV1 => match oir {
            OIr::Load(name) => TaskKind::Load { name: name.clone() },
            _ => bail!(
                "operation {} no longer matches admitted adapter {}",
                id.0,
                adapter.name()
            ),
        },
        DispatchAdapterV1::TrustedInlineRendererV1
            if renderer_inputs_statically_preparable(oir) =>
        {
            let OIr::Exec { backend, .. } = oir else {
                unreachable!("renderer preparability requires an Exec node")
            };
            TaskKind::Renderer {
                renderer: backend.renderer,
                canonical: backend.canonical.clone(),
            }
        }
        DispatchAdapterV1::AutonomousEphemeralShimV1 if adapter_matches(adapter, plan, id, oir) => {
            let OIr::Exec { backend, .. } = oir else {
                unreachable!("ephemeral shim adapter requires an Exec node")
            };
            TaskKind::EphemeralShim {
                language: backend.canonical.clone(),
                renderer: backend.renderer,
            }
        }
        DispatchAdapterV1::TrustedInlineRendererV1
        | DispatchAdapterV1::AutonomousEphemeralShimV1
        | DispatchAdapterV1::CoordinatorV1 => bail!(
            "operation {} no longer matches admitted adapter {}",
            id.0,
            adapter.name()
        ),
    };
    Ok(Box::new(build_task(frame, plan, id, kind, shim_runtime)?))
}

/// Execute one owned prepared task on a worker thread. Strict adapters are
/// effect-free; the explicit autonomous shim adapter may perform the unordered
/// hosted effects recorded by its admission contract.
fn execute_prepared(task: &ParallelTask, context: &TaskContext) -> Result<OValue> {
    #[cfg(test)]
    let _overlap_guard = task.overlap_probe.as_ref().map(TestOverlapProbe::enter);
    #[cfg(test)]
    if let Some(probe) = task.pipeline_probe.as_ref() {
        probe.enter(task.plan_node);
    }

    match &task.body {
        ParallelTaskBody::Load { name, scope } => scope
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Undefined variable: ${name}")),
        ParallelTaskBody::Render {
            renderer,
            canonical,
            parts,
        } => {
            let mut buf = String::new();
            for part in parts {
                match part {
                    RenderPart::Verbatim(text) => buf.push_str(text),
                    RenderPart::Splice(value) => buf.push_str(&render_with(*renderer, value)),
                }
            }
            match canonical.as_str() {
                "html" => Ok(OValue::html(buf)),
                "markdown" | "text" | "latex" => Ok(OValue::str_(buf)),
                other => bail!("inline OIR backend `{other}` has no value executor"),
            }
        }
        ParallelTaskBody::EphemeralShim {
            language,
            code,
            bindings,
            shim,
            sandbox,
            executable_leases,
        } => {
            let lexical_bindings = bindings.clone();
            run_ephemeral_with_eval_callback(
                language,
                code,
                bindings.clone(),
                shim,
                sandbox,
                Some(executable_leases),
                |src, explicit_scope, remaining| {
                    let callback_scope = match explicit_scope {
                        None => lexical_bindings.clone(),
                        Some(OValue::Scope { bindings }) => bindings,
                        Some(other) => bail!(
                            "O.eval explicit scope must be an OScope, got {}",
                            other.type_name()
                        ),
                    };
                    context.eval_o_source_with_timeout(src, callback_scope, remaining)
                },
            )
        }
    }
}

impl PreparedTask for ParallelTask {
    fn execute(self: Box<Self>, context: &TaskContext) -> Result<OValue> {
        execute_prepared(&self, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{ExecutionTrace, Policy};
    use crate::executor::pool::WorkerPool;
    use crate::executor::task::{TaskOutcome, TaskSubmission, TaskToken};

    #[test]
    fn execute_runs_safe_renderers_with_real_worker_overlap() {
        let task_count = 4;
        let session = TestOverlapSession::begin(task_count);
        let tasks = (0..task_count)
            .map(|index| ParallelTask {
                body: ParallelTaskBody::Render {
                    renderer: SpliceRenderer::Default,
                    canonical: "text".to_string(),
                    parts: vec![RenderPart::Verbatim(format!("renderer-{index}"))],
                },
                overlap_probe: Some(Arc::clone(&session.probe)),
                pipeline_probe: None,
                plan_node: PlanNodeId(index),
            })
            .collect::<Vec<_>>();

        let mut pool = WorkerPool::new(task_count).unwrap();
        for (index, task) in tasks.into_iter().enumerate() {
            pool.submit(TaskSubmission::new(TaskToken(index), Box::new(task)))
                .unwrap();
        }
        let results = (0..task_count)
            .map(|_| match pool.recv_completion().unwrap().outcome {
                TaskOutcome::Completed(result) => result,
                TaskOutcome::InfrastructureAbort(error) => {
                    panic!("renderer worker infrastructure failed: {error:#}")
                }
            })
            .collect::<Vec<_>>();

        assert!(
            results.iter().all(Result::is_ok),
            "all parallel renderer tasks should complete successfully"
        );
        assert!(
            session.peak() > 1,
            "expected overlapping worker execution, observed peak {}",
            session.peak()
        );
    }

    #[test]
    fn execute_keeps_singleton_local_worker_placement() {
        let session = TestOverlapSession::begin(1);
        let task = ParallelTask {
            body: ParallelTaskBody::Render {
                renderer: SpliceRenderer::Default,
                canonical: "text".to_string(),
                parts: vec![RenderPart::Verbatim("singleton".to_string())],
            },
            overlap_probe: Some(Arc::clone(&session.probe)),
            pipeline_probe: None,
            plan_node: PlanNodeId(0),
        };

        let mut pool = WorkerPool::new(1).unwrap();
        pool.submit(TaskSubmission::new(TaskToken(0), Box::new(task)))
            .unwrap();
        let results = [match pool.recv_completion().unwrap().outcome {
            TaskOutcome::Completed(result) => result,
            TaskOutcome::InfrastructureAbort(error) => {
                panic!("renderer worker infrastructure failed: {error:#}")
            }
        }];

        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
        assert!(
            session.ran_off_owner(),
            "admitted local-worker task executed on the coordinator thread"
        );
    }

    #[test]
    fn preparation_consumes_the_admitted_adapter_without_reclassification() {
        let program = crate::ir::OIrProgram {
            nodes: vec![OIr::Load("bound".into())],
        };
        let plan = program.plan();
        let id = plan.roots[0];
        let frame = GraphEvalFrame {
            values: vec![None; plan.nodes.len()],
            base_scope: HashMap::from([("bound".to_string(), OValue::str_("value"))]),
            node_policy: vec![Policy::Eager; plan.nodes.len()],
            trace: ExecutionTrace::new(),
        };

        let error = prepare(
            DispatchAdapterV1::TrustedInlineRendererV1,
            &frame,
            &plan,
            id,
            &program.nodes[0],
            None,
        )
        .err()
        .expect("a renderer adapter cannot prepare an O scope load");
        assert!(error
            .to_string()
            .contains("no longer matches admitted adapter"));

        let task = prepare(
            DispatchAdapterV1::OScopeLoadV1,
            &frame,
            &plan,
            id,
            &program.nodes[0],
            None,
        )
        .expect("the admitted scope-load adapter remains preparable");
        let (events, _event_rx) = std::sync::mpsc::channel();
        let context = TaskContext::new(TaskToken(0), events);
        assert_eq!(task.execute(&context).unwrap(), OValue::str_("value"));
    }
}
