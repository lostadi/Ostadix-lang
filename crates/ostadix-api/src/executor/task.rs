//! Owned task contract crossing from admission-aware preparation into a local
//! execution lane.
//!
//! A `PreparedTask` contains no evaluator borrow or mutable process-registry
//! state. The coordinator issues the opaque `TaskToken`; worker code cannot
//! choose or forge which admitted operation receives the result. Durable trace
//! and commit remain provisional until the coordinator settles them in
//! semantic order; a verified-pure infallible value may become visible earlier
//! only to equally safe worker dependents.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::time::{Duration, Instant};

use crate::value::OValue;
use anyhow::{anyhow, bail, Result};

/// Coordinator-issued identity for one submitted operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TaskToken(pub(crate) usize);

/// Driver-private identity for one physical attempt.
///
/// The graph executor deliberately knows only the stable execution/task
/// digests and monotonically increasing generation needed to fence stale
/// completions. Protocol adapters map their own authenticated coordinates to
/// this value; no wire type, node identity, graph coordinate, or `TaskToken`
/// crosses this lower-layer boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalAttemptCoordinateV1 {
    execution_sha256: [u8; 32],
    logical_task_sha256: [u8; 32],
    generation: u64,
}

impl PhysicalAttemptCoordinateV1 {
    pub(crate) fn new(
        execution_sha256: [u8; 32],
        logical_task_sha256: [u8; 32],
        generation: u64,
    ) -> Result<Self> {
        if execution_sha256 == [0; 32] {
            bail!("physical attempt execution digest must be nonzero");
        }
        if logical_task_sha256 == [0; 32] {
            bail!("physical attempt logical-task digest must be nonzero");
        }
        if generation == 0 {
            bail!("physical attempt generation must be nonzero");
        }
        Ok(Self {
            execution_sha256,
            logical_task_sha256,
            generation,
        })
    }

    pub(crate) fn generation(self) -> u64 {
        self.generation
    }

    pub(crate) fn same_logical_task(self, other: Self) -> bool {
        self.execution_sha256 == other.execution_sha256
            && self.logical_task_sha256 == other.logical_task_sha256
    }
}

/// An owned, `Send`-only computation prepared from already-materialized inputs.
///
/// Implementations do not materialize HGraph outputs, advance graph resource
/// state, or emit settlement trace events. Most adapters are effect-free. An
/// explicitly autonomous hosted adapter may perform unordered external effects
/// under its separately admitted non-strict contract.
pub(crate) trait PreparedTask: Send + 'static {
    fn execute(self: Box<Self>, context: &TaskContext) -> Result<OValue>;
}

/// Worker-side access to coordinator-owned services. The first supported
/// service is recursive `O.eval`: the hosted process stays on its worker while
/// the coordinator evaluates the quoted O source and sends the value back.
pub(crate) struct TaskContext {
    token: TaskToken,
    events: Sender<WorkerEvent>,
}

impl TaskContext {
    pub(crate) fn new(token: TaskToken, events: Sender<WorkerEvent>) -> Self {
        Self { token, events }
    }

    #[cfg(test)]
    pub(crate) fn eval_o_source(
        &self,
        src: String,
        scope: HashMap<String, OValue>,
    ) -> Result<OValue> {
        self.eval_o_source_with_timeout(src, scope, crate::process::backend_operation_timeout())
    }

    /// Request a coordinator-owned recursive evaluation while retaining the
    /// deadline of the physical backend operation that initiated it.
    pub(crate) fn eval_o_source_with_timeout(
        &self,
        src: String,
        scope: HashMap<String, OValue>,
        timeout: Duration,
    ) -> Result<OValue> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            crate::process::infrastructure_error(anyhow!(
                "local worker callback deadline overflowed"
            ))
        })?;
        let (reply, result) = mpsc::sync_channel(1);
        self.events
            .send(WorkerEvent::EvalRequest(TaskEvalRequest {
                token: self.token,
                src,
                scope,
                deadline,
                reply,
            }))
            .map_err(|_| {
                crate::process::infrastructure_error(anyhow!(
                    "local worker callback channel disconnected"
                ))
            })?;
        crate::process::lifecycle_trace(
            "worker.callback_requested",
            format!("token={}", self.token.0),
        );
        let result = result.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => crate::process::infrastructure_error(anyhow!(
                "local worker O.eval callback did not settle within {} ms",
                timeout.as_millis()
            )),
            mpsc::RecvTimeoutError::Disconnected => crate::process::infrastructure_error(anyhow!(
                "local worker callback reply channel disconnected"
            )),
        })?;
        crate::process::lifecycle_trace(
            "worker.callback_received",
            format!("token={} success={}", self.token.0, result.is_ok()),
        );
        match result {
            Ok(value) => Ok(value),
            Err(TaskCallbackFailure::Semantic(message)) => Err(anyhow!(message)),
            Err(TaskCallbackFailure::Infrastructure(message)) => {
                Err(crate::process::infrastructure_error(anyhow!(message)))
            }
        }
    }
}

