//! Owned task contract crossing from admission-aware preparation into a local
//! execution lane.
//!
//! A [`PreparedTask`] contains no evaluator borrow or mutable process-registry
//! state. The coordinator issues the opaque [`TaskToken`]; worker code cannot
//! choose or forge which admitted operation receives the result. Durable trace
//! and commit remain provisional until the coordinator settles them in
//! semantic order; a verified-pure infallible value may become visible earlier
//! only to equally safe worker dependents.

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
