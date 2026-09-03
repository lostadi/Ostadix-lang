//! Fixed-size persistent local-worker pool.
//!
//! By default, workers live for one graph-coordinator execution and are reused
//! across changing HGraph readiness frontiers. Embeddings may explicitly retain
//! an idle pool across sequential executions. Each completion is delivered
//! independently; this module does not batch, order, or settle results.

use std::sync::{
    mpsc::{self, Receiver, SyncSender, TryRecvError},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};

use anyhow::{anyhow, bail, Context, Result};

use super::task::{TaskCompletion, TaskContext, TaskSubmission, WorkerEvent};

/// A bounded pool of persistent local workers.
pub(crate) struct WorkerPool {
    capacity: usize,
    creator_affinity: Option<Vec<u8>>,
    outstanding: usize,
    submissions: Option<SyncSender<TaskSubmission>>,
    events: Receiver<WorkerEvent>,
    workers: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    pub(crate) fn new(capacity: usize) -> Result<Self> {
        if capacity == 0 {
            bail!("local worker pool capacity must be at least one");
        }

        let (submission_tx, submission_rx) = mpsc::sync_channel(capacity);
        let submission_rx = Arc::new(Mutex::new(submission_rx));
        let (event_tx, event_rx) = mpsc::channel();
        let mut workers = Vec::with_capacity(capacity);
        let creator_affinity = current_thread_affinity();

        for index in 0..capacity {
            let submissions = Arc::clone(&submission_rx);
            let events = event_tx.clone();
            let spawn = thread::Builder::new()
                .name(format!("ostadix-local-worker-{index}"))
                .spawn(move || worker_loop(submissions, events));
            match spawn {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    drop(submission_tx);
                    join_workers(workers);
                    return Err(error).context("failed to create local worker pool");
                }
            }
        }
        drop(event_tx);

