//! Crate-private execution-attempt driver boundary.
//!
//! The coordinator owns readiness, dispatch selection, publication, and
//! settlement. A driver only provides the physical submission/event surface
//! already implemented by the persistent local-worker pool.

use anyhow::{bail, Result};

use crate::eval_core::GraphEvalFrame;
use crate::evidence::AdmittedExecution;
use crate::ir::{ExecutionPlan, OIr, PlanNodeId};

use super::pool::WorkerPool;
use super::task::{PhysicalAttemptCoordinateV1, PreparedTask, TaskSubmission, WorkerEvent};

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

/// One protocol-neutral physical attempt prepared from admitted graph state.
/// The concrete adapter retains all authentication and wire semantics above
/// the executor root; the coordinator receives only an owned task plus the
/// minimal coordinate needed for stale-completion fencing.
pub(crate) struct PreparedPhysicalAttemptV1 {
    coordinate: PhysicalAttemptCoordinateV1,
    task: Box<dyn PreparedTask>,
}

impl PreparedPhysicalAttemptV1 {
    pub(crate) fn new(
        coordinate: PhysicalAttemptCoordinateV1,
        task: Box<dyn PreparedTask>,
    ) -> Self {
        Self { coordinate, task }
    }

    pub(crate) fn into_parts(self) -> (PhysicalAttemptCoordinateV1, Box<dyn PreparedTask>) {
        (self.coordinate, self.task)
    }
}

/// High-layer policy injection for an explicitly selected physical-attempt
/// realization. The executor owns graph scheduling and the five-method driver
/// seam; a concrete adapter owns protocol authorization, preparation, and
/// candidate validation without introducing an upward dependency.
pub(crate) trait PhysicalAttemptAdapterV1: Send + Sync {
    fn create_driver(&self) -> Result<Box<dyn AttemptDriver>>;

    fn prepare_attempt(
        &self,
        admitted: &AdmittedExecution<'_>,
        frame: &GraphEvalFrame,
        flat: &[&OIr],
        plan: &ExecutionPlan,
        id: PlanNodeId,
    ) -> Result<PreparedPhysicalAttemptV1>;
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
        if submission.physical_attempt().is_some() {
            bail!("local worker driver rejected an externally coordinated physical attempt");
        }
        self.pool.submit(submission)
    }

    fn try_recv_event(&mut self) -> Result<Option<WorkerEvent>> {
        self.pool.try_recv_event()
    }

    fn recv_event(&mut self) -> Result<WorkerEvent> {
        self.pool.recv_event()
    }
}
