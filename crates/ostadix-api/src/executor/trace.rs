//! Trace helpers for the graph coordinator.
//!
//! The canonical trace types (`ExecutionTrace` / `TraceEvent`) live in the
//! evaluator-independent execution core. The evaluator publicly reexports the
//! exact types so existing trace-dependent embedders keep observing the same
//! `NodeReady` / `NodeStarted` / `NodeFinished` / `NodeFailed` events.

use crate::eval_core::{ExecutionTrace, TraceEvent};
use crate::ir::PlanNodeId;

/// A thin sink that appends coordinator lifecycle events to an
/// [`ExecutionTrace`].
#[derive(Debug, Default)]
pub struct TraceSink {
    trace: ExecutionTrace,
}

impl TraceSink {
    pub(crate) fn crossing_submitted(
        &mut self,
        observation: crate::backend_morphism::BackendCrossingObservationV1,
    ) {
        self.trace.backend_crossings.push(observation);
    }

    pub(crate) fn crossing_mut(
        &mut self,
        id: PlanNodeId,
    ) -> Option<&mut crate::backend_morphism::BackendCrossingObservationV1> {
        self.trace
            .backend_crossings
            .iter_mut()
            .find(|observation| observation.plan_node == id.0)
    }

    pub fn new() -> Self {
        Self {
            trace: ExecutionTrace::new(),
        }
    }

    pub fn ready(&mut self, id: PlanNodeId) {
        self.trace.events.push(TraceEvent::NodeReady(id));
    }

    pub fn started(&mut self, id: PlanNodeId) {
        self.trace.events.push(TraceEvent::NodeStarted(id));
    }

    pub fn finished(&mut self, id: PlanNodeId, value_type: String, fingerprint: Option<String>) {
        if let Some(crossing) = self.crossing_mut(id) {
            crossing.published = crossing.result.is_some();
        }
        self.trace.events.push(TraceEvent::NodeFinished {
            id,
            value_type,
            fingerprint,
        });
    }

    pub fn failed(&mut self, id: PlanNodeId, message: String) {
        self.trace
            .events
            .push(TraceEvent::NodeFailed { id, message });
    }

    pub fn discarded(&mut self, id: PlanNodeId, reason: String) {
        if let Some(crossing) = self.crossing_mut(id) {
            crossing.discarded = true;
            crossing.published = false;
        }
        self.trace
            .events
            .push(TraceEvent::NodeDiscarded { id, reason });
    }

    /// Consume the sink, yielding the accumulated trace.
    pub fn into_trace(self) -> ExecutionTrace {
        self.trace
    }
}