/// One task submitted to a worker lane. The token is kept outside the task so
/// an adapter cannot redirect its result to another operation.
pub(crate) struct TaskSubmission {
    token: TaskToken,
    task: Box<dyn PreparedTask>,
    /// Present only for an externally coordinated physical attempt. The
    /// worker never interprets this neutral coordinate; its driver retains the
    /// coordinate-to-`TaskToken` map privately.
    physical_attempt: Option<PhysicalAttemptCoordinateV1>,
}

impl TaskSubmission {
    pub(crate) fn new(token: TaskToken, task: Box<dyn PreparedTask>) -> Self {
        Self {
            token,
            task,
            physical_attempt: None,
        }
    }

    pub(crate) fn physical(
        token: TaskToken,
        attempt: PhysicalAttemptCoordinateV1,
        task: Box<dyn PreparedTask>,
    ) -> Self {
        Self {
            token,
            task,
            physical_attempt: Some(attempt),
        }
    }

    pub(crate) fn token(&self) -> TaskToken {
        self.token
    }

    pub(crate) fn physical_attempt(&self) -> Option<PhysicalAttemptCoordinateV1> {
        self.physical_attempt
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        TaskToken,
        Option<PhysicalAttemptCoordinateV1>,
        Box<dyn PreparedTask>,
    ) {
        (self.token, self.physical_attempt, self.task)
    }
}

/// A physical worker completion. Its durable outcome is not semantically
/// settled before the ordered frontier, although a verified-pure infallible
/// value may provisionally unlock equally safe worker dependents.
pub(crate) struct TaskCompletion {
    pub(crate) token: TaskToken,
    /// Coordinator-private physical identity. It is retained only long enough
    /// for a remote driver to enforce gate 19 and is never serialized.
    pub(crate) physical_attempt: Option<PhysicalAttemptCoordinateV1>,
    pub(crate) outcome: TaskOutcome,
}

impl TaskCompletion {
    #[cfg(test)]
    pub(crate) fn completed(token: TaskToken, outcome: Result<OValue>) -> Self {
        Self {
            token,
            physical_attempt: None,
            outcome: TaskOutcome::Completed(outcome.map(Box::new)),
        }
    }

    #[cfg(test)]
    pub(crate) fn infrastructure_abort(token: TaskToken, error: anyhow::Error) -> Self {
        Self {
            token,
            physical_attempt: None,
            outcome: TaskOutcome::InfrastructureAbort(error),
        }
    }

    pub(crate) fn physical_attempt(&self) -> Option<PhysicalAttemptCoordinateV1> {
        self.physical_attempt
    }
}

/// Physical completion class. An adapter error is semantic only for an admitted
/// fallible operation; an error from an admitted-infallible adapter is a broken
/// execution contract and must never masquerade as `NodeFailed`.
pub(crate) enum TaskOutcome {
    Completed(Result<Box<OValue>>),
    InfrastructureAbort(anyhow::Error),
}

/// One physical event from a worker. Callback requests do not free a worker
/// slot; only `Completion` decrements the pool's outstanding-task count.
pub(crate) enum WorkerEvent {
    Completion(TaskCompletion),
    EvalRequest(TaskEvalRequest),
}

pub(crate) struct TaskEvalRequest {
    pub(crate) token: TaskToken,
    pub(crate) src: String,
    pub(crate) scope: HashMap<String, OValue>,
    pub(crate) deadline: Instant,
    reply: SyncSender<std::result::Result<OValue, TaskCallbackFailure>>,
}

impl TaskEvalRequest {
    pub(crate) fn respond(
        self,
        result: std::result::Result<OValue, TaskCallbackFailure>,
    ) -> Result<()> {
        self.reply
            .send(result)
            .map_err(|_| anyhow!("local worker abandoned an O.eval callback reply"))
    }
}

pub(crate) enum TaskCallbackFailure {
    Semantic(String),
    Infrastructure(String),
}
