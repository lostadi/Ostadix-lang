//! Owned task contract crossing from admission-aware preparation into a local
//! execution lane.
//!
//! A [`PreparedTask`] contains no evaluator borrow or mutable process-registry
//! state. The coordinator issues the opaque [`TaskToken`]; worker code cannot
//! choose or forge which admitted operation receives the result. Durable trace
//! and commit remain provisional until the coordinator settles them in
//! semantic order; a verified-pure infallible value may become visible earlier
//! only to equally safe worker dependents.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender, SyncSender};

use anyhow::{anyhow, Result};

use crate::value::OValue;

/// Coordinator-issued identity for one submitted operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TaskToken(pub(crate) usize);

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

    pub(crate) fn eval_o_source(
        &self,
        src: String,
        scope: HashMap<String, OValue>,
    ) -> Result<OValue> {
        let (reply, result) = mpsc::sync_channel(1);
        self.events
            .send(WorkerEvent::EvalRequest(TaskEvalRequest {
                token: self.token,
                src,
                scope,
                reply,
            }))
            .map_err(|_| anyhow!("local worker callback channel disconnected"))?;
        result
            .recv()
            .map_err(|_| anyhow!("local worker callback reply channel disconnected"))?
            .map_err(anyhow::Error::msg)
    }
}

/// One task submitted to a worker lane. The token is kept outside the task so
/// an adapter cannot redirect its result to another operation.
pub(crate) struct TaskSubmission {
    token: TaskToken,
    task: Box<dyn PreparedTask>,
}

impl TaskSubmission {
    pub(crate) fn new(token: TaskToken, task: Box<dyn PreparedTask>) -> Self {
        Self { token, task }
    }

    pub(crate) fn into_parts(self) -> (TaskToken, Box<dyn PreparedTask>) {
        (self.token, self.task)
    }
}

/// A physical worker completion. Its durable outcome is not semantically
/// settled before the ordered frontier, although a verified-pure infallible
/// value may provisionally unlock equally safe worker dependents.
pub(crate) struct TaskCompletion {
    pub(crate) token: TaskToken,
    pub(crate) outcome: TaskOutcome,
}

impl TaskCompletion {
    pub(crate) fn completed(token: TaskToken, outcome: Result<OValue>) -> Self {
        Self {
            token,
            outcome: TaskOutcome::Completed(outcome.map(Box::new)),
        }
    }

    pub(crate) fn infrastructure_abort(token: TaskToken, error: anyhow::Error) -> Self {
        Self {
            token,
            outcome: TaskOutcome::InfrastructureAbort(error),
        }
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
    reply: SyncSender<std::result::Result<OValue, String>>,
}

impl TaskEvalRequest {
    pub(crate) fn respond(self, result: std::result::Result<OValue, String>) -> Result<()> {
        self.reply
            .send(result)
            .map_err(|_| anyhow!("local worker abandoned an O.eval callback reply"))
    }
}
