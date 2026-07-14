//! Parallel worker pool for provably-safe operations.
//!
//! Only pure, attribute-free inline value renderers (`html`, `markdown`,
//! `text`, `latex`) are dispatched here. Their inputs are already materialized
//! `OValue`s, so the render is a pure function of Send-only data: no reference
//! to the `!Send` evaluator ever crosses a thread boundary. Renders run on
//! `std::thread::scope` worker threads and their results are committed by the
//! coordinator in deterministic order.

use anyhow::{bail, Result};

#[cfg(test)]
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
#[cfg(test)]
use std::time::Duration;

use crate::eval::{render_with, GraphEvalFrame};
use crate::ir::{ExecutionMode, ExecutionPlan, OIr, PlanNodeId, PlanNodeKind, SpliceRenderer};
use crate::value::OValue;

/// A statically-determined parallel task classification for a plan node.
#[derive(Clone, Debug)]
pub struct TaskKind {
    renderer: SpliceRenderer,
    canonical: String,
}

/// A fully-built, Send-only render task.
#[derive(Clone, Debug)]
pub struct ParallelTask {
    renderer: SpliceRenderer,
    canonical: String,
    parts: Vec<RenderPart>,
    #[cfg(test)]
    overlap_probe: Option<Arc<TestOverlapProbe>>,
}

#[derive(Clone, Debug)]
enum RenderPart {
    /// Literal text spliced verbatim (a `Text` child).
    Verbatim(String),
    /// A materialized value spliced through the backend renderer.
    Splice(Box<OValue>),
}

/// Test-only rendezvous that records how many render workers are executing at
/// the same time. Early workers wait for the expected batch, but the wait is
/// bounded so a regression to sequential execution fails instead of hanging.
#[cfg(test)]
#[derive(Debug)]
struct TestOverlapProbe {
    expected: usize,
    state: Mutex<TestOverlapState>,
    changed: Condvar,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestOverlapState {
    active: usize,
    arrived: usize,
    peak: usize,
}

#[cfg(test)]
static TEST_OBSERVATION_LOCK: Mutex<()> = Mutex::new(());
#[cfg(test)]
static ACTIVE_TEST_PROBE: Mutex<Option<(std::thread::ThreadId, Arc<TestOverlapProbe>)>> =
    Mutex::new(None);

/// Exclusive, thread-scoped observation of tasks built by one graph
/// coordinator test. Matching the builder thread prevents unrelated parallel
/// tests from contributing to the overlap witness.
#[cfg(test)]
pub(crate) struct TestOverlapSession {
    probe: Arc<TestOverlapProbe>,
    _guard: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl TestOverlapSession {
    pub(crate) fn begin(expected: usize) -> Self {
        let guard = TEST_OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let probe = Arc::new(TestOverlapProbe::new(expected));
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
fn active_test_probe() -> Option<Arc<TestOverlapProbe>> {
    ACTIVE_TEST_PROBE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|(owner, _)| *owner == std::thread::current().id())
        .map(|(_, probe)| Arc::clone(probe))
}

#[cfg(test)]
impl TestOverlapProbe {
    fn new(expected: usize) -> Self {
        Self {
            expected,
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

/// Classify a plan node as a parallel render task, if it is an attribute-free
/// pure inline value backend block.
pub(crate) fn classify(_plan: &ExecutionPlan, oir: &OIr, _id: PlanNodeId) -> Option<TaskKind> {
    let OIr::Exec { attr, backend, .. } = oir else {
        return None;
    };
    if attr.is_some() || !backend.pure {
        return None;
    }
    if backend.execution != ExecutionMode::InlineValue {
        return None;
    }
    match backend.canonical.as_str() {
        "html" | "markdown" | "text" | "latex" => Some(TaskKind {
            renderer: backend.renderer,
            canonical: backend.canonical.clone(),
        }),
        _ => None,
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

/// Build a Send-only render task from the frame's already-materialized inputs.
pub(crate) fn build_task(
    frame: &GraphEvalFrame,
    plan: &ExecutionPlan,
    id: PlanNodeId,
    kind: TaskKind,
) -> Result<ParallelTask> {
    let children = plan.child_schedule(id).map_err(anyhow::Error::msg)?;
    let mut parts = Vec::new();
    for child in children {
        match &plan.nodes[child.0].kind {
            // Store children contribute only to the (unused for inline value
            // output) local scope.
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
    Ok(ParallelTask {
        renderer: kind.renderer,
        canonical: kind.canonical,
        parts,
        #[cfg(test)]
        overlap_probe: active_test_probe(),
    })
}

/// Compute one render task. Pure; safe to run on a worker thread.
fn render(task: &ParallelTask) -> Result<OValue> {
    #[cfg(test)]
    let _overlap_guard = task.overlap_probe.as_ref().map(TestOverlapProbe::enter);

    let mut buf = String::new();
    for part in &task.parts {
        match part {
            RenderPart::Verbatim(text) => buf.push_str(text),
            RenderPart::Splice(value) => buf.push_str(&render_with(task.renderer, value)),
        }
    }
    match task.canonical.as_str() {
        "html" => Ok(OValue::html(buf)),
        "markdown" | "text" | "latex" => Ok(OValue::str_(buf)),
        other => bail!("inline OIR backend `{other}` has no value executor"),
    }
}

/// Execute a batch of render tasks across worker threads, preserving input
/// order in the returned results.
pub(crate) fn execute(tasks: Vec<ParallelTask>) -> Vec<Result<OValue>> {
    if tasks.len() <= 1 {
        return tasks.iter().map(render).collect();
    }

    let mut results: Vec<Option<Result<OValue>>> = (0..tasks.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(tasks.len());
        for task in &tasks {
            handles.push(scope.spawn(move || render(task)));
        }
        for (slot, handle) in results.iter_mut().zip(handles) {
            *slot = Some(match handle.join() {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("parallel render worker panicked")),
            });
        }
    });
    results
        .into_iter()
        .map(|slot| slot.expect("every task slot is filled"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_runs_safe_renderers_with_real_worker_overlap() {
        let task_count = 4;
        let session = TestOverlapSession::begin(task_count);
        let tasks = (0..task_count)
            .map(|index| ParallelTask {
                renderer: SpliceRenderer::Default,
                canonical: "text".to_string(),
                parts: vec![RenderPart::Verbatim(format!("renderer-{index}"))],
                overlap_probe: Some(Arc::clone(&session.probe)),
            })
            .collect();

        let results = execute(tasks);

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
}
