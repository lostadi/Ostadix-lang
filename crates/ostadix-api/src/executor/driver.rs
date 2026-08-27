//! Crate-private execution-attempt driver boundary.
//!
//! The coordinator owns readiness, dispatch selection, publication, and
//! settlement. A driver only provides the physical submission/event surface
//! already implemented by the persistent local-worker pool.

use anyhow::Result;

use super::pool::WorkerPool;
use super::task::{TaskSubmission, WorkerEvent};

/// Physical attempt transport used beneath the graph coordinator.
///
/// This deliberately mirrors the existing worker-pool surface. Cancellation,
/// leases, retries, remote placement, and protocol concerns remain outside M1.
pub(crate) trait AttemptDriver {
    fn available_slots(&self) -> usize;
    fn outstanding(&self) -> usize;
    fn submit(&mut self, submission: TaskSubmission) -> Result<()>;
    fn try_recv_event(&mut self) -> Result<Option<WorkerEvent>>;
    fn recv_event(&mut self) -> Result<WorkerEvent>;
}

/// Exact local adapter over the existing persistent worker pool.
pub(crate) struct LocalWorkerDriver {
    pool: WorkerPool,
}

impl LocalWorkerDriver {
    pub(crate) fn new(capacity: usize) -> Result<Self> {
        Ok(Self {
            pool: WorkerPool::new(capacity)?,
        })
    }
}

impl AttemptDriver for LocalWorkerDriver {
    fn available_slots(&self) -> usize {
        self.pool.available_slots()
    }

    fn outstanding(&self) -> usize {
        self.pool.outstanding()
    }

    fn submit(&mut self, submission: TaskSubmission) -> Result<()> {
        self.pool.submit(submission)
    }

    fn try_recv_event(&mut self) -> Result<Option<WorkerEvent>> {
        self.pool.try_recv_event()
    }

    fn recv_event(&mut self) -> Result<WorkerEvent> {
        self.pool.recv_event()
    }
}