        Ok(Self {
            capacity,
            creator_affinity,
            outstanding: 0,
            submissions: Some(submission_tx),
            events: event_rx,
            workers,
        })
    }

    pub(crate) fn outstanding(&self) -> usize {
        self.outstanding
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Worker threads inherit the creator's Linux/Android affinity mask.
    /// Retained pools are reusable only while the calling thread still has
    /// that exact mask; a CPU-policy or cpuset change must create new workers.
    pub(crate) fn matches_current_affinity(&self) -> bool {
        #[cfg(any(target_os = "android", target_os = "linux"))]
        {
            self.creator_affinity
                .as_ref()
                .zip(current_thread_affinity().as_ref())
                .is_some_and(|(created, current)| created == current)
        }
        #[cfg(not(any(target_os = "android", target_os = "linux")))]
        {
            self.creator_affinity.is_none()
        }
    }

    pub(crate) fn available_slots(&self) -> usize {
        self.capacity - self.outstanding
    }

    /// Submit one coordinator-prepared task without exceeding the fixed
    /// in-flight bound. The coordinator emits `Started` only after this call
    /// succeeds.
    pub(crate) fn submit(&mut self, submission: TaskSubmission) -> Result<()> {
        if self.outstanding == self.capacity {
            bail!("local worker pool is at capacity ({})", self.capacity);
        }
        self.submissions
            .as_ref()
            .ok_or_else(|| anyhow!("local worker pool is shut down"))?
            .send(submission)
            .map_err(|_| anyhow!("local worker pool disconnected before task submission"))?;
        self.outstanding += 1;
        Ok(())
    }

    /// Wait for one physical worker event. Callback requests keep the worker
    /// outstanding; only a completion returns capacity to the pool.
    pub(crate) fn recv_event(&mut self) -> Result<WorkerEvent> {
        let event = self
            .events
            .recv()
            .map_err(|_| anyhow!("local worker pool disconnected with tasks outstanding"))?;
        if matches!(event, WorkerEvent::Completion(_)) {
            self.accept_completion()?;
        }
        Ok(event)
    }

    /// Receive one already-available worker event without blocking.
    pub(crate) fn try_recv_event(&mut self) -> Result<Option<WorkerEvent>> {
        match self.events.try_recv() {
            Ok(event) => {
                if matches!(event, WorkerEvent::Completion(_)) {
                    self.accept_completion()?;
                }
                Ok(Some(event))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) if self.outstanding == 0 => Ok(None),
            Err(TryRecvError::Disconnected) => {
                bail!("local worker pool disconnected with tasks outstanding")
            }
        }
    }

    /// Completion-only convenience used by effect-free adapter and pool tests.
    /// Interactive tasks must be driven through `recv_event` by the coordinator.
    #[cfg(test)]
    pub(crate) fn recv_completion(&mut self) -> Result<TaskCompletion> {
        match self.recv_event()? {
            WorkerEvent::Completion(completion) => Ok(completion),
            WorkerEvent::EvalRequest(request) => {
                request.respond(Err(super::task::TaskCallbackFailure::Infrastructure(
                    "O.eval callback requires the graph coordinator".to_string(),
                )))?;
                bail!("unexpected O.eval callback in completion-only worker consumer")
            }
        }
    }

    fn accept_completion(&mut self) -> Result<()> {
        if self.outstanding == 0 {
            bail!("local worker pool produced an unexpected completion");
        }
        self.outstanding -= 1;
        Ok(())
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn current_thread_affinity() -> Option<Vec<u8>> {
    // SAFETY: the value is zero-initialized, the kernel receives its exact
    // allocation size, pid 0 selects the calling thread, and the immutable
    // byte view does not outlive `set`.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        let size = std::mem::size_of::<libc::cpu_set_t>();
        if libc::sched_getaffinity(0, size, &mut set) != 0 {
            return None;
        }
        Some(std::slice::from_raw_parts((&set as *const libc::cpu_set_t).cast(), size).to_vec())
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn current_thread_affinity() -> Option<Vec<u8>> {
    None
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        // Disconnect the queue before joining so idle workers leave recv().
        self.submissions.take();
        crate::process::lifecycle_trace("pool.submission_channel_closed", "");
        join_workers(self.workers.drain(..));
    }
}

fn join_workers(workers: impl IntoIterator<Item = JoinHandle<()>>) {
    for (index, worker) in workers.into_iter().enumerate() {
        let _ = worker.join();
        crate::process::lifecycle_trace("pool.worker_joined", format!("worker={index}"));
    }
}

fn worker_loop(
    submissions: Arc<Mutex<Receiver<TaskSubmission>>>,
    events: mpsc::Sender<WorkerEvent>,
) {
    loop {
        let submission = submissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv();
        let Ok(submission) = submission else {
            return;
        };
        let (token, physical_attempt, task) = submission.into_parts();
        crate::process::lifecycle_trace("worker.task_received", format!("token={}", token.0));
        let context = TaskContext::new(token, events.clone());
        // This converts panics only in unwind-capable profiles. A panic-abort
        // build terminates the process before Rust can produce a completion.
        let outcome =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task.execute(&context)))
            {
                Ok(Err(error)) if crate::process::is_infrastructure_error(&error) => {
                    super::task::TaskOutcome::InfrastructureAbort(error)
                }
                Ok(outcome) => super::task::TaskOutcome::Completed(outcome.map(Box::new)),
                Err(_) => super::task::TaskOutcome::InfrastructureAbort(anyhow!(
                    "prepared local-worker task panicked"
                )),
            };
        let completion = TaskCompletion {
            token,
            physical_attempt,
            outcome,
        };
        if events.send(WorkerEvent::Completion(completion)).is_err() {
            return;
        }
        crate::process::lifecycle_trace("worker.completion_emitted", format!("token={}", token.0));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex};

    use super::*;
    use crate::executor::task::{PreparedTask, TaskOutcome, TaskToken};
    use crate::value::OValue;

    struct RecordingTask {
        threads: Arc<Mutex<Vec<thread::ThreadId>>>,
        value: &'static str,
    }

    impl PreparedTask for RecordingTask {
        fn execute(self: Box<Self>, _context: &TaskContext) -> Result<OValue> {
            self.threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(thread::current().id());
            Ok(OValue::str_(self.value))
        }
    }

    struct BlockingTask {
        gate: Arc<(Mutex<bool>, Condvar)>,
    }

    impl PreparedTask for BlockingTask {
        fn execute(self: Box<Self>, _context: &TaskContext) -> Result<OValue> {
            let (released, changed) = &*self.gate;
            let released = released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let released = changed
                .wait_while(released, |released| !*released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            debug_assert!(*released);
            Ok(OValue::str_("slow"))
        }
    }

    struct PanicTask;

    impl PreparedTask for PanicTask {
        fn execute(self: Box<Self>, _context: &TaskContext) -> Result<OValue> {
            panic!("test worker panic")
        }
    }

    struct InfrastructureErrorTask;

    impl PreparedTask for InfrastructureErrorTask {
        fn execute(self: Box<Self>, _context: &TaskContext) -> Result<OValue> {
            Err(crate::process::infrastructure_error(anyhow!(
                "backend teardown failed"
            )))
        }
    }

    #[test]
    fn pool_rejects_zero_capacity() {
        assert!(WorkerPool::new(0).is_err());
    }

    #[test]
    fn new_pool_matches_its_creator_affinity() {
        let pool = WorkerPool::new(1).unwrap();
        assert!(pool.matches_current_affinity());
    }

    #[test]
    fn one_worker_is_reused_across_dispatch_frontiers() {
        let owner = thread::current().id();
        let threads = Arc::new(Mutex::new(Vec::new()));
        let mut pool = WorkerPool::new(1).unwrap();

        for (token, value) in [(0, "first"), (1, "second")] {
            pool.submit(TaskSubmission::new(
                TaskToken(token),
                Box::new(RecordingTask {
                    threads: Arc::clone(&threads),
                    value,
                }),
            ))
            .unwrap();
            let completion = pool.recv_completion().unwrap();
            assert_eq!(completion.token, TaskToken(token));
            assert!(matches!(completion.outcome, TaskOutcome::Completed(Ok(_))));
        }

        let threads = threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0], threads[1], "the worker was not reused");
        assert_ne!(threads[0], owner, "task ran on the coordinator thread");
    }

    #[test]
    fn completions_arrive_individually_without_a_wave_barrier() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let mut pool = WorkerPool::new(2).unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(0),
            Box::new(BlockingTask {
                gate: Arc::clone(&gate),
            }),
        ))
        .unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(1),
            Box::new(RecordingTask {
                threads,
                value: "fast",
            }),
        ))
        .unwrap();

        let first = pool.recv_completion().unwrap();
        assert_eq!(first.token, TaskToken(1));
        assert_eq!(pool.outstanding(), 1);

        let (released, changed) = &*gate;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
        assert_eq!(pool.recv_completion().unwrap().token, TaskToken(0));
    }

    #[test]
    fn pool_enforces_its_outstanding_bound() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let mut pool = WorkerPool::new(1).unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(0),
            Box::new(BlockingTask {
                gate: Arc::clone(&gate),
            }),
        ))
        .unwrap();
        let error = pool
            .submit(TaskSubmission::new(TaskToken(1), Box::new(PanicTask)))
            .expect_err("a second outstanding task exceeds capacity");
        assert!(error.to_string().contains("at capacity"));

        let (released, changed) = &*gate;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
        pool.recv_completion().unwrap();
    }

    #[test]
    fn task_panic_becomes_an_infrastructure_completion_and_the_worker_survives() {
        let threads = Arc::new(Mutex::new(Vec::new()));
        let mut pool = WorkerPool::new(1).unwrap();
        pool.submit(TaskSubmission::new(TaskToken(0), Box::new(PanicTask)))
            .unwrap();
        let panic = pool.recv_completion().unwrap();
        let TaskOutcome::InfrastructureAbort(error) = panic.outcome else {
            panic!("worker panic must not become an ordinary task outcome")
        };
        assert!(error.to_string().contains("panicked"));

        pool.submit(TaskSubmission::new(
            TaskToken(1),
            Box::new(RecordingTask {
                threads,
                value: "after-panic",
            }),
        ))
        .unwrap();
        assert!(matches!(
            pool.recv_completion().unwrap().outcome,
            TaskOutcome::Completed(Ok(_))
        ));
    }

    #[test]
    fn typed_backend_lifecycle_error_remains_infrastructure() {
        let mut pool = WorkerPool::new(1).unwrap();
        pool.submit(TaskSubmission::new(
            TaskToken(0),
            Box::new(InfrastructureErrorTask),
        ))
        .unwrap();

        let completion = pool.recv_completion().unwrap();
        let TaskOutcome::InfrastructureAbort(error) = completion.outcome else {
            panic!("typed backend lifecycle fault became a semantic completion")
        };
        assert!(error.to_string().contains("backend teardown failed"));
    }
}
