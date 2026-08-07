//! Owned task contract crossing from admission-aware preparation into a local
//! execution lane.
//!
//! A [`PreparedTask`] contains no evaluator borrow or mutable process-registry
//! state. The coordinator issues the opaque [`TaskToken`]; worker code cannot
//! choose or forge which admitted operation receives the result. Completion is
//! provisional until the coordinator settles it in semantic order.

use anyhow::Result;

use crate::value::OValue;

/// Coordinator-issued identity for one submitted operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TaskToken(pub(crate) usize);

/// An owned, `Send`-only computation prepared from already-materialized inputs.
///
/// Implementations perform computation only. They do not materialize HGraph
/// outputs, advance resource state, emit settlement trace events, or commit
/// externally visible effects.
pub(crate) trait PreparedTask: Send + 'static {
    fn execute(self: Box<Self>) -> Result<OValue>;
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

/// A physical worker completion. Its outcome is not semantically visible
/// until the coordinator accepts it at the ordered settlement frontier.
pub(crate) struct TaskCompletion {
    pub(crate) token: TaskToken,
    pub(crate) outcome: TaskOutcome,
}

impl TaskCompletion {
    pub(crate) fn completed(token: TaskToken, outcome: Result<OValue>) -> Self {
        Self {
            token,
            outcome: TaskOutcome::Completed(outcome),
        }
    }

    pub(crate) fn infrastructure_abort(token: TaskToken, error: anyhow::Error) -> Self {
        Self {
            token,
            outcome: TaskOutcome::InfrastructureAbort(error),
        }
    }
}

/// Physical completion class. Adapter-returned errors are semantic outcomes;
/// a broken worker execution mechanism must never masquerade as `NodeFailed`.
pub(crate) enum TaskOutcome {
    Completed(Result<OValue>),
    InfrastructureAbort(anyhow::Error),
}
