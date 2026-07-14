//! Trace helpers for the graph coordinator.
//!
//! The canonical trace types (`ExecutionTrace` / `TraceEvent`) live in the
//! evaluator so that existing trace-dependent tests keep observing the same
//! `NodeReady` / `NodeStarted` / `NodeFinished` / `NodeFailed` events. This
//! module provides small helpers for emitting those events from the
//! coordinator's event loop.

use crate::eval::{ExecutionTrace, TraceEvent};
use crate::ir::PlanNodeId;

/// A thin sink that appends coordinator lifecycle events to an
/// [`ExecutionTrace`].
#[derive(Debug, Default)]
pub struct TraceSink {
    trace: ExecutionTrace,
}

impl TraceSink {
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

    /// Consume the sink, yielding the accumulated trace.
    pub fn into_trace(self) -> ExecutionTrace {
        self.trace
    }
}
