use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, RwLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use thiserror::Error;

use crate::backend::state::{BackendStateTierV1, EvaluatorStateSnapshotV1};
use crate::environment::EnvironmentRefV2;
use crate::eval::{
    Evaluator, PreparedPlacementDeadlineExpiredV1, PreparedPlacementFragmentV1,
    PreparedPlacementRefusalV1,
};
use crate::ir::BackendRegistry;
use crate::placement::{
    ActorGenerationIdV1, BackendStateSupportV2, CanonicalPlacementRecordV1, GenerationV1,
    SemanticDigestV1, SnapshotCompatibilityV2, StateQuotaLimitsV2, StateReservationV2,
    StateSessionIdV2, TaskAttemptIdV1,
};
use crate::runtime_exec::validate_native_runtime_binary;
use crate::value::OValue;

use super::super::protocol::{
    canonical_hosted_bytes, canonical_hosted_sha256, truncate_hosted_error_message, unix_time_ms,
};
use super::auth::{
    AuthorizedPlacementV2, PlacementAuthorizationContextV2, SharedPlacementAuthorizerV2,
};
use super::crypto::{constant_time_eq, decode_fixed_hex, salted_bearer_hash};
use super::protocol::*;
use super::store::{DurableSessionStoreV2, DurableStoreReopenRequiredV2, JournalReadV2};

const TERMINAL_RECORD_OVERHEAD_RESERVATION: u64 = 64 * 1024;
const SESSION_CLOSE_HEADROOM_RESERVATION: u64 = 64 * 1024;
const ACTOR_FENCE_HEADROOM_RESERVATION: u64 = 64 * 1024;
const RECOVERY_TERMINAL_HEADROOM_RESERVATION: u64 = 64 * 1024;
const BACKEND_STATE_CODEC_NAME_DOMAIN_V2: &str = "ostadix/backend-state-codec-name/v2";

#[derive(Debug, Clone)]
pub struct HostedV2RuntimeConfig {
    pub node_id: String,
    pub node_generation: GenerationV1,
    pub shim_dir: PathBuf,
    pub runtime_executable: PathBuf,
    pub state_quota_generation: GenerationV1,
    pub state_quotas: StateQuotaLimitsV2,
}

#[derive(Clone)]
pub struct HostedV2Runtime {
    inner: Arc<RuntimeInnerV2>,
}

/// Stable direct-call error returned once explicit runtime shutdown begins.
/// Hosted wire callers receive the matching non-retryable `runtime-closed`
/// protocol error instead.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("hosted V2 runtime is closed")]
pub struct HostedV2RuntimeClosedV2;

/// Explicit shutdown completed and released the durable store, but at least
/// one owned actor thread panicked while being drained.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("hosted V2 runtime shutdown completed with worker failures: {message}")]
pub struct HostedV2RuntimeShutdownErrorV2 {
    message: String,
}

impl HostedV2RuntimeShutdownErrorV2 {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Debug for HostedV2Runtime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostedV2Runtime")
            .field("node_id", &self.inner.config.node_id)
            .field("store", &self.inner.store)
            .finish_non_exhaustive()
    }
}

struct RuntimeInnerV2 {
    config: HostedV2RuntimeConfig,
    store: RuntimeStoreV2,
    authorizer: SharedPlacementAuthorizerV2,
    state: Mutex<RuntimeStateV2>,
    lifecycle: Mutex<RuntimeLifecycleV2>,
    lifecycle_changed: Condvar,
    worker_lifecycles: Mutex<Vec<Arc<ActorWorkerLifecycleV2>>>,
    #[cfg(debug_assertions)]
    current_view_prelock_barrier_for_test:
        Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
}

#[derive(Default)]
struct RuntimeStateV2 {
    sessions: HashMap<String, SessionRecordV2>,
    workers: HashMap<String, Arc<ActorWorkerV2>>,
    retired_session_ids: HashSet<String>,
    used_lease_nonces: HashSet<String>,
    durable_bytes: u64,
    reserved_durable_bytes: u64,
    state_bytes_reserved: u64,
    authority_control_headroom_bytes: u64,
    authority_journal_sequence: u64,
    authority_journal_head_sha256: Option<String>,
    unreadable_sessions: Vec<String>,
    #[cfg(debug_assertions)]
    close_actor_before_execute_for_test: HashSet<String>,
    #[cfg(debug_assertions)]
    force_checkpoint_failure_for_test: HashSet<String>,
    #[cfg(debug_assertions)]
    checkpoint_failure_terminal_barrier_for_test:
        HashMap<String, (Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimePhaseV2 {
    Running,
    ShuttingDown,
    Closed,
}

struct RuntimeLifecycleV2 {
    phase: RuntimePhaseV2,
    active_calls: usize,
    shutdown_error: Option<HostedV2RuntimeShutdownErrorV2>,
}

impl Default for RuntimeLifecycleV2 {
    fn default() -> Self {
        Self {
            phase: RuntimePhaseV2::Running,
            active_calls: 0,
            shutdown_error: None,
        }
    }
}

struct RuntimeCallGuardV2<'a> {
    inner: &'a RuntimeInnerV2,
}

impl Drop for RuntimeCallGuardV2<'_> {
    fn drop(&mut self) {
        let mut lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lifecycle.active_calls = lifecycle.active_calls.saturating_sub(1);
        if lifecycle.active_calls == 0 {
            self.inner.lifecycle_changed.notify_all();
        }
    }
}

struct RuntimeStoreV2 {
    store: RwLock<Option<DurableSessionStoreV2>>,
}

impl std::fmt::Debug for RuntimeStoreV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let store = self
            .store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        formatter
            .debug_struct("RuntimeStoreV2")
            .field("open", &store.is_some())
            .field("store", &store.as_ref())
            .finish()
    }
}

impl RuntimeStoreV2 {
    fn new(store: DurableSessionStoreV2) -> Self {
        Self {
            store: RwLock::new(Some(store)),
        }
    }

    fn with_store<T>(
        &self,
        operation: impl FnOnce(&DurableSessionStoreV2) -> Result<T>,
    ) -> Result<T> {
        let store = self
            .store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let store = store
            .as_ref()
            .ok_or_else(|| anyhow::Error::new(HostedV2RuntimeClosedV2))?;
        operation(store)
    }

    fn take(&self) -> Option<DurableSessionStoreV2> {
        self.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn is_reopen_required(&self) -> Result<bool> {
        self.with_store(|store| Ok(store.is_reopen_required()))
    }

    fn issue_journal_entry(&self, entry: JournalEntryV2) -> Result<SignedJournalEntryV2> {
        self.with_store(|store| store.signer().issue_journal_entry(entry))
    }

    fn encoded_frame_bytes<T: serde::Serialize>(&self, value: &T) -> Result<u64> {
        self.with_store(|store| store.encoded_frame_bytes(value))
    }

    fn install_session(&self, session_id: &str, entry: &SignedJournalEntryV2) -> Result<u64> {
        self.with_store(|store| store.install_session(session_id, entry))
    }

    fn append_entry(&self, session_id: &str, entry: &SignedJournalEntryV2) -> Result<u64> {
        self.with_store(|store| store.append_entry(session_id, entry))
    }

    fn append_authority_entry(&self, entry: &SignedJournalEntryV2) -> Result<u64> {
        self.with_store(|store| store.append_authority_entry(entry))
    }

    fn operation_new_bytes(
        &self,
        session_id: &str,
        operation: &PreparedOperationV2,
    ) -> Result<u64> {
        self.with_store(|store| store.operation_new_bytes(session_id, operation))
    }

    fn write_operation(&self, session_id: &str, operation: &PreparedOperationV2) -> Result<u64> {
        self.with_store(|store| store.write_operation(session_id, operation))
    }

    fn read_operation(&self, session_id: &str, operation_id: &str) -> Result<PreparedOperationV2> {
        self.with_store(|store| store.read_operation(session_id, operation_id))
    }

    fn checkpoint_new_bytes(
        &self,
        session_id: &str,
        actor_generation_sha256: &str,
        snapshot: &EvaluatorStateSnapshotV1,
        max_snapshot_payload_bytes: u64,
    ) -> Result<u64> {
        self.with_store(|store| {
            store.checkpoint_new_bytes(
                session_id,
                actor_generation_sha256,
                snapshot,
                max_snapshot_payload_bytes,
            )
        })
    }

    fn write_checkpoint(
        &self,
        session_id: &str,
        actor_generation_sha256: &str,
        snapshot: &EvaluatorStateSnapshotV1,
        max_snapshot_payload_bytes: u64,
    ) -> Result<u64> {
        self.with_store(|store| {
            store.write_checkpoint(
                session_id,
                actor_generation_sha256,
                snapshot,
                max_snapshot_payload_bytes,
            )
        })
    }

    fn read_checkpoint(
        &self,
        session_id: &str,
        actor_generation_sha256: &str,
        expected_snapshot_payload_bytes: u64,
    ) -> Result<EvaluatorStateSnapshotV1> {
        self.with_store(|store| {
            store.read_checkpoint(
                session_id,
                actor_generation_sha256,
                expected_snapshot_payload_bytes,
            )
        })
    }

    fn read_authority_journal(&self) -> Result<JournalReadV2> {
        self.with_store(DurableSessionStoreV2::read_authority_journal)
    }

    fn list_session_ids(&self) -> Result<Vec<String>> {
        self.with_store(DurableSessionStoreV2::list_session_ids)
    }

    fn read_closed_session_gc_archive(&self, event: &JournalEventV2) -> Result<JournalReadV2> {
        self.with_store(|store| store.read_closed_session_gc_archive(event))
    }

    fn read_journal(&self, session_id: &str) -> Result<JournalReadV2> {
        self.with_store(|store| store.read_journal(session_id))
    }

    fn session_durable_bytes(&self, session_id: &str) -> Result<u64> {
        self.with_store(|store| store.session_durable_bytes(session_id))
    }
}

struct ActorWorkerV2 {
    sender: mpsc::Sender<ActorCommandV2>,
    lifecycle: Arc<ActorWorkerLifecycleV2>,
}

struct ActorWorkerLifecycleV2 {
    session_id: String,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

impl ActorWorkerV2 {
    fn send(&self, command: ActorCommandV2) -> std::result::Result<(), ()> {
        self.sender.send(command).map_err(|_| ())
    }

    fn request_close(&self) {
        let _ = self.sender.send(ActorCommandV2::Close);
    }

    fn join(&self) -> Option<thread::Result<()>> {
        self.lifecycle.join()
    }
}

impl ActorWorkerLifecycleV2 {
    fn join(&self) -> Option<thread::Result<()>> {
        self.join
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(thread::JoinHandle::join)
    }
}

struct SessionRecordV2 {
    session_id: String,
    node_id: String,
    principal_sha256: String,
    bearer_salt: [u8; 32],
    bearer_hash: String,
    open_capability_commitment: crate::placement::SemanticDigestV1,
    open_request_sha256: String,
    open_placement_lease_sha256: String,
    open_placement_lease_nonce: String,
    open_client_request_id: String,
    open_receipt: SignedJournalEntryV2,
    state_tier: SessionStateTierV2,
    state_session: StateSessionIdV2,
    state_quota_generation: GenerationV1,
    state_quota_limits: StateQuotaLimitsV2,
    state_reservation: StateReservationV2,
    status: SessionStatusV2,
    next_client_sequence: u64,
    actor_id: Option<String>,
    actor_generation: Option<ActorGenerationIdV1>,
    next_actor_generation: GenerationV1,
    actor_has_state: bool,
    placement_identity: HostedPlacementIdentityV2,
    checkpoint: Option<DurableCheckpointV2>,
    recovery_attempt: Option<RecoveryAttemptV2>,
    durable_bytes: u64,
    operations: BTreeMap<String, OperationRecordV2>,
    commits: BTreeMap<u64, ClientCommitV2>,
    journal_sequence: u64,
    journal_head_sha256: String,
    head_receipt: SignedJournalEntryV2,
    created_unix_ms: u64,
    updated_unix_ms: u64,
    // Preparation retains a process-local, non-serializable evaluator handle.
    // It is deliberately an in-memory exclusion token, not durable session
    // state: no operation has been accepted while this is Some.
    preparation: Option<PreparationReservationV2>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparationReservationV2 {
    request_sha256: String,
    client_sequence: u64,
    client_request_id: String,
    operation_id: String,
    journal_head_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenAdmissionBasisV2 {
    active_sessions: u32,
    reserved_state_bytes: u64,
}

#[derive(Clone)]
struct RecoveryProbeV2 {
    source_utf8: String,
    task_attempt: TaskAttemptIdV1,
    canonical_backend: String,
    environment: EnvironmentRefV2,
    backend_implementation: SemanticDigestV1,
    sandbox_policy: SemanticDigestV1,
    launch_generation: SemanticDigestV1,
}

#[derive(Clone)]
struct OperationRecordV2 {
    view: OperationViewV2,
    reserved_bytes: u64,
}

#[derive(Clone)]
struct DurableCheckpointV2 {
    actor_generation: ActorGenerationIdV1,
    snapshot_sha256: String,
    snapshot_bytes: u64,
}

#[derive(Clone)]
struct RecoveryAttemptV2 {
    receipt_sha256: String,
    client_sequence: u64,
    client_request_id: String,
    request_sha256: String,
    warrant_sha256: String,
    placement_lease_sha256: String,
    placement_lease_nonce: String,
    trigger: RecoveryTriggerV2,
    previous_actor_generation: ActorGenerationIdV1,
    attempted_actor_generation: ActorGenerationIdV1,
    checkpoint_sha256: String,
    checkpoint_bytes: u64,
    reserved_bytes: u64,
}

struct ClientCommitV2 {
    request_id: String,
    request_sha256: String,
    receipt: SignedJournalEntryV2,
}

enum ActorCommandV2 {
    Prepare {
        operation: PreparedOperationV2,
        reply: mpsc::Sender<std::result::Result<PreparedPlacementFragmentV1, String>>,
    },
    Execute {
        operation: PreparedOperationV2,
        // Prepared fragments retain a complete admitted graph/runtime bundle.
        // Keep the command envelope small because it also carries tiny
        // lifecycle variants through the same channel.
        prepared: Box<PreparedPlacementFragmentV1>,
        actor_generation: Option<ActorGenerationIdV1>,
    },
    /// Force a staged immutable checkpoint through the backend RestoreV1
    /// boundary before the caller may publish a recovery decision. The
    /// internally prepared probes carry no user input and are discarded with
    /// the replacement evaluator if any backend omits or refuses its receipt.
    Recover {
        snapshot: EvaluatorStateSnapshotV1,
        snapshot_limit: u64,
        probes: Vec<RecoveryProbeV2>,
        deadline: Instant,
        reply: mpsc::Sender<std::result::Result<(), String>>,
    },
    #[cfg(debug_assertions)]
    PanicForTest,
    Close,
}

enum ExecutionDispositionV2 {
    /// Admission/evaluator entry failed before any backend command launched.
    Untouched(OperationOutcomeV2),
    /// The backend command settled (successfully or semantically failed), so
    /// actor state can be checkpointed before publishing the terminal record.
    Settled(OperationOutcomeV2),
    /// The backend process/transport failed after dispatch. Effects and result
    /// publication are unknown and must never be rewritten as a definite
    /// failure for a stateful actor.
    InFlightInfrastructure(String),
}

#[derive(Debug, Error)]
#[error("{message}")]
struct HostedV2Rejection {
    code: &'static str,
    message: String,
    retryable: bool,
}

fn reject(code: &'static str, message: impl Into<String>, retryable: bool) -> anyhow::Error {
    HostedV2Rejection {
        code,
        message: message.into(),
        retryable,
    }
    .into()
}

fn bounded_durable_text(message: &str) -> String {
    truncate_hosted_error_message(message.to_owned())
}

fn runtime_closed_error() -> anyhow::Error {
    HostedV2RuntimeClosedV2.into()
}

impl RuntimeInnerV2 {
    fn begin_call(&self) -> Result<RuntimeCallGuardV2<'_>> {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lifecycle.phase != RuntimePhaseV2::Running {
            return Err(runtime_closed_error());
        }
        lifecycle.active_calls = lifecycle
            .active_calls
            .checked_add(1)
            .context("hosted V2 active-call accounting overflow")?;
        Ok(RuntimeCallGuardV2 { inner: self })
    }

    fn require_running(&self) -> Result<()> {
        let lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lifecycle.phase == RuntimePhaseV2::Running {
            Ok(())
        } else {
            Err(runtime_closed_error())
        }
    }

    fn completed_shutdown_result(&self) -> Result<()> {
        let lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &lifecycle.shutdown_error {
            Some(error) => Err(error.clone().into()),
            None => Ok(()),
        }
    }
}

impl Drop for RuntimeInnerV2 {
    fn drop(&mut self) {
        // Explicit shutdown is the only deterministic lifecycle barrier. Drop
        // merely makes a best effort to wake actor threads; joining here could
        // deadlock if the last transient Arc is released by an actor itself.
        let lifecycle = self
            .lifecycle
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lifecycle.phase == RuntimePhaseV2::Running {
            lifecycle.phase = RuntimePhaseV2::ShuttingDown;
        }
        let workers = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .workers
            .values();
        for worker in workers {
            worker.request_close();
        }
    }
}

impl HostedV2Runtime {
    pub fn open(
        config: HostedV2RuntimeConfig,
        store: DurableSessionStoreV2,
        authorizer: SharedPlacementAuthorizerV2,
    ) -> Result<Self> {
        validate_identifier_v2("node_id", &config.node_id)?;
        validate_native_runtime_binary(&config.runtime_executable)
            .context("hosted V2 runtime executable is not a supported native image")?;
        let durable_bytes = store.durable_bytes()?;
        if durable_bytes > config.state_quotas.max_state_bytes_total() {
            bail!(
                "existing hosted V2 state uses {durable_bytes} bytes, above configured quota {}",
                config.state_quotas.max_state_bytes_total()
            );
        }
        let authority_control_headroom_bytes = store.remaining_authority_control_headroom_bytes();
        let inner = Arc::new(RuntimeInnerV2 {
            config,
            store: RuntimeStoreV2::new(store),
            authorizer,
            state: Mutex::new(RuntimeStateV2 {
                durable_bytes,
                authority_control_headroom_bytes,
                ..RuntimeStateV2::default()
            }),
            lifecycle: Mutex::new(RuntimeLifecycleV2::default()),
            lifecycle_changed: Condvar::new(),
            worker_lifecycles: Mutex::new(Vec::new()),
            #[cfg(debug_assertions)]
            current_view_prelock_barrier_for_test: Mutex::new(None),
        });
        let runtime = Self { inner };
        runtime.load_durable_sessions()?;
        Ok(runtime)
    }

    pub fn node_id(&self) -> Result<&str> {
        self.inner.require_running()?;
        Ok(&self.inner.config.node_id)
    }

    pub fn state_quotas(&self) -> Result<&StateQuotaLimitsV2> {
        self.inner.require_running()?;
        Ok(&self.inner.config.state_quotas)
    }

    /// Stop admission, drain every already-admitted actor mailbox, join every
    /// actor thread ever spawned by this runtime, and release this runtime's
    /// durable-store/root-lock ownership before returning. Concurrent callers
    /// share one idempotent outcome.
    pub fn shutdown(&self) -> Result<()> {
        let mut lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match lifecycle.phase {
                RuntimePhaseV2::Running => {
                    lifecycle.phase = RuntimePhaseV2::ShuttingDown;
                    while lifecycle.active_calls != 0 {
                        lifecycle = self
                            .inner
                            .lifecycle_changed
                            .wait(lifecycle)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    break;
                }
                RuntimePhaseV2::ShuttingDown => {
                    lifecycle = self
                        .inner
                        .lifecycle_changed
                        .wait(lifecycle)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                RuntimePhaseV2::Closed => {
                    drop(lifecycle);
                    return self.inner.completed_shutdown_result();
                }
            }
        }
        drop(lifecycle);

        let current_workers = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .workers
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for worker in &current_workers {
            worker.request_close();
        }
        let lifecycles = self
            .inner
            .worker_lifecycles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut failures = Vec::new();
        for lifecycle in &lifecycles {
            if let Some(Err(payload)) = lifecycle.join() {
                let detail = payload
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".to_owned());
                failures.push(format!("{}: {detail}", lifecycle.session_id));
            }
        }
        failures.sort();
        failures.dedup();

        self.inner
            .worker_lifecycles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .workers
            .clear();

        // The write-side store barrier proves no store operation is still in
        // progress. Workers have already joined and public calls were drained,
        // so dropping this Option releases the runtime-owned root lock now.
        drop(self.inner.store.take());

        let shutdown_error = (!failures.is_empty()).then(|| HostedV2RuntimeShutdownErrorV2 {
            message: failures.join("; "),
        });
        let mut lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lifecycle.phase = RuntimePhaseV2::Closed;
        lifecycle.shutdown_error = shutdown_error.clone();
        self.inner.lifecycle_changed.notify_all();
        drop(lifecycle);
        match shutdown_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    /// Return live durable-byte accounting for fault-injection regressions.
    /// This is absent from release builds and is not a hosted protocol API.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn durable_accounting_for_test(&self, session_id: &str) -> Result<(u64, u64, u64)> {
        let _call = self.inner.begin_call()?;
        let state = self.lock_state()?;
        let session = state
            .sessions
            .get(session_id)
            .context("test accounting requested an unknown hosted session")?;
        Ok((
            state.durable_bytes,
            session.durable_bytes,
            state.reserved_durable_bytes,
        ))
    }

    /// Deterministically close one session actor after its next successful
    /// Prepare and before Execute is sent. Debug-only integration tests use
    /// this to exercise the accepted-before-start interruption transition.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn inject_actor_close_before_execute_for_test(&self, session_id: &str) -> Result<()> {
        let _call = self.inner.begin_call()?;
        let mut state = self.lock_state()?;
        if !state.sessions.contains_key(session_id) {
            bail!("test actor-close injection names an unknown hosted session");
        }
        if !state
            .close_actor_before_execute_for_test
            .insert(session_id.to_owned())
        {
            bail!("test actor-close injection is already armed for this session");
        }
        Ok(())
    }

    /// Force the next checkpoint for one session to fail, then pause after the
    /// signed ActorCheckpointFailed record and before OperationTerminal. This
    /// exposes the exact inter-record window to concurrency regressions.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn inject_checkpoint_failure_gap_for_test(
        &self,
        session_id: &str,
        entered: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) -> Result<()> {
        let _call = self.inner.begin_call()?;
        let mut state = self.lock_state()?;
        if !state.sessions.contains_key(session_id) {
            bail!("test checkpoint-failure injection names an unknown hosted session");
        }
        if state.force_checkpoint_failure_for_test.contains(session_id)
            || state
                .checkpoint_failure_terminal_barrier_for_test
                .contains_key(session_id)
        {
            bail!("test checkpoint-failure injection is already armed for this session");
        }
        state
            .force_checkpoint_failure_for_test
            .insert(session_id.to_owned());
        state
            .checkpoint_failure_terminal_barrier_for_test
            .insert(session_id.to_owned(), (entered, release));
        Ok(())
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn has_worker_for_test(&self, session_id: &str) -> Result<bool> {
        let _call = self.inner.begin_call()?;
        Ok(self.lock_state()?.workers.contains_key(session_id))
    }

    /// Make one actor panic at its next mailbox turn. Shutdown must still join
    /// it, release the durable root lock, and publish a stable typed failure.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn inject_worker_panic_for_test(&self, session_id: &str) -> Result<()> {
        let _call = self.inner.begin_call()?;
        let worker = self
            .lock_state()?
            .workers
            .get(session_id)
            .cloned()
            .context("test worker-panic injection names an unknown hosted session")?;
        worker
            .send(ActorCommandV2::PanicForTest)
            .map_err(|_| reject("actor-unavailable", "session actor is unavailable", true))
    }

    /// Pause the next Status/Actors request after its optimistic store-current
    /// check and before it locks runtime state. This is a deterministic test
    /// hook for proving that current-head views recheck a concurrently poisoned
    /// durable store after entering the state critical section.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn inject_current_view_prelock_barrier_for_test(
        &self,
        entered: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) -> Result<()> {
        let _call = self.inner.begin_call()?;
        let mut hook = self
            .inner
            .current_view_prelock_barrier_for_test
            .lock()
            .map_err(|_| anyhow::anyhow!("hosted V2 current-view test hook lock is poisoned"))?;
        if hook.replace((entered, release)).is_some() {
            bail!("hosted V2 current-view test hook is already armed");
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn wait_current_view_prelock_barrier_for_test(&self) -> Result<()> {
        let barriers = self
            .inner
            .current_view_prelock_barrier_for_test
            .lock()
            .map_err(|_| anyhow::anyhow!("hosted V2 current-view test hook lock is poisoned"))?
            .take();
        if let Some((entered, release)) = barriers {
            entered.wait();
            release.wait();
        }
        Ok(())
    }

    pub fn unreadable_sessions(&self) -> Result<Vec<String>> {
        let _call = self.inner.begin_call()?;
        Ok(self.lock_state()?.unreadable_sessions.clone())
    }

    fn require_store_current(&self) -> Result<()> {
        if self.inner.store.is_reopen_required()? {
            return Err(reject(
                "store-reopen-required",
                "durable store state is indeterminate; reopen the node before serving mutations or current-head views",
                false,
            ));
        }
        Ok(())
    }

    pub fn handle_request(
        &self,
        principal_sha256: &str,
        request: HostedRequestV2,
    ) -> HostedResponseV2 {
        if let Err(error) = self.inner.require_running() {
            return error_response(error);
        }
        if let Err(error) = request.validate() {
            return HostedResponseV2::Error {
                error: HostedProtocolErrorV2::new("invalid-request", format!("{error:#}"), false),
            };
        }
        let outcome = match request {
            HostedRequestV2::OpenSession { request, .. } => {
                self.open_session(principal_sha256, request)
            }
            HostedRequestV2::SubmitOperation { request, .. } => {
                self.submit_operation(principal_sha256, request)
            }
            HostedRequestV2::Status { query, .. } => self.status(principal_sha256, query),
            HostedRequestV2::Actors { query, .. } => self.actors(principal_sha256, query),
            HostedRequestV2::ResetSession { request, .. } => {
                self.reset_session(principal_sha256, request)
            }
            HostedRequestV2::RecoverSession { request, .. } => {
                self.recover_session(principal_sha256, request)
            }
            HostedRequestV2::CloseSession { request, .. } => {
                self.close_session(principal_sha256, request)
            }
        };
        outcome.unwrap_or_else(error_response)
    }

    pub fn open_session(
        &self,
        principal_sha256: &str,
        request: OpenSessionRequestV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        validate_sha256_v2("principal_sha256", principal_sha256)?;
        request.validate()?;
        let session_id = request.proposed_capability.session_id.clone();
        let proposed_bearer = decode_fixed_hex::<32>(
            "proposed session bearer",
            &request.proposed_capability.bearer,
        )?;
        let placement_lease_sha256 = request
            .placement_lease
            .authority
            .semantic_digest()?
            .to_string();
        let placement_lease_nonce = request.placement_lease.authority.lease_nonce().to_string();
        let open_request_sha256 = canonical_hosted_sha256(&request)?;

        // A retry of a durably committed Open must not depend on the original
        // short-lived capacity observation still being fresh. Match the exact
        // first record before generic nonce/session reuse checks instead.
        {
            let state = self.lock_state()?;
            if state.retired_session_ids.contains(&session_id) {
                return Err(reject(
                    "state-session-retired",
                    "hosted state-session identity was permanently retired by signed GC authority",
                    false,
                ));
            }
            if let Some(response) = duplicate_open_response(
                &state,
                principal_sha256,
                &request,
                &proposed_bearer,
                &open_request_sha256,
                &placement_lease_sha256,
                &placement_lease_nonce,
            )? {
                return Ok(response);
            }
        }
        let now = unix_time_ms()?;
        let proposed_session = request.placement_lease.command.state_session.clone();
        let proposed_reservation = request.placement_lease.command.state_reservation.clone();
        let mut context = PlacementAuthorizationContextV2 {
            node_id: self.inner.config.node_id.clone(),
            node_generation: self.inner.config.node_generation,
            principal_sha256: principal_sha256.to_owned(),
            state_session: proposed_session,
            session_state_tier: request.state_tier,
            client_request_id: request.client_request_id.clone(),
            client_sequence: 0,
            purpose: PlacementPurposeV2::OpenSession,
            operation_sha256: None,
            recovery_warrant_sha256: None,
            state_quota_generation: self.inner.config.state_quota_generation,
            state_quota_limits: self.inner.config.state_quotas.clone(),
            state_reservation: proposed_reservation,
            current_actor_generation: None,
            next_actor_generation: GenerationV1::new(1)
                .expect("hosted actor generations start at one"),
            prepared_fragment: None,
            expected_session_identity: None,
            now_unix_ms: now,
        };
        let authorized = self
            .inner
            .authorizer
            .authorize(&context, &request.placement_lease)
            .map_err(|error| reject("placement-denied", format!("{error:#}"), false))?;

        let mut state = self.lock_state()?;
        if state.retired_session_ids.contains(&session_id) {
            return Err(reject(
                "state-session-retired",
                "hosted state-session identity was permanently retired by signed GC authority",
                false,
            ));
        }
        if let Some(response) = duplicate_open_response(
            &state,
            principal_sha256,
            &request,
            &proposed_bearer,
            &open_request_sha256,
            &placement_lease_sha256,
            &placement_lease_nonce,
        )? {
            return Ok(response);
        }
        if state.used_lease_nonces.contains(&authorized.lease_nonce) {
            return Err(reject(
                "placement-lease-reused",
                "hosted placement lease nonce was already consumed",
                false,
            ));
        }
        let authorized_session_id = authorized
            .state_session
            .semantic_digest()
            .context("failed to digest admitted state session")?
            .to_string();
        if authorized_session_id != session_id {
            return Err(reject(
                "open-session-identity-mismatch",
                "placement authority admitted a different state session than the proposed capability",
                false,
            ));
        }
        if state.sessions.contains_key(&session_id) {
            self.journal_placement_refusal(
                &mut state,
                &authorized,
                &request.placement_lease.command,
                "state-session-reused",
                "placement authority proposed an existing state session",
            )?;
            return Err(reject(
                "state-session-reused",
                "placement authority proposed an existing state session",
                false,
            ));
        }
        let active_total = state
            .sessions
            .values()
            .filter(|session| session.status != SessionStatusV2::Closed)
            .count() as u32;
        if active_total >= self.inner.config.state_quotas.max_open_sessions() {
            self.journal_placement_refusal(
                &mut state,
                &authorized,
                &request.placement_lease.command,
                "quota-exceeded",
                "hosted V2 open-session hard quota is exhausted; no session was evicted",
            )?;
            return Err(quota_rejection("open sessions"));
        }
        if authorized.state_reservation.actor_count() != 1 {
            self.journal_placement_refusal(
                &mut state,
                &authorized,
                &request.placement_lease.command,
                "state-reservation-unsupported",
                "hosted V2 currently realizes exactly one actor per session",
            )?;
            return Err(reject(
                "state-reservation-unsupported",
                "hosted V2 currently realizes exactly one actor per session",
                false,
            ));
        }
        let projected_reserved = reserved_state_capacity(&state)?
            .checked_add(authorized.state_reservation.state_bytes())
            .context("hosted state reservation accounting overflow")?;
        if projected_reserved > self.inner.config.state_quotas.max_state_bytes_total() {
            self.journal_placement_refusal(
                &mut state,
                &authorized,
                &request.placement_lease.command,
                "quota-exceeded",
                "hosted V2 state-byte hard quota is exhausted; no session was evicted",
            )?;
            return Err(quota_rejection("state bytes total"));
        }

        // Snapshot only the mutable coordinates that govern Open admission.
        // Process acquisition and the second authority pass happen without the
        // global mutex; an unrelated mutation may proceed, but this Open must
        // restart if it changes either capacity coordinate before commit.
        let admission_basis = open_admission_basis(&state)?;
        drop(state);

        // Acquire the fallible process resource before creating durable
        // session identity or returning its sole bearer. The idle actor owns
        // no session state until a later accepted Execute command.
        let sender = match spawn_actor(&self.inner, &session_id, request.state_tier) {
            Ok(sender) => sender,
            Err(error) => {
                let mut state = self.lock_state()?;
                if state.retired_session_ids.contains(&session_id) {
                    return Err(reject(
                        "state-session-retired",
                        "hosted state-session identity was permanently retired by signed GC authority",
                        false,
                    ));
                }
                if let Some(response) = duplicate_open_response(
                    &state,
                    principal_sha256,
                    &request,
                    &proposed_bearer,
                    &open_request_sha256,
                    &placement_lease_sha256,
                    &placement_lease_nonce,
                )? {
                    return Ok(response);
                }
                self.journal_placement_refusal(
                    &mut state,
                    &authorized,
                    &request.placement_lease.command,
                    "actor-spawn-failed",
                    &format!("hosted session actor could not be acquired: {error:#}"),
                )?;
                return Err(reject(
                    "actor-unavailable",
                    format!("hosted session actor could not be acquired: {error:#}"),
                    true,
                ));
            }
        };

        let salt = random_32()?;
        let bearer_hash = salted_bearer_hash(&salt, &proposed_bearer);

        // The first pass proves the request was eligible before resource
        // acquisition. Re-sample wall time after that potentially slow work
        // and ask the authority to validate the exact same proof again. The
        // authorizer is deliberately called without the global runtime lock.
        context.now_unix_ms = unix_time_ms()?;
        let commit_authorized = self
            .inner
            .authorizer
            .authorize(&context, &request.placement_lease)
            .map_err(|error| reject("placement-expired", format!("{error:#}"), false))?;

        let mut state = self.lock_state()?;
        if state.retired_session_ids.contains(&session_id) {
            return Err(reject(
                "state-session-retired",
                "hosted state-session identity was permanently retired by signed GC authority",
                false,
            ));
        }
        if let Some(response) = duplicate_open_response(
            &state,
            principal_sha256,
            &request,
            &proposed_bearer,
            &open_request_sha256,
            &placement_lease_sha256,
            &placement_lease_nonce,
        )? {
            return Ok(response);
        }
        if commit_authorized != authorized {
            return Err(reject(
                "placement-changed",
                "open-session placement authorization changed between validation passes",
                false,
            ));
        }
        let commit_now = unix_time_ms()?;
        if commit_now >= open_freshness_deadline(&request)? {
            return Err(reject(
                "placement-expired",
                "open-session placement evidence expired before durable commit",
                false,
            ));
        }
        if state.used_lease_nonces.contains(&authorized.lease_nonce) {
            return Err(reject(
                "placement-lease-reused",
                "hosted placement lease nonce was already consumed",
                false,
            ));
        }
        if state.sessions.contains_key(&session_id) {
            return Err(reject(
                "state-session-reused",
                "placement authority proposed an existing state session",
                false,
            ));
        }
        if open_admission_basis(&state)? != admission_basis {
            return Err(reject(
                "open-admission-stale",
                "hosted session capacity changed during Open resource acquisition or authorization",
                true,
            ));
        }

        let event = JournalEventV2::SessionOpened {
            request_sha256: open_request_sha256.clone(),
            principal_sha256: principal_sha256.to_owned(),
            bearer_salt: hex::encode(salt),
            bearer_hash: bearer_hash.clone(),
            capability_commitment: request.capability_commitment.clone(),
            state_tier: request.state_tier,
            state_session: authorized.state_session.clone(),
            state_quota_generation: authorized.state_quota_generation,
            state_quota_limits: authorized.state_quota_limits.clone(),
            state_reservation: authorized.state_reservation.clone(),
            placement_identity: authorized.placement_identity.clone(),
            placement_lease_sha256: authorized.lease_sha256.clone(),
            placement_lease_nonce: authorized.lease_nonce.clone(),
            client_request_id: request.client_request_id.clone(),
        };
        let receipt = self.issue_entry(&session_id, 1, None, commit_now, event)?;
        let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
        if needed
            .checked_add(SESSION_CLOSE_HEADROOM_RESERVATION)
            .and_then(|projected| projected.checked_add(ACTOR_FENCE_HEADROOM_RESERVATION))
            .is_none_or(|projected| projected > authorized.state_reservation.state_bytes())
        {
            self.journal_placement_refusal(
                &mut state,
                &authorized,
                &request.placement_lease.command,
                "quota-exceeded",
                "session opening receipt exceeds the authenticated state reservation",
            )?;
            return Err(quota_rejection("state bytes per session"));
        }
        let written = self.inner.store.install_session(&session_id, &receipt)?;
        state.durable_bytes = state
            .durable_bytes
            .checked_add(written)
            .context("hosted durable-byte accounting overflow")?;
        state.state_bytes_reserved = state
            .state_bytes_reserved
            .checked_add(authorized.state_reservation.state_bytes())
            .context("hosted state reservation accounting overflow")?;
        state.used_lease_nonces.insert(authorized.lease_nonce);
        state.sessions.insert(
            session_id.clone(),
            SessionRecordV2 {
                session_id: session_id.clone(),
                node_id: self.inner.config.node_id.clone(),
                principal_sha256: principal_sha256.to_owned(),
                bearer_salt: salt,
                bearer_hash,
                open_capability_commitment: request.capability_commitment,
                open_request_sha256,
                open_placement_lease_sha256: placement_lease_sha256,
                open_placement_lease_nonce: placement_lease_nonce,
                open_client_request_id: request.client_request_id,
                open_receipt: receipt.clone(),
                state_tier: request.state_tier,
                state_session: authorized.state_session,
                state_quota_generation: authorized.state_quota_generation,
                state_quota_limits: authorized.state_quota_limits,
                state_reservation: authorized.state_reservation,
                status: SessionStatusV2::Ready,
                next_client_sequence: 1,
                actor_id: None,
                actor_generation: None,
                next_actor_generation: GenerationV1::new(1)
                    .expect("hosted actor generations start at one"),
                actor_has_state: false,
                placement_identity: authorized.placement_identity,
                checkpoint: None,
                recovery_attempt: None,
                durable_bytes: written,
                operations: BTreeMap::new(),
                commits: BTreeMap::new(),
                journal_sequence: 1,
                journal_head_sha256: receipt.entry_sha256.clone(),
                head_receipt: receipt.clone(),
                created_unix_ms: commit_now,
                updated_unix_ms: commit_now,
                preparation: None,
            },
        );
        state.workers.insert(session_id.clone(), sender);
        Ok(HostedResponseV2::SessionOpened {
            capability: request.proposed_capability,
            receipt,
        })
    }

    pub fn submit_operation(
        &self,
        principal_sha256: &str,
        request: SubmitOperationRequestV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        request.credentials.validate()?;
        request.operation.validate()?;
        let request_sha256 = canonical_hosted_sha256(&request)?;
        let operation_sha256 = request.operation.sha256()?;
        let session_id = request.credentials.session_id.clone();
        // Exact durable settlement wins over volatile admission facts. A
        // response retry after its deadline or a catalog upgrade must return
        // the signed OperationAccepted receipt, not re-adjudicate the request
        // against today's environment.
        {
            let state = self.lock_state()?;
            authenticate_locked(&state, principal_sha256, &request.credentials)?;
            if let Some(receipt) = duplicate_commit(
                &state,
                &session_id,
                request.client_sequence,
                &request.client_request_id,
                &request_sha256,
            )? {
                return Ok(HostedResponseV2::Committed { receipt });
            }
        }
        let initial_now = unix_time_ms()?;
        if request.operation.expected_backend_catalog_sha256
            != BackendRegistry::global().catalog_sha256()
        {
            return Err(reject(
                "backend-catalog-mismatch",
                "operation was prepared for a different backend catalog",
                false,
            ));
        }
        if initial_now >= request.operation.deadline_unix_ms {
            return Err(reject(
                "deadline-expired",
                "operation deadline expired before admission",
                false,
            ));
        }

        let (sender, preparation) = {
            let mut state = self.lock_state()?;
            authenticate_locked(&state, principal_sha256, &request.credentials)?;
            if let Some(receipt) = duplicate_commit(
                &state,
                &session_id,
                request.client_sequence,
                &request.client_request_id,
                &request_sha256,
            )? {
                return Ok(HostedResponseV2::Committed { receipt });
            }
            require_next_sequence(&state, &session_id, request.client_sequence)?;
            let session = state.sessions.get(&session_id).unwrap();
            if session.status != SessionStatusV2::Ready {
                return Err(reject(
                    "session-not-ready",
                    format!("session is {:?}, not ready", session.status),
                    true,
                ));
            }
            if session.preparation.is_some() {
                return Err(reject(
                    "session-preparing",
                    "another operation is being prepared for this session",
                    true,
                ));
            }
            if session.operations.len() >= MAX_OPERATIONS_PER_SESSION_V2 {
                return Err(reject(
                    "protocol-bound-exceeded",
                    "session reached the fixed operation-ledger codec bound",
                    false,
                ));
            }
            if session
                .operations
                .contains_key(&request.operation.operation_id)
            {
                return Err(reject(
                    "operation-id-reused",
                    "operation identity already exists in this session",
                    false,
                ));
            }
            let preparation = PreparationReservationV2 {
                request_sha256: request_sha256.clone(),
                client_sequence: request.client_sequence,
                client_request_id: request.client_request_id.clone(),
                operation_id: request.operation.operation_id.clone(),
                journal_head_sha256: session.journal_head_sha256.clone(),
            };
            let sender = match state.workers.get(&session_id).cloned() {
                Some(sender) => sender,
                None => {
                    let session = state.sessions.get(&session_id).unwrap();
                    if session.state_tier.needs_live_actor() && session.actor_generation.is_some() {
                        let had_state = session.actor_has_state;
                        fence_missing_worker_locked(
                            &self.inner,
                            &mut state,
                            &session_id,
                            "session worker disappeared before operation preparation",
                        )?;
                        return Err(reject(
                            if had_state {
                                "session-recovery-required"
                            } else {
                                "actor-generation-retired"
                            },
                            if had_state {
                                "stateful session worker was lost; explicit recovery is required before Execute"
                            } else {
                                "state-empty actor generation was retired; retry Execute against the fenced successor"
                            },
                            !had_state,
                        ));
                    }
                    let sender = spawn_actor(&self.inner, &session_id, session.state_tier)
                        .map_err(|error| {
                            reject(
                                "actor-unavailable",
                                format!("session actor could not be started: {error:#}"),
                                true,
                            )
                        })?;
                    state.workers.insert(session_id.clone(), sender.clone());
                    sender
                }
            };
            state.sessions.get_mut(&session_id).unwrap().preparation = Some(preparation.clone());
            (sender, preparation)
        };

        let (prepared_sender, prepared_receiver) = mpsc::channel();
        let prepared_result = sender
            .send(ActorCommandV2::Prepare {
                operation: request.operation.clone(),
                reply: prepared_sender,
            })
            .map_err(|_| reject("actor-unavailable", "session actor is unavailable", true))
            .and_then(|()| {
                let prepare_wait_ms = request
                    .operation
                    .deadline_unix_ms
                    .saturating_sub(initial_now)
                    .min(30_000);
                prepared_receiver
                    .recv_timeout(Duration::from_millis(prepare_wait_ms.max(1)))
                    .map_err(|_| {
                        reject(
                            "fragment-prepare-timeout",
                            "session actor did not prepare the fragment before the local safety bound",
                            true,
                        )
                    })?
                    .map_err(|message| reject("fragment-prepare-failed", message, false))
            });

        // Reacquire only long enough to prove that no same-session durable
        // coordinate changed while the process-local handle was prepared.
        let context_basis = {
            let mut state = self.lock_state()?;
            authenticate_locked(&state, principal_sha256, &request.credentials)?;
            let session = state
                .sessions
                .get(&session_id)
                .context("session disappeared during placement preparation")?;
            let unchanged = session.preparation.as_ref() == Some(&preparation)
                && session.status == SessionStatusV2::Ready
                && session.next_client_sequence == request.client_sequence
                && session.journal_head_sha256 == preparation.journal_head_sha256
                && !session
                    .operations
                    .contains_key(&request.operation.operation_id);
            if !unchanged {
                if state
                    .sessions
                    .get(&session_id)
                    .and_then(|session| session.preparation.as_ref())
                    == Some(&preparation)
                {
                    state.sessions.get_mut(&session_id).unwrap().preparation = None;
                }
                return Err(reject(
                    "preparation-stale",
                    "session coordinates changed while the placement fragment was prepared",
                    true,
                ));
            }
            let prepared = match prepared_result {
                Ok(prepared) => prepared,
                Err(error) => {
                    state.sessions.get_mut(&session_id).unwrap().preparation = None;
                    if error
                        .downcast_ref::<HostedV2Rejection>()
                        .is_some_and(|rejection| rejection.code == "actor-unavailable")
                    {
                        state.workers.remove(&session_id);
                    }
                    return Err(error);
                }
            };
            if prepared.bindings().source_sha256() != request.operation.source_sha256
                || prepared.bindings().task_attempt() != &request.operation.task_attempt
            {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(reject(
                    "fragment-binding-mismatch",
                    "locally prepared source/task bindings differ from the submitted operation",
                    false,
                ));
            }
            let session = state.sessions.get(&session_id).unwrap();
            let expected_actor_generation = session
                .actor_generation
                .as_ref()
                .map(ActorGenerationIdV1::generation)
                .unwrap_or(session.next_actor_generation);
            (
                prepared,
                session.state_session.clone(),
                session.state_tier,
                session.state_quota_generation,
                session.state_quota_limits.clone(),
                session.state_reservation.clone(),
                session.actor_generation.clone(),
                expected_actor_generation,
                session.placement_identity.clone(),
            )
        };

        let (
            prepared,
            state_session,
            session_state_tier,
            state_quota_generation,
            state_quota_limits,
            state_reservation,
            current_actor_generation,
            expected_actor_generation,
            expected_session_identity,
        ) = context_basis;
        // This timestamp is intentionally sampled after potentially expensive
        // preparation and immediately before validating all signed lifetimes.
        let authorization_now = match unix_time_ms() {
            Ok(now) => now,
            Err(error) => {
                clear_preparation(&self.inner, &session_id, &preparation)?;
                return Err(error);
            }
        };
        if authorization_now >= request.operation.deadline_unix_ms {
            clear_preparation(&self.inner, &session_id, &preparation)?;
            return Err(reject(
                "deadline-expired",
                "operation deadline expired during placement preparation",
                false,
            ));
        }
        let mut context = PlacementAuthorizationContextV2 {
            node_id: self.inner.config.node_id.clone(),
            node_generation: self.inner.config.node_generation,
            principal_sha256: principal_sha256.to_owned(),
            state_session,
            session_state_tier,
            client_request_id: request.client_request_id.clone(),
            client_sequence: request.client_sequence,
            purpose: PlacementPurposeV2::Execute,
            operation_sha256: Some(operation_sha256.clone()),
            recovery_warrant_sha256: None,
            state_quota_generation,
            state_quota_limits,
            state_reservation,
            current_actor_generation,
            next_actor_generation: expected_actor_generation,
            prepared_fragment: Some(prepared.bindings().clone()),
            expected_session_identity: Some(expected_session_identity),
            now_unix_ms: authorization_now,
        };
        let authorized_result = self
            .inner
            .authorizer
            .authorize(&context, &request.placement_lease)
            .map_err(|error| reject("placement-denied", format!("{error:#}"), false));

        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal_sha256, &request.credentials)?;
        let session = state
            .sessions
            .get(&session_id)
            .context("session disappeared during placement authorization")?;
        let unchanged = session.preparation.as_ref() == Some(&preparation)
            && session.status == SessionStatusV2::Ready
            && session.next_client_sequence == request.client_sequence
            && session.journal_head_sha256 == preparation.journal_head_sha256
            && !session
                .operations
                .contains_key(&request.operation.operation_id);
        if !unchanged {
            if state
                .sessions
                .get(&session_id)
                .and_then(|session| session.preparation.as_ref())
                == Some(&preparation)
            {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
            }
            return Err(reject(
                "authorization-stale",
                "session coordinates changed while placement authority was evaluated",
                true,
            ));
        }
        let commit_validation_now = match unix_time_ms() {
            Ok(now) => now,
            Err(error) => {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(error);
            }
        };
        if commit_validation_now >= request.operation.deadline_unix_ms {
            state.sessions.get_mut(&session_id).unwrap().preparation = None;
            return Err(reject(
                "deadline-expired",
                "operation deadline expired before durable acceptance",
                false,
            ));
        }
        let authorized = match authorized_result {
            Ok(authorized) => authorized,
            Err(error) => {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(error);
            }
        };
        context.now_unix_ms = commit_validation_now;
        drop(state);
        let commit_authorized = self
            .inner
            .authorizer
            .authorize(&context, &request.placement_lease)
            .map_err(|error| reject("placement-expired", format!("{error:#}"), false));

        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal_sha256, &request.credentials)?;
        let session = state
            .sessions
            .get(&session_id)
            .context("session disappeared during commit authorization")?;
        let unchanged = session.preparation.as_ref() == Some(&preparation)
            && session.status == SessionStatusV2::Ready
            && session.next_client_sequence == request.client_sequence
            && session.journal_head_sha256 == preparation.journal_head_sha256
            && !session
                .operations
                .contains_key(&request.operation.operation_id);
        if !unchanged {
            if state
                .sessions
                .get(&session_id)
                .and_then(|session| session.preparation.as_ref())
                == Some(&preparation)
            {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
            }
            return Err(reject(
                "commit-stale",
                "session coordinates changed during final placement authorization",
                true,
            ));
        }
        let commit_now = match unix_time_ms() {
            Ok(now) => now,
            Err(error) => {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(error);
            }
        };
        state.sessions.get_mut(&session_id).unwrap().preparation = None;
        let commit_authorized = commit_authorized?;
        if commit_authorized != authorized {
            return Err(reject(
                "placement-changed",
                "placement authorization changed between preparation and commit",
                false,
            ));
        }
        if commit_now >= request.operation.deadline_unix_ms
            || commit_now >= placement_freshness_deadline(&request.placement_lease)
        {
            return Err(reject(
                "placement-expired",
                "operation or placement lease expired before durable acceptance",
                false,
            ));
        }
        if state.used_lease_nonces.contains(&authorized.lease_nonce) {
            return Err(reject(
                "placement-lease-reused",
                "hosted placement lease nonce was already consumed",
                false,
            ));
        }

        let actor_id = match &authorized.actor_generation {
            Some(_) => Some(
                state.sessions[&session_id]
                    .actor_id
                    .clone()
                    .unwrap_or(fresh_identifier("actor")?),
            ),
            None => None,
        };

        let session = state.sessions.get(&session_id).unwrap();
        let event = JournalEventV2::OperationAccepted {
            client_sequence: request.client_sequence,
            client_request_id: request.client_request_id.clone(),
            request_sha256: request_sha256.clone(),
            operation_id: request.operation.operation_id.clone(),
            task_attempt: request.operation.task_attempt.clone(),
            operation_sha256: operation_sha256.clone(),
            source_sha256: request.operation.source_sha256.clone(),
            actor_id: actor_id.clone(),
            actor_generation: authorized.actor_generation.clone(),
            placement_lease_sha256: authorized.lease_sha256.clone(),
            placement_lease_nonce: authorized.lease_nonce.clone(),
        };
        let receipt = self.issue_next_entry(session, commit_now, event)?;
        // A retry may find the exact immutable operation blob left behind by
        // a crash after blob publication but before OperationAccepted. Charge
        // only the bytes this write will actually add; otherwise the retry can
        // be rejected at quota even though it is durable-byte neutral.
        let operation_frame = self
            .inner
            .store
            .operation_new_bytes(&session_id, &request.operation)?;
        let journal_frame = self.inner.store.encoded_frame_bytes(&receipt)?;
        let reservation = request
            .operation
            .output_limit_bytes
            .checked_add(TERMINAL_RECORD_OVERHEAD_RESERVATION)
            .context("hosted operation reservation overflow")?;
        let needed = operation_frame
            .checked_add(journal_frame)
            .and_then(|value| value.checked_add(reservation))
            .context("hosted durable reservation overflow")?;
        if let Err(error) = ensure_session_durable_capacity(&state, &session_id, needed) {
            self.journal_placement_refusal(
                &mut state,
                &authorized,
                &request.placement_lease.command,
                "quota-exceeded",
                "operation durable records exceed the authenticated session reservation",
            )?;
            return Err(error);
        }

        let next_durable_after_operation = state
            .durable_bytes
            .checked_add(operation_frame)
            .context("hosted durable-byte accounting overflow")?;
        let next_session_after_operation = state.sessions[&session_id]
            .durable_bytes
            .checked_add(operation_frame)
            .context("hosted session-byte accounting overflow")?;
        let operation_written = self
            .inner
            .store
            .write_operation(&session_id, &request.operation)?;
        if operation_written != operation_frame {
            bail!(
                "operation durable-byte delta changed between preflight ({operation_frame}) and publication ({operation_written})"
            );
        }
        // The immutable blob is durable independently of the following
        // journal append. Charge it immediately so an append failure followed
        // by an exact zero-delta retry cannot make the blob disappear from
        // in-process quota accounting.
        state.durable_bytes = next_durable_after_operation;
        state.sessions.get_mut(&session_id).unwrap().durable_bytes = next_session_after_operation;
        let journal_written = self.inner.store.append_entry(&session_id, &receipt)?;
        state.durable_bytes = state
            .durable_bytes
            .checked_add(journal_written)
            .context("hosted durable-byte accounting overflow")?;
        state.reserved_durable_bytes = state
            .reserved_durable_bytes
            .checked_add(reservation)
            .context("hosted durable reservation overflow")?;
        state.used_lease_nonces.insert(authorized.lease_nonce);
        let accepted = OperationViewV2 {
            operation_id: request.operation.operation_id.clone(),
            task_attempt: request.operation.task_attempt.clone(),
            operation_sha256,
            status: OperationStatusV2::Accepted,
            accepted_unix_ms: commit_now,
            started_unix_ms: None,
            finished_unix_ms: None,
            outcome: None,
        };
        {
            let session = state.sessions.get_mut(&session_id).unwrap();
            session.durable_bytes = session
                .durable_bytes
                .checked_add(journal_written)
                .context("hosted session-byte accounting overflow")?;
            apply_receipt_head(session, &receipt);
            session.status = SessionStatusV2::Executing;
            if session.actor_generation.is_none() {
                session.actor_id = actor_id;
                session.actor_generation = authorized.actor_generation.clone();
            }
            session.operations.insert(
                request.operation.operation_id.clone(),
                OperationRecordV2 {
                    view: accepted,
                    reserved_bytes: reservation,
                },
            );
            record_commit(
                session,
                request.client_sequence,
                request.client_request_id,
                request_sha256,
                receipt.clone(),
            )?;
        }
        let actor_generation = state.sessions[&session_id].actor_generation.clone();
        drop(state);
        #[cfg(debug_assertions)]
        {
            let close_before_execute = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?
                .close_actor_before_execute_for_test
                .remove(&session_id);
            if close_before_execute {
                self.inner
                    .state
                    .lock()
                    .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?
                    .workers
                    .remove(&session_id);
                sender.request_close();
                if let Some(result) = sender.join() {
                    result.map_err(|_| anyhow::anyhow!("test actor panicked during close"))?;
                }
            }
        }
        if sender
            .send(ActorCommandV2::Execute {
                operation: request.operation,
                prepared: Box::new(prepared),
                actor_generation,
            })
            .is_err()
        {
            if let Ok(mut state) = self.inner.state.lock() {
                state.workers.remove(&session_id);
            }
            interrupt_before_start(
                &self.inner,
                &session_id,
                "session actor command channel is closed",
            )?;
        }
        Ok(HostedResponseV2::Committed { receipt })
    }

    pub fn status(
        &self,
        principal_sha256: &str,
        query: SessionQueryV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        #[cfg(debug_assertions)]
        self.wait_current_view_prelock_barrier_for_test()?;
        let state = self.lock_state()?;
        self.require_store_current()?;
        authenticate_locked(&state, principal_sha256, &query.credentials)?;
        let session = state.sessions.get(&query.credentials.session_id).unwrap();
        let mut view = session_view(session, unix_time_ms()?);
        if let Some(operation_id) = query.operation_id {
            let selected = view.operations.remove(&operation_id).ok_or_else(|| {
                reject(
                    "operation-not-found",
                    "operation does not exist in this session",
                    false,
                )
            })?;
            view.operations.clear();
            view.operations.insert(operation_id, selected);
        }
        let response = HostedResponseV2::Status {
            session: view,
            head_receipt: session.head_receipt.clone(),
        };
        self.require_store_current()?;
        Ok(response)
    }

    pub fn actors(
        &self,
        principal_sha256: &str,
        query: SessionQueryV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        #[cfg(debug_assertions)]
        self.wait_current_view_prelock_barrier_for_test()?;
        let state = self.lock_state()?;
        self.require_store_current()?;
        authenticate_locked(&state, principal_sha256, &query.credentials)?;
        let session = state.sessions.get(&query.credentials.session_id).unwrap();
        let response = HostedResponseV2::Actors {
            session_id: session.session_id.clone(),
            actors: vec![actor_observation(session, unix_time_ms()?)],
            journal_head_sha256: session.journal_head_sha256.clone(),
            head_receipt: session.head_receipt.clone(),
        };
        self.require_store_current()?;
        Ok(response)
    }

    pub fn reset_session(
        &self,
        principal_sha256: &str,
        request: SessionMutationRequestV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        let request_sha256 = canonical_hosted_sha256(&request)?;
        let now = unix_time_ms()?;
        let session_id = request.credentials.session_id.clone();
        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal_sha256, &request.credentials)?;
        if let Some(receipt) = duplicate_commit(
            &state,
            &session_id,
            request.client_sequence,
            &request.client_request_id,
            &request_sha256,
        )? {
            return Ok(HostedResponseV2::Committed { receipt });
        }
        require_next_sequence(&state, &session_id, request.client_sequence)?;
        let session = state.sessions.get(&session_id).unwrap();
        if session.preparation.is_some() {
            return Err(reject(
                "session-preparing",
                "reset refuses while an operation is being prepared",
                true,
            ));
        }
        if session.operations.values().any(|operation| {
            matches!(
                operation.view.status,
                OperationStatusV2::Accepted | OperationStatusV2::Running
            )
        }) {
            return Err(reject(
                "session-busy",
                "reset refuses while an operation is accepted or running",
                true,
            ));
        }
        if session.status == SessionStatusV2::Executing
            || session.status == SessionStatusV2::Closing
            || session.status == SessionStatusV2::Closed
            || session.status == SessionStatusV2::Quarantined
        {
            return Err(reject(
                "session-reset-forbidden",
                format!("cannot reset a session in {:?} state", session.status),
                false,
            ));
        }
        if session
            .operations
            .values()
            .any(|operation| operation.view.status == OperationStatusV2::Ambiguous)
        {
            return Err(reject(
                "ambiguous-recovery-required",
                "an ambiguous operation can only be resolved by a warrant-gated recovery",
                false,
            ));
        }
        let next_generation = match session.actor_generation.as_ref() {
            Some(actor) => GenerationV1::new(
                actor
                    .generation()
                    .get()
                    .checked_add(1)
                    .context("actor generation overflow")?,
            )?,
            None => session.next_actor_generation,
        };
        let event = JournalEventV2::SessionReset {
            client_sequence: request.client_sequence,
            client_request_id: request.client_request_id.clone(),
            request_sha256: request_sha256.clone(),
            previous_actor_generation: session.actor_generation.clone(),
            next_actor_generation: next_generation,
        };
        let receipt = self.issue_next_entry(session, now, event)?;
        let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
        ensure_session_durable_capacity(&state, &session_id, needed)?;
        let written = self.inner.store.append_entry(&session_id, &receipt)?;
        state.durable_bytes += written;
        let session = state.sessions.get_mut(&session_id).unwrap();
        session.durable_bytes += written;
        session.actor_id = None;
        session.actor_generation = None;
        session.next_actor_generation = next_generation;
        session.actor_has_state = false;
        session.checkpoint = None;
        session.status = SessionStatusV2::Ready;
        apply_receipt_head(session, &receipt);
        record_commit(
            session,
            request.client_sequence,
            request.client_request_id,
            request_sha256,
            receipt.clone(),
        )?;
        // Retire the old evaluator mapping while the reset commit still owns
        // the global state lock. A concurrent Submit can now only create a
        // fresh evaluator under `next_generation`; it can never enqueue behind
        // a delayed Reset command on the pre-reset process.
        let sender = state.workers.remove(&session_id);
        drop(state);
        if let Some(sender) = sender {
            let _ = sender.send(ActorCommandV2::Close);
        }
        Ok(HostedResponseV2::Committed { receipt })
    }

    pub fn recover_session(
        &self,
        principal_sha256: &str,
        request: RecoverSessionRequestV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        request.warrant.validate()?;
        let warrant_sha256 = request.warrant.sha256()?;
        let request_sha256 = canonical_hosted_sha256(&request)?;
        let session_id = request.credentials.session_id.clone();
        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal_sha256, &request.credentials)?;
        if let Some(receipt) = duplicate_commit(
            &state,
            &session_id,
            request.client_sequence,
            &request.client_request_id,
            &request_sha256,
        )? {
            return Ok(HostedResponseV2::Committed { receipt });
        }
        require_next_sequence(&state, &session_id, request.client_sequence)?;
        let session = state.sessions.get(&session_id).unwrap();
        if session.status != SessionStatusV2::RecoveryRequired {
            return Err(reject(
                "recovery-not-required",
                "session is not awaiting recovery",
                false,
            ));
        }
        if session.preparation.is_some() {
            return Err(reject(
                "session-recovering",
                "another authenticated recovery handshake is already in progress",
                true,
            ));
        }
        if session.recovery_attempt.is_some() {
            return Err(reject(
                "recovery-attempt-unterminated",
                "a durable recovery attempt has no terminal record; the session is isolated until startup repair can append a signed refusal",
                false,
            ));
        }
        if request.warrant.session_id != session_id {
            return Err(reject(
                "recovery-warrant-mismatch",
                "recovery warrant names a different session",
                false,
            ));
        }
        if request.warrant.evidence_sha256 != session.journal_head_sha256 {
            return Err(reject(
                "recovery-warrant-mismatch",
                "recovery warrant does not bind the current signed journal head",
                false,
            ));
        }
        validate_recovery_trigger(session, &request.warrant.trigger)?;
        let recovery_reservation = PreparationReservationV2 {
            request_sha256: request_sha256.clone(),
            client_sequence: request.client_sequence,
            client_request_id: request.client_request_id.clone(),
            operation_id: request.warrant.warrant_id.clone(),
            journal_head_sha256: session.journal_head_sha256.clone(),
        };
        let mut context = PlacementAuthorizationContextV2 {
            node_id: self.inner.config.node_id.clone(),
            node_generation: self.inner.config.node_generation,
            principal_sha256: principal_sha256.to_owned(),
            state_session: session.state_session.clone(),
            session_state_tier: session.state_tier,
            client_request_id: request.client_request_id.clone(),
            client_sequence: request.client_sequence,
            purpose: PlacementPurposeV2::Recover,
            operation_sha256: None,
            recovery_warrant_sha256: Some(warrant_sha256.clone()),
            state_quota_generation: session.state_quota_generation,
            state_quota_limits: session.state_quota_limits.clone(),
            state_reservation: session.state_reservation.clone(),
            current_actor_generation: session.actor_generation.clone(),
            next_actor_generation: session.next_actor_generation,
            prepared_fragment: None,
            expected_session_identity: Some(session.placement_identity.clone()),
            // Sampled only after releasing the global runtime mutex so an
            // authority cannot validate against time spent waiting for it.
            now_unix_ms: 0,
        };
        state.sessions.get_mut(&session_id).unwrap().preparation =
            Some(recovery_reservation.clone());
        drop(state);

        context.now_unix_ms = match unix_time_ms() {
            Ok(now) => now,
            Err(error) => {
                clear_preparation(&self.inner, &session_id, &recovery_reservation)?;
                return Err(error);
            }
        };
        let authorized = match self
            .inner
            .authorizer
            .authorize(&context, &request.placement_lease)
        {
            Ok(authorized) => authorized,
            Err(error) => {
                clear_preparation(&self.inner, &session_id, &recovery_reservation)?;
                return Err(reject("placement-denied", format!("{error:#}"), false));
            }
        };

        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal_sha256, &request.credentials)?;
        let unchanged = state.sessions.get(&session_id).is_some_and(|session| {
            recovery_coordinates_unchanged(session, &recovery_reservation, &request)
        });
        if !unchanged {
            if state
                .sessions
                .get(&session_id)
                .and_then(|session| session.preparation.as_ref())
                == Some(&recovery_reservation)
            {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
            }
            return Err(reject(
                "authorization-stale",
                "session coordinates changed while recovery authority was evaluated",
                true,
            ));
        }
        if state.used_lease_nonces.contains(&authorized.lease_nonce) {
            state.sessions.get_mut(&session_id).unwrap().preparation = None;
            return Err(reject(
                "placement-lease-reused",
                "hosted placement lease nonce was already consumed",
                false,
            ));
        }
        drop(state);

        context.now_unix_ms = match unix_time_ms() {
            Ok(now) => now,
            Err(error) => {
                clear_preparation(&self.inner, &session_id, &recovery_reservation)?;
                return Err(error);
            }
        };
        let commit_authorized = match self
            .inner
            .authorizer
            .authorize(&context, &request.placement_lease)
        {
            Ok(authorized) => authorized,
            Err(error) => {
                clear_preparation(&self.inner, &session_id, &recovery_reservation)?;
                return Err(reject("placement-expired", format!("{error:#}"), false));
            }
        };

        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal_sha256, &request.credentials)?;
        let unchanged = state.sessions.get(&session_id).is_some_and(|session| {
            recovery_coordinates_unchanged(session, &recovery_reservation, &request)
        });
        if !unchanged {
            if state
                .sessions
                .get(&session_id)
                .and_then(|session| session.preparation.as_ref())
                == Some(&recovery_reservation)
            {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
            }
            return Err(reject(
                "commit-stale",
                "session coordinates changed during final recovery authorization",
                true,
            ));
        }
        if commit_authorized != authorized {
            state.sessions.get_mut(&session_id).unwrap().preparation = None;
            return Err(reject(
                "placement-changed",
                "recovery placement authorization changed between validation passes",
                false,
            ));
        }
        if state.used_lease_nonces.contains(&authorized.lease_nonce) {
            state.sessions.get_mut(&session_id).unwrap().preparation = None;
            return Err(reject(
                "placement-lease-reused",
                "hosted placement lease nonce was already consumed",
                false,
            ));
        }
        let commit_now = match unix_time_ms() {
            Ok(now) => now,
            Err(error) => {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(error);
            }
        };
        if commit_now >= authorized.expires_at_unix_ms {
            state.sessions.get_mut(&session_id).unwrap().preparation = None;
            return Err(reject(
                "placement-expired",
                "recovery placement authority expired before backend restore",
                false,
            ));
        }

        if state.sessions[&session_id].state_tier == SessionStateTierV2::CheckpointRestore {
            let snapshot = match checkpoint_for_session(
                &self.inner.store,
                state.sessions.get(&session_id).unwrap(),
            ) {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => {
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        None,
                        "recovery-checkpoint-missing",
                        "checkpoint recovery has no durable actor snapshot to restore",
                    )
                }
                Err(error) => {
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        None,
                        "recovery-checkpoint-invalid",
                        &format!("durable actor checkpoint cannot be restored: {error:#}"),
                    )
                }
            };
            let session = state.sessions.get(&session_id).unwrap();
            let previous_actor_generation = match session.actor_generation.clone() {
                Some(actor) => actor,
                None => {
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        None,
                        "recovery-actor-missing",
                        "checkpoint recovery requires an established actor generation",
                    )
                }
            };
            let next_client_sequence = match request.client_sequence.checked_add(1) {
                Some(sequence) => sequence,
                None => {
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        None,
                        "client-sequence-exhausted",
                        "checkpoint recovery cannot advance the client mutation sequence",
                    )
                }
            };
            let actor_generation = match successor_actor_generation(&previous_actor_generation) {
                Ok(actor) => actor,
                Err(error) => {
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        None,
                        "recovery-generation-invalid",
                        &format!("checkpoint recovery cannot advance actor generation: {error:#}"),
                    )
                }
            };
            if actor_generation.generation() != session.next_actor_generation {
                return self.commit_recovery_refusal_locked(
                    &mut state,
                    &session_id,
                    &request,
                    &request_sha256,
                    &warrant_sha256,
                    &authorized,
                    None,
                    "recovery-generation-invalid",
                    "checkpoint recovery does not name the uniquely fenced successor generation",
                );
            }
            let probes = match recovery_probes(
                &session_id,
                &request.client_request_id,
                &snapshot,
                &actor_generation,
            ) {
                Ok(probes) => probes,
                Err(error) => {
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        None,
                        "recovery-probe-unsupported",
                        &format!(
                            "no exact state-neutral restore handshake is available: {error:#}"
                        ),
                    )
                }
            };
            let checkpoint_sha256 = session
                .checkpoint
                .as_ref()
                .expect("checkpoint snapshot and record were validated together")
                .snapshot_sha256
                .clone();
            let checkpoint_bytes = session
                .checkpoint
                .as_ref()
                .expect("checkpoint snapshot and record were validated together")
                .snapshot_bytes;
            let snapshot_limit = session.state_reservation.snapshot_bytes_per_actor();

            // Checkpoint loading and probe construction can consume most of a
            // placement lease's remaining lifetime. Reauthorize after that
            // work without holding the global runtime mutex, then recapture
            // wall time once the exact session coordinates are locked again.
            // A physical generation is not allocated until this final fresh
            // authorization has succeeded.
            drop(state);
            context.now_unix_ms = match unix_time_ms() {
                Ok(now) => now,
                Err(error) => {
                    clear_preparation(&self.inner, &session_id, &recovery_reservation)?;
                    return Err(error);
                }
            };
            let attempt_authorized = match self
                .inner
                .authorizer
                .authorize(&context, &request.placement_lease)
            {
                Ok(authorized) => authorized,
                Err(error) => {
                    clear_preparation(&self.inner, &session_id, &recovery_reservation)?;
                    return Err(reject("placement-expired", format!("{error:#}"), false));
                }
            };
            let mut state = self.lock_state()?;
            authenticate_locked(&state, principal_sha256, &request.credentials)?;
            if !state.sessions.get(&session_id).is_some_and(|session| {
                recovery_coordinates_unchanged(session, &recovery_reservation, &request)
            }) {
                if state
                    .sessions
                    .get(&session_id)
                    .and_then(|session| session.preparation.as_ref())
                    == Some(&recovery_reservation)
                {
                    state.sessions.get_mut(&session_id).unwrap().preparation = None;
                }
                return Err(reject(
                    "commit-stale",
                    "session coordinates changed before durable recovery allocation",
                    true,
                ));
            }
            if attempt_authorized != authorized {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(reject(
                    "placement-changed",
                    "recovery placement authorization changed before durable allocation",
                    false,
                ));
            }
            if state.used_lease_nonces.contains(&authorized.lease_nonce) {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(reject(
                    "placement-lease-reused",
                    "hosted placement lease nonce was already consumed",
                    false,
                ));
            }
            let attempt_now = match unix_time_ms() {
                Ok(now) => now,
                Err(error) => {
                    state.sessions.get_mut(&session_id).unwrap().preparation = None;
                    return Err(error);
                }
            };
            if attempt_now >= placement_freshness_deadline(&request.placement_lease) {
                state.sessions.get_mut(&session_id).unwrap().preparation = None;
                return Err(reject(
                    "placement-expired",
                    "recovery placement evidence expired before durable actor allocation",
                    false,
                ));
            }
            let session = state.sessions.get(&session_id).unwrap();

            // Fence the replacement's physical generation and consume its
            // placement lease before a process can be spawned. A crash from
            // this point onward can therefore never reuse this generation;
            // startup turns an unterminated attempt into a signed refusal.
            let attempt_event = JournalEventV2::RecoveryAttemptStarted {
                client_sequence: request.client_sequence,
                client_request_id: request.client_request_id.clone(),
                request_sha256: request_sha256.clone(),
                warrant_sha256: warrant_sha256.clone(),
                placement_lease_sha256: authorized.lease_sha256.clone(),
                placement_lease_nonce: authorized.lease_nonce.clone(),
                trigger: request.warrant.trigger.clone(),
                previous_actor_generation: previous_actor_generation.clone(),
                attempted_actor_generation: actor_generation.clone(),
                checkpoint_sha256: checkpoint_sha256.clone(),
                checkpoint_bytes,
            };
            let prepared_attempt = (|| {
                let attempt_receipt = self.issue_next_entry(session, attempt_now, attempt_event)?;
                let attempt_bytes = self.inner.store.encoded_frame_bytes(&attempt_receipt)?;
                let attempt_capacity = attempt_bytes
                    .checked_add(RECOVERY_TERMINAL_HEADROOM_RESERVATION)
                    .context("recovery attempt capacity reservation overflow")?;
                ensure_session_durable_capacity(&state, &session_id, attempt_capacity)?;
                let next_durable_bytes = state
                    .durable_bytes
                    .checked_add(attempt_bytes)
                    .context("hosted durable-byte accounting overflow")?;
                let next_session_bytes = session
                    .durable_bytes
                    .checked_add(attempt_bytes)
                    .context("hosted session-byte accounting overflow")?;
                let next_reserved_durable_bytes = state
                    .reserved_durable_bytes
                    .checked_add(RECOVERY_TERMINAL_HEADROOM_RESERVATION)
                    .context("hosted recovery reservation accounting overflow")?;
                let attempted_next_actor_generation =
                    successor_actor_generation(&actor_generation)?.generation();
                Ok::<_, anyhow::Error>((
                    attempt_receipt,
                    attempt_bytes,
                    next_durable_bytes,
                    next_session_bytes,
                    next_reserved_durable_bytes,
                    attempted_next_actor_generation,
                ))
            })();
            let (
                attempt_receipt,
                attempt_bytes,
                next_durable_bytes,
                next_session_bytes,
                next_reserved_durable_bytes,
                attempted_next_actor_generation,
            ) = match prepared_attempt {
                Ok(prepared) => prepared,
                Err(error) => {
                    state.sessions.get_mut(&session_id).unwrap().preparation = None;
                    return Err(error);
                }
            };
            let attempt_written = match self.inner.store.append_entry(&session_id, &attempt_receipt)
            {
                Ok(written) => written,
                Err(error) => {
                    quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                    return Err(error);
                }
            };
            if attempt_written != attempt_bytes {
                let error =
                    anyhow::anyhow!("encoded recovery-attempt journal frame length changed");
                quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                return Err(error);
            }
            state.durable_bytes = next_durable_bytes;
            state.reserved_durable_bytes = next_reserved_durable_bytes;
            state
                .used_lease_nonces
                .insert(authorized.lease_nonce.clone());
            let session = state.sessions.get_mut(&session_id).unwrap();
            session.durable_bytes = next_session_bytes;
            session.actor_id = None;
            session.actor_generation = Some(actor_generation.clone());
            session.next_actor_generation = attempted_next_actor_generation;
            session.actor_has_state = false;
            session.recovery_attempt = Some(RecoveryAttemptV2 {
                receipt_sha256: attempt_receipt.entry_sha256.clone(),
                client_sequence: request.client_sequence,
                client_request_id: request.client_request_id.clone(),
                request_sha256: request_sha256.clone(),
                warrant_sha256: warrant_sha256.clone(),
                placement_lease_sha256: authorized.lease_sha256.clone(),
                placement_lease_nonce: authorized.lease_nonce.clone(),
                trigger: request.warrant.trigger.clone(),
                previous_actor_generation: previous_actor_generation.clone(),
                attempted_actor_generation: actor_generation.clone(),
                checkpoint_sha256: checkpoint_sha256.clone(),
                checkpoint_bytes,
                reserved_bytes: RECOVERY_TERMINAL_HEADROOM_RESERVATION,
            });
            apply_receipt_head(session, &attempt_receipt);

            if let Some(sender) = state.workers.remove(&session_id) {
                let _ = sender.send(ActorCommandV2::Close);
            }
            let replacement = match spawn_actor(
                &self.inner,
                &session_id,
                SessionStateTierV2::CheckpointRestore,
            ) {
                Ok(sender) => sender,
                Err(error) => {
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        Some(&actor_generation),
                        "actor-unavailable",
                        &format!("checkpoint recovery actor could not be started: {error:#}"),
                    )
                }
            };
            let handshake_now = match unix_time_ms() {
                Ok(now) => now,
                Err(error) => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        Some(&actor_generation),
                        "clock-unavailable",
                        &format!("cannot bound checkpoint recovery handshake: {error:#}"),
                    );
                }
            };
            let handshake_ms = authorized
                .expires_at_unix_ms
                .saturating_sub(handshake_now)
                .min(30_000);
            if handshake_ms == 0 {
                let _ = replacement.send(ActorCommandV2::Close);
                return self.commit_recovery_refusal_locked(
                    &mut state,
                    &session_id,
                    &request,
                    &request_sha256,
                    &warrant_sha256,
                    &authorized,
                    Some(&actor_generation),
                    "placement-expired",
                    "recovery placement authority expired before backend restore",
                );
            }
            let deadline = match Instant::now().checked_add(Duration::from_millis(handshake_ms)) {
                Some(deadline) => deadline,
                None => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        Some(&actor_generation),
                        "deadline-conversion-failed",
                        "checkpoint recovery deadline cannot be represented by the local monotonic clock",
                    );
                }
            };
            let (reply, acknowledgement) = mpsc::channel();
            if replacement
                .send(ActorCommandV2::Recover {
                    snapshot,
                    snapshot_limit,
                    probes,
                    deadline,
                    reply,
                })
                .is_err()
            {
                return self.commit_recovery_refusal_locked(
                    &mut state,
                    &session_id,
                    &request,
                    &request_sha256,
                    &warrant_sha256,
                    &authorized,
                    Some(&actor_generation),
                    "actor-unavailable",
                    "checkpoint recovery actor stopped before backend restore",
                );
            }
            drop(state);

            let acknowledged = acknowledgement
                .recv_timeout(Duration::from_millis(handshake_ms))
                .map_err(|_| {
                    "backend restore did not acknowledge before the placement lease deadline"
                        .to_owned()
                })
                .and_then(|result| result);

            let mut state = match self.lock_state() {
                Ok(state) => state,
                Err(error) => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    return Err(error);
                }
            };
            if let Err(error) = authenticate_locked(&state, principal_sha256, &request.credentials)
            {
                let _ = replacement.send(ActorCommandV2::Close);
                quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                return Err(error);
            }
            let unchanged = state.sessions.get(&session_id).is_some_and(|session| {
                recovery_attempt_coordinates_unchanged(
                    session,
                    &recovery_reservation,
                    &request,
                    &actor_generation,
                )
            });
            if !unchanged {
                let _ = replacement.send(ActorCommandV2::Close);
                let error = reject(
                    "recovery-stale",
                    "session coordinates changed during backend restore acknowledgement",
                    true,
                );
                quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                return Err(error);
            }
            if let Err(message) = acknowledged {
                let _ = replacement.send(ActorCommandV2::Close);
                return self.commit_recovery_refusal_locked(
                    &mut state,
                    &session_id,
                    &request,
                    &request_sha256,
                    &warrant_sha256,
                    &authorized,
                    Some(&actor_generation),
                    "state-restore-failed",
                    &message,
                );
            }
            let committed_now = match unix_time_ms() {
                Ok(now) => now,
                Err(error) => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        Some(&actor_generation),
                        "clock-unavailable",
                        &format!("cannot timestamp checkpoint recovery acknowledgement: {error:#}"),
                    );
                }
            };
            if committed_now >= authorized.expires_at_unix_ms {
                let _ = replacement.send(ActorCommandV2::Close);
                return self.commit_recovery_refusal_locked(
                    &mut state,
                    &session_id,
                    &request,
                    &request_sha256,
                    &warrant_sha256,
                    &authorized,
                    Some(&actor_generation),
                    "placement-expired",
                    "recovery placement authority expired before durable acknowledgement",
                );
            }

            let actor_id = match fresh_identifier("actor") {
                Ok(actor_id) => actor_id,
                Err(error) => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    return self.commit_recovery_refusal_locked(
                        &mut state,
                        &session_id,
                        &request,
                        &request_sha256,
                        &warrant_sha256,
                        &authorized,
                        Some(&actor_generation),
                        "actor-identity-unavailable",
                        &format!("cannot identify acknowledged replacement actor: {error:#}"),
                    );
                }
            };
            let recovery_attempt_sha256 = state.sessions[&session_id]
                .recovery_attempt
                .as_ref()
                .context("acknowledged recovery has no durable attempt allocation")?
                .receipt_sha256
                .clone();
            let event = JournalEventV2::RecoveryCommitted {
                client_sequence: request.client_sequence,
                client_request_id: request.client_request_id.clone(),
                request_sha256: request_sha256.clone(),
                warrant_sha256,
                placement_lease_sha256: authorized.lease_sha256,
                placement_lease_nonce: authorized.lease_nonce.clone(),
                recovery_attempt_sha256,
                trigger: request.warrant.trigger.clone(),
                previous_actor_generation,
                actor_generation: actor_generation.clone(),
                actor_id: actor_id.clone(),
                checkpoint_sha256: Some(checkpoint_sha256),
                checkpoint_bytes: Some(checkpoint_bytes),
            };
            let session = state.sessions.get(&session_id).unwrap();
            let receipt = match self.issue_next_entry(session, committed_now, event) {
                Ok(receipt) => receipt,
                Err(error) => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                    return Err(error);
                }
            };
            let needed = match self.inner.store.encoded_frame_bytes(&receipt) {
                Ok(needed) => needed,
                Err(error) => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                    return Err(error);
                }
            };
            let recovery_reserved = state.sessions[&session_id]
                .recovery_attempt
                .as_ref()
                .expect("recovery attempt was checked above")
                .reserved_bytes;
            if needed > recovery_reserved {
                let _ = replacement.send(ActorCommandV2::Close);
                let error = anyhow::anyhow!(
                    "recovery commit requires {needed} bytes, exceeding reserved terminal headroom {recovery_reserved}"
                );
                quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                return Err(error);
            }
            let next_durable_bytes = match state.durable_bytes.checked_add(needed) {
                Some(bytes) => bytes,
                None => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    let error = anyhow::anyhow!("hosted durable-byte accounting overflow");
                    quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                    return Err(error);
                }
            };
            let next_session_bytes = match state.sessions[&session_id]
                .durable_bytes
                .checked_add(needed)
            {
                Some(bytes) => bytes,
                None => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    let error = anyhow::anyhow!("hosted session-byte accounting overflow");
                    quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                    return Err(error);
                }
            };
            let written = match self.inner.store.append_entry(&session_id, &receipt) {
                Ok(written) => written,
                Err(error) => {
                    let _ = replacement.send(ActorCommandV2::Close);
                    quarantine_recovery_attempt_locked(&mut state, &session_id, &error);
                    return Err(error);
                }
            };
            debug_assert_eq!(written, needed, "encoded journal frame length changed");
            state.durable_bytes = next_durable_bytes;
            state.used_lease_nonces.insert(authorized.lease_nonce);
            state.reserved_durable_bytes = state
                .reserved_durable_bytes
                .saturating_sub(recovery_reserved);
            let session = state.sessions.get_mut(&session_id).unwrap();
            session.durable_bytes = next_session_bytes;
            session.preparation = None;
            session.recovery_attempt = None;
            session.actor_id = Some(actor_id);
            session.actor_generation = Some(actor_generation.clone());
            session.next_actor_generation = actor_generation.generation();
            session
                .checkpoint
                .as_mut()
                .expect("checkpoint was validated before recovery")
                .actor_generation = actor_generation;
            session.actor_has_state = true;
            session.status = SessionStatusV2::Ready;
            if let RecoveryTriggerV2::AmbiguousOperation { operation_id, .. } =
                &request.warrant.trigger
            {
                let operation = session
                    .operations
                    .get_mut(operation_id)
                    .expect("ambiguous recovery operation checked before recovery");
                operation.view.status = OperationStatusV2::Failed;
                operation.view.finished_unix_ms = Some(committed_now);
                operation.view.outcome = Some(OperationOutcomeV2::failed(
                    OperationFailureStageV2::Infrastructure,
                    "ambiguous-attempt-recovered",
                    "attempt outcome was ambiguous; authenticated recovery restored the last durable actor checkpoint without replay",
                ));
                operation.reserved_bytes = 0;
            }
            apply_receipt_head(session, &receipt);
            session.commits.insert(
                request.client_sequence,
                ClientCommitV2 {
                    request_id: request.client_request_id,
                    request_sha256,
                    receipt: receipt.clone(),
                },
            );
            session.next_client_sequence = next_client_sequence;
            state.workers.insert(session_id, replacement);
            return Ok(HostedResponseV2::Committed { receipt });
        }

        // Replay and process-retained tiers deliberately remain fail-closed:
        // V2 has no automatic replay/publication adapter and never treats a
        // signed recovery request as permission to guess at actor state.
        self.commit_recovery_refusal_locked(
            &mut state,
            &session_id,
            &request,
            &request_sha256,
            &warrant_sha256,
            &authorized,
            None,
            "recovery-tier-unsupported",
            "authenticated recovery is implemented only for checkpoint/restore sessions; replay and live-only tiers remain fail-closed",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_recovery_refusal_locked(
        &self,
        state: &mut RuntimeStateV2,
        session_id: &str,
        request: &RecoverSessionRequestV2,
        request_sha256: &str,
        warrant_sha256: &str,
        authorized: &AuthorizedPlacementV2,
        attempted_actor_generation: Option<&ActorGenerationIdV1>,
        code: &str,
        message: &str,
    ) -> Result<HostedResponseV2> {
        let recovery_reservation = state
            .sessions
            .get(session_id)
            .and_then(|session| session.preparation.clone())
            .context("recovery refusal has no matching preparation reservation")?;
        let durable_refusal = (|| {
            let session = state
                .sessions
                .get(session_id)
                .context("session disappeared before recovery refusal")?;
            if session.preparation.as_ref() != Some(&recovery_reservation) {
                bail!("recovery refusal preparation reservation changed");
            }
            if session.next_client_sequence != request.client_sequence {
                bail!("recovery refusal client sequence changed");
            }
            let refusal_now = unix_time_ms()?;
            // Once a durable attempt exists its terminal refusal must remain
            // appendable even after authority expiry. Before allocation,
            // however, an expired proof cannot consume the client sequence or
            // lease nonce through a newly signed refusal.
            if attempted_actor_generation.is_none()
                && refusal_now >= placement_freshness_deadline(&request.placement_lease)
            {
                return Err(reject(
                    "placement-expired",
                    "recovery placement evidence expired before durable refusal",
                    false,
                ));
            }
            let recovery_attempt_sha256 = match attempted_actor_generation {
                Some(attempted) => {
                    let attempt = session
                        .recovery_attempt
                        .as_ref()
                        .context("spawned recovery refusal has no durable attempt allocation")?;
                    if attempt.attempted_actor_generation != *attempted
                        || attempt.client_sequence != request.client_sequence
                        || attempt.client_request_id != request.client_request_id
                        || attempt.request_sha256 != request_sha256
                        || attempt.warrant_sha256 != warrant_sha256
                        || attempt.placement_lease_sha256 != authorized.lease_sha256
                        || attempt.placement_lease_nonce != authorized.lease_nonce
                        || attempt.trigger != request.warrant.trigger
                        || session.actor_generation.as_ref() != Some(attempted)
                    {
                        bail!(
                            "spawned recovery refusal differs from its durable attempt allocation"
                        );
                    }
                    Some(attempt.receipt_sha256.clone())
                }
                None => {
                    if session.recovery_attempt.is_some() {
                        bail!("recovery refusal omits an existing durable attempt allocation");
                    }
                    None
                }
            };
            let event = JournalEventV2::RecoveryRefused {
                client_sequence: request.client_sequence,
                client_request_id: request.client_request_id.clone(),
                request_sha256: request_sha256.to_owned(),
                warrant_sha256: warrant_sha256.to_owned(),
                placement_lease_sha256: authorized.lease_sha256.clone(),
                placement_lease_nonce: authorized.lease_nonce.clone(),
                recovery_attempt_sha256,
                attempted_actor_generation: attempted_actor_generation.cloned(),
                code: code.to_owned(),
                message: bounded_durable_text(message),
            };
            let receipt = self.issue_next_entry(session, refusal_now, event)?;
            let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
            if let Some(attempt) = &session.recovery_attempt {
                if needed > attempt.reserved_bytes {
                    bail!(
                        "recovery refusal requires {needed} bytes, exceeding reserved terminal headroom {}",
                        attempt.reserved_bytes
                    );
                }
            } else {
                ensure_session_durable_capacity(state, session_id, needed)?;
            }
            let next_durable_bytes = state
                .durable_bytes
                .checked_add(needed)
                .context("hosted durable-byte accounting overflow")?;
            let next_session_bytes = session
                .durable_bytes
                .checked_add(needed)
                .context("hosted session-byte accounting overflow")?;
            let next_client_sequence = request
                .client_sequence
                .checked_add(1)
                .context("hosted V2 client sequence overflow")?;
            let written = self.inner.store.append_entry(session_id, &receipt)?;
            debug_assert_eq!(written, needed, "encoded journal frame length changed");
            Ok((
                receipt,
                next_durable_bytes,
                next_session_bytes,
                next_client_sequence,
            ))
        })();
        let (receipt, next_durable_bytes, next_session_bytes, next_client_sequence) =
            match durable_refusal {
                Ok(committed) => committed,
                Err(error) => {
                    let mut quarantine_attempt = false;
                    if let Some(session) = state.sessions.get_mut(session_id) {
                        if attempted_actor_generation.is_some() {
                            session.preparation = None;
                            session.status = SessionStatusV2::Quarantined;
                            quarantine_attempt = true;
                        } else if session.preparation.as_ref() == Some(&recovery_reservation) {
                            session.preparation = None;
                            session.status = SessionStatusV2::RecoveryRequired;
                        }
                    }
                    if quarantine_attempt {
                        state.workers.remove(session_id);
                        state.unreadable_sessions.push(format!(
                            "{session_id}: durable recovery attempt could not be terminated: {error:#}"
                        ));
                    }
                    return Err(error);
                }
            };
        state.durable_bytes = next_durable_bytes;
        if let Some(attempt) = state.sessions[session_id].recovery_attempt.as_ref() {
            state.reserved_durable_bytes = state
                .reserved_durable_bytes
                .saturating_sub(attempt.reserved_bytes);
        }
        state
            .used_lease_nonces
            .insert(authorized.lease_nonce.clone());
        let session = state.sessions.get_mut(session_id).unwrap();
        debug_assert_eq!(
            session.preparation.as_ref(),
            Some(&recovery_reservation),
            "recovery refusal reservation changed while the runtime lock was held"
        );
        session.durable_bytes = next_session_bytes;
        session.preparation = None;
        session.status = SessionStatusV2::RecoveryRequired;
        session.recovery_attempt = None;
        apply_receipt_head(session, &receipt);
        session.commits.insert(
            request.client_sequence,
            ClientCommitV2 {
                request_id: request.client_request_id.clone(),
                request_sha256: request_sha256.to_owned(),
                receipt: receipt.clone(),
            },
        );
        session.next_client_sequence = next_client_sequence;
        Ok(HostedResponseV2::Committed { receipt })
    }

    pub fn close_session(
        &self,
        principal_sha256: &str,
        request: SessionMutationRequestV2,
    ) -> Result<HostedResponseV2> {
        let _call = self.inner.begin_call()?;
        self.require_store_current()?;
        let request_sha256 = canonical_hosted_sha256(&request)?;
        let now = unix_time_ms()?;
        let session_id = request.credentials.session_id.clone();
        let mut state = self.lock_state()?;
        authenticate_locked(&state, principal_sha256, &request.credentials)?;
        if let Some(receipt) = duplicate_commit(
            &state,
            &session_id,
            request.client_sequence,
            &request.client_request_id,
            &request_sha256,
        )? {
            return Ok(HostedResponseV2::Committed { receipt });
        }
        require_next_sequence(&state, &session_id, request.client_sequence)?;
        let session = state.sessions.get(&session_id).unwrap();
        if session.preparation.is_some() {
            return Err(reject(
                "session-preparing",
                "close refuses while an operation is being prepared",
                true,
            ));
        }
        if session.operations.values().any(|operation| {
            matches!(
                operation.view.status,
                OperationStatusV2::Accepted | OperationStatusV2::Running
            )
        }) {
            return Err(reject(
                "session-busy",
                "close refuses while an operation is accepted or running",
                true,
            ));
        }
        if session.status == SessionStatusV2::Executing {
            return Err(reject(
                "session-busy",
                "close refuses while an operation is executing",
                true,
            ));
        }
        if session.status == SessionStatusV2::Closed {
            return Err(reject("session-closed", "session is already closed", false));
        }
        let event = JournalEventV2::SessionClosed {
            client_sequence: request.client_sequence,
            client_request_id: request.client_request_id.clone(),
            request_sha256: request_sha256.clone(),
            actor_generation: session.actor_generation.clone(),
        };
        let receipt = self.issue_next_entry(session, now, event)?;
        let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
        ensure_close_durable_capacity(&state, &session_id, needed)?;
        let written = self.inner.store.append_entry(&session_id, &receipt)?;
        state.durable_bytes += written;
        let released_state_bytes = state.sessions[&session_id].state_reservation.state_bytes();
        state.state_bytes_reserved = state
            .state_bytes_reserved
            .saturating_sub(released_state_bytes);
        let session = state.sessions.get_mut(&session_id).unwrap();
        session.durable_bytes += written;
        session.status = SessionStatusV2::Closed;
        apply_receipt_head(session, &receipt);
        record_commit(
            session,
            request.client_sequence,
            request.client_request_id,
            request_sha256,
            receipt.clone(),
        )?;
        if let Some(sender) = state.workers.remove(&session_id) {
            let _ = sender.send(ActorCommandV2::Close);
        }
        Ok(HostedResponseV2::Committed { receipt })
    }

    fn issue_entry(
        &self,
        session_id: &str,
        sequence: u64,
        previous: Option<String>,
        now: u64,
        event: JournalEventV2,
    ) -> Result<SignedJournalEntryV2> {
        self.inner.store.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: session_id.to_owned(),
            sequence,
            previous_entry_sha256: previous,
            recorded_unix_ms: now,
            event,
        })
    }

    fn issue_next_entry(
        &self,
        session: &SessionRecordV2,
        now: u64,
        event: JournalEventV2,
    ) -> Result<SignedJournalEntryV2> {
        self.issue_entry(
            &session.session_id,
            session
                .journal_sequence
                .checked_add(1)
                .context("hosted journal sequence overflow")?,
            Some(session.journal_head_sha256.clone()),
            now,
            event,
        )
    }

    fn journal_placement_refusal(
        &self,
        state: &mut RuntimeStateV2,
        authorized: &AuthorizedPlacementV2,
        command: &HostedCommandBindingV2,
        code: &str,
        message: &str,
    ) -> Result<()> {
        if state.used_lease_nonces.contains(&authorized.lease_nonce) {
            return Ok(());
        }
        let state_session_sha256 = authorized
            .state_session
            .semantic_digest()
            .context("failed to digest refused state session")?
            .to_string();
        let event = JournalEventV2::PlacementLeaseRefused {
            state_session_sha256,
            placement_lease_sha256: authorized.lease_sha256.clone(),
            placement_lease_nonce: authorized.lease_nonce.clone(),
            hosted_command_sha256: command.semantic_digest()?.to_string(),
            code: code.to_owned(),
            message: bounded_durable_text(message),
        };
        let now = unix_time_ms()?;
        let receipt = self.issue_entry(
            super::store::AUTHORITY_JOURNAL_ID_V2,
            state
                .authority_journal_sequence
                .checked_add(1)
                .context("hosted authority journal sequence overflow")?,
            state.authority_journal_head_sha256.clone(),
            now,
            event,
        )?;
        let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
        let projected = reserved_state_capacity(state)?
            .checked_add(needed)
            .context("hosted refusal journal capacity overflow")?;
        if projected > self.inner.config.state_quotas.max_state_bytes_total() {
            bail!(
                "cannot durably consume refused placement nonce: state-byte capacity is exhausted"
            );
        }
        let written = self.inner.store.append_authority_entry(&receipt)?;
        state.durable_bytes = state
            .durable_bytes
            .checked_add(written)
            .context("hosted durable-byte accounting overflow")?;
        state.authority_journal_sequence = receipt.entry.sequence;
        state.authority_journal_head_sha256 = Some(receipt.entry_sha256);
        state
            .used_lease_nonces
            .insert(authorized.lease_nonce.clone());
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RuntimeStateV2>> {
        self.inner
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))
    }

    fn load_durable_sessions(&self) -> Result<()> {
        let authority_journal = self.inner.store.read_authority_journal()?;
        if let Some(corruption) = authority_journal.corruption {
            bail!("hosted placement-authority journal is corrupt: {corruption}");
        }
        let ids = self.inner.store.list_session_ids()?;
        let mut state = self.lock_state()?;
        for entry in authority_journal.entries {
            match &entry.entry.event {
                JournalEventV2::PlacementLeaseRefused { .. } => {
                    let nonce = entry
                        .entry
                        .event
                        .placement_lease_nonce()
                        .context("placement refusal omits lease nonce")?;
                    state.used_lease_nonces.insert(nonce.to_owned());
                }
                event @ JournalEventV2::ClosedSessionGcAuthorized { .. } => {
                    let retired_session_id = event.retired_session_id().context(
                        "closed-session GC authorization omits retired session identity",
                    )?;
                    if !state
                        .retired_session_ids
                        .insert(retired_session_id.to_owned())
                    {
                        bail!(
                            "hosted placement-authority journal retires session `{retired_session_id}` more than once"
                        );
                    }
                    let archived = self.inner.store.read_closed_session_gc_archive(event)?;
                    for archived_entry in archived.entries {
                        if let Some(nonce) = archived_entry.entry.event.placement_lease_nonce() {
                            state.used_lease_nonces.insert(nonce.to_owned());
                        }
                    }
                }
                JournalEventV2::ClosedSessionGcCompleted { .. }
                | JournalEventV2::JournalTailRepaired { .. } => {}
                _ => bail!("hosted placement-authority journal contains a session event"),
            }
            state.authority_journal_sequence = entry.entry.sequence;
            state.authority_journal_head_sha256 = Some(entry.entry_sha256);
        }
        for session_id in ids {
            if state.retired_session_ids.contains(&session_id) {
                continue;
            }
            let journal = match self.inner.store.read_journal(&session_id) {
                Ok(journal) => journal,
                Err(error) => {
                    state
                        .unreadable_sessions
                        .push(format!("{session_id}: {error:#}"));
                    continue;
                }
            };
            if journal.entries.is_empty() {
                state
                    .unreadable_sessions
                    .push(format!("{session_id}: empty journal"));
                continue;
            }
            match reconstruct_session(
                &self.inner.config.node_id,
                &self.inner.store,
                &journal.entries,
            ) {
                Ok(mut session) => {
                    let canonical_session_id = session
                        .state_session
                        .semantic_digest()
                        .context("failed to digest durable state session")?
                        .to_string();
                    if canonical_session_id != session_id
                        || session.state_session.node_id() != self.inner.config.node_id
                        || session.state_session.node_generation()
                            != self.inner.config.node_generation
                        || session.state_quota_generation
                            != self.inner.config.state_quota_generation
                        || session.state_quota_limits != self.inner.config.state_quotas
                    {
                        state.unreadable_sessions.push(format!(
                            "{session_id}: durable state identity or quota generation does not match node configuration"
                        ));
                        continue;
                    }
                    if let Err(error) = session
                        .state_reservation
                        .validate_against(&self.inner.config.state_quotas)
                    {
                        state
                            .unreadable_sessions
                            .push(format!("{session_id}: {error}"));
                        continue;
                    }
                    session.durable_bytes = self.inner.store.session_durable_bytes(&session_id)?;
                    if session.durable_bytes > session.state_reservation.state_bytes() {
                        state.unreadable_sessions.push(format!(
                            "{session_id}: durable bytes exceed authenticated state reservation"
                        ));
                        continue;
                    }
                    let pending_terminal_bytes = session
                        .operations
                        .values()
                        .try_fold(0_u64, |total, operation| {
                            total.checked_add(operation.reserved_bytes)
                        })
                        .and_then(|operations| {
                            operations.checked_add(
                                session
                                    .recovery_attempt
                                    .as_ref()
                                    .map_or(0, |attempt| attempt.reserved_bytes),
                            )
                        });
                    let projected_control_safe_bytes = pending_terminal_bytes.and_then(|pending| {
                        session
                            .durable_bytes
                            .checked_add(pending)
                            .and_then(|value| value.checked_add(SESSION_CLOSE_HEADROOM_RESERVATION))
                            .and_then(|value| value.checked_add(ACTOR_FENCE_HEADROOM_RESERVATION))
                    });
                    if session.status != SessionStatusV2::Closed
                        && projected_control_safe_bytes.is_none_or(|projected| {
                            projected > session.state_reservation.state_bytes()
                        })
                    {
                        state.unreadable_sessions.push(format!(
                            "{session_id}: durable session no longer preserves terminal, actor-fence, and close headroom"
                        ));
                        continue;
                    }
                    if journal.corruption.is_some() {
                        session.status = SessionStatusV2::Quarantined;
                    }
                    for entry in &journal.entries {
                        if let Some(nonce) = entry.entry.event.placement_lease_nonce() {
                            state.used_lease_nonces.insert(nonce.to_owned());
                        }
                    }
                    let pending_operation_reservations = session
                        .operations
                        .values()
                        .try_fold(0_u64, |total, operation| {
                            total.checked_add(operation.reserved_bytes)
                        })
                        .context("reconstructed operation reservation accounting overflow")?;
                    state.reserved_durable_bytes = state
                        .reserved_durable_bytes
                        .checked_add(pending_operation_reservations)
                        .context("hosted operation reservation accounting overflow")?;
                    if let Some(attempt) = &session.recovery_attempt {
                        state.reserved_durable_bytes = state
                            .reserved_durable_bytes
                            .checked_add(attempt.reserved_bytes)
                            .context("hosted recovery reservation accounting overflow")?;
                    }
                    if session.status != SessionStatusV2::Closed {
                        state.state_bytes_reserved = state
                            .state_bytes_reserved
                            .checked_add(session.state_reservation.state_bytes())
                            .context("hosted state reservation accounting overflow")?;
                    }
                    state.sessions.insert(session_id, session);
                }
                Err(error) => state
                    .unreadable_sessions
                    .push(format!("{session_id}: {error:#}")),
            }
        }
        if reserved_state_capacity(&state)? > self.inner.config.state_quotas.max_state_bytes_total()
        {
            bail!("durable hosted sessions exceed configured state-byte capacity");
        }
        drop(state);
        self.classify_restart_interruptions()
    }

    fn classify_restart_interruptions(&self) -> Result<()> {
        let now = unix_time_ms()?;
        let mut state = self.lock_state()?;
        let ids = state.sessions.keys().cloned().collect::<Vec<_>>();
        for session_id in ids {
            let classified: Result<()> = (|| {
                let interrupted_recovery = state.sessions[&session_id].recovery_attempt.clone();
                if let Some(attempt) = interrupted_recovery {
                    let session = &state.sessions[&session_id];
                    let receipt = self.issue_next_entry(
                        session,
                        now,
                        JournalEventV2::RecoveryRefused {
                            client_sequence: attempt.client_sequence,
                            client_request_id: attempt.client_request_id.clone(),
                            request_sha256: attempt.request_sha256.clone(),
                            warrant_sha256: attempt.warrant_sha256.clone(),
                            placement_lease_sha256: attempt.placement_lease_sha256.clone(),
                            placement_lease_nonce: attempt.placement_lease_nonce.clone(),
                            recovery_attempt_sha256: Some(attempt.receipt_sha256.clone()),
                            attempted_actor_generation: Some(
                                attempt.attempted_actor_generation.clone(),
                            ),
                            code: "recovery-attempt-interrupted".to_owned(),
                            message: "node restarted after durably allocating a recovery actor generation but before publishing its terminal acknowledgement".to_owned(),
                        },
                    )?;
                    let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
                    if needed > attempt.reserved_bytes {
                        bail!(
                            "restart recovery refusal requires {needed} bytes, exceeding reserved terminal headroom {}",
                            attempt.reserved_bytes
                        );
                    }
                    let written = self.inner.store.append_entry(&session_id, &receipt)?;
                    if written != needed {
                        bail!("encoded restart recovery-refusal frame length changed");
                    }
                    state.durable_bytes = state
                        .durable_bytes
                        .checked_add(written)
                        .context("hosted durable-byte accounting overflow")?;
                    state.reserved_durable_bytes = state
                        .reserved_durable_bytes
                        .saturating_sub(attempt.reserved_bytes);
                    let session = state.sessions.get_mut(&session_id).unwrap();
                    session.durable_bytes = session
                        .durable_bytes
                        .checked_add(written)
                        .context("hosted session-byte accounting overflow")?;
                    session.recovery_attempt = None;
                    session.preparation = None;
                    session.actor_id = None;
                    session.actor_has_state = false;
                    session.status = SessionStatusV2::RecoveryRequired;
                    apply_receipt_head(session, &receipt);
                    record_commit(
                        session,
                        attempt.client_sequence,
                        attempt.client_request_id,
                        attempt.request_sha256,
                        receipt,
                    )?;
                }
                let pending = {
                    let session = &state.sessions[&session_id];
                    if session.status == SessionStatusV2::Quarantined
                        || session.status == SessionStatusV2::Closed
                    {
                        Vec::new()
                    } else {
                        session
                            .operations
                            .values()
                            .filter_map(|operation| match operation.view.status {
                                OperationStatusV2::Accepted => Some((
                                    operation.view.operation_id.clone(),
                                    operation.view.operation_sha256.clone(),
                                    OperationStatusV2::NotStarted,
                                    "node restarted after durable acceptance but before execution start",
                                )),
                                OperationStatusV2::Running => Some((
                                    operation.view.operation_id.clone(),
                                    operation.view.operation_sha256.clone(),
                                    OperationStatusV2::Ambiguous,
                                    "node restarted after execution start without a terminal record",
                                )),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    }
                };
                for (operation_id, digest, classification, reason) in pending {
                    let session = &state.sessions[&session_id];
                    let reserved = session
                        .operations
                        .get(&operation_id)
                        .context("restart interruption names an unknown operation")?
                        .reserved_bytes;
                    let receipt = self.issue_next_entry(
                        session,
                        now,
                        JournalEventV2::OperationInterrupted {
                            operation_id: operation_id.clone(),
                            operation_sha256: digest,
                            classification,
                            reason: bounded_durable_text(reason),
                        },
                    )?;
                    let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
                    if needed > reserved {
                        bail!(
                            "restart interruption requires {needed} bytes, exceeding reserved terminal headroom {reserved}"
                        );
                    }
                    let written = self.inner.store.append_entry(&session_id, &receipt)?;
                    if written != needed {
                        bail!("encoded restart-interruption frame length changed");
                    }
                    state.durable_bytes = state
                        .durable_bytes
                        .checked_add(written)
                        .context("hosted durable-byte accounting overflow")?;
                    state.reserved_durable_bytes = state
                        .reserved_durable_bytes
                        .checked_sub(reserved)
                        .context("restart interruption reservation accounting underflow")?;
                    let session = state.sessions.get_mut(&session_id).unwrap();
                    session.durable_bytes = session
                        .durable_bytes
                        .checked_add(written)
                        .context("hosted session-byte accounting overflow")?;
                    apply_receipt_head(session, &receipt);
                    let operation = session.operations.get_mut(&operation_id).unwrap();
                    operation.view.status = classification;
                    operation.reserved_bytes = 0;
                    if classification == OperationStatusV2::Ambiguous {
                        session.status = SessionStatusV2::RecoveryRequired;
                    } else {
                        session.status = SessionStatusV2::Ready;
                    }
                }
                let needs_checkpoint_restore = {
                    let session = &state.sessions[&session_id];
                    session.status == SessionStatusV2::Ready
                        && session.state_tier == SessionStateTierV2::CheckpointRestore
                        && session.actor_has_state
                };
                if needs_checkpoint_restore {
                    // Validate the immutable snapshot before publishing the
                    // physical-generation loss below. No ActorRestored evidence
                    // exists until an explicit ActorLost recovery receives a
                    // backend RestoreV1 acknowledgement.
                    checkpoint_for_session(&self.inner.store, &state.sessions[&session_id])?
                        .context("checkpoint session has state without a durable checkpoint")?;
                }
                let needs_empty_actor_retirement = {
                    let session = &state.sessions[&session_id];
                    session.status == SessionStatusV2::Ready
                        && session.state_tier.needs_live_actor()
                        && session.actor_generation.is_some()
                        && session.actor_id.is_some()
                        && !session.actor_has_state
                };
                if needs_empty_actor_retirement {
                    let session = &state.sessions[&session_id];
                    let previous_actor_generation = session
                        .actor_generation
                        .clone()
                        .context("empty stateful actor has no generation to retire")?;
                    let next_actor_generation = GenerationV1::new(
                        previous_actor_generation
                            .generation()
                            .get()
                            .checked_add(1)
                            .context("actor generation overflow during restart retirement")?,
                    )?;
                    let receipt = self.issue_next_entry(
                        session,
                        now,
                        JournalEventV2::ActorGenerationRetired {
                            previous_actor_generation,
                            next_actor_generation,
                            reason: "node restart retired a state-empty physical evaluator/actor"
                                .to_owned(),
                        },
                    )?;
                    let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
                    ensure_actor_fence_durable_capacity(&state, &session_id, needed)?;
                    let written = self.inner.store.append_entry(&session_id, &receipt)?;
                    state.durable_bytes += written;
                    let session = state.sessions.get_mut(&session_id).unwrap();
                    session.durable_bytes += written;
                    session.actor_id = None;
                    session.actor_generation = None;
                    session.next_actor_generation = next_actor_generation;
                    session.actor_has_state = false;
                    session.status = SessionStatusV2::Ready;
                    apply_receipt_head(session, &receipt);
                }
                let needs_actor_recovery = {
                    let session = &state.sessions[&session_id];
                    matches!(
                        session.status,
                        SessionStatusV2::Ready | SessionStatusV2::RecoveryRequired
                    ) && session.state_tier.needs_live_actor()
                        && session.actor_has_state
                        && session.actor_id.is_some()
                };
                if needs_actor_recovery {
                    let session = &state.sessions[&session_id];
                    let previous_actor_generation = session
                        .actor_generation
                        .clone()
                        .context("stateful session lost state without an actor generation")?;
                    let next_generation = GenerationV1::new(
                        previous_actor_generation
                            .generation()
                            .get()
                            .checked_add(1)
                            .context("actor generation overflow during restart recovery")?,
                    )?;
                    let receipt = self.issue_next_entry(
                        session,
                        now,
                        JournalEventV2::ActorStateLost {
                            previous_actor_generation: previous_actor_generation.clone(),
                            next_actor_generation: next_generation,
                            reason: "node restart lost the journaled physical evaluator/actor"
                                .to_owned(),
                        },
                    )?;
                    let needed = self.inner.store.encoded_frame_bytes(&receipt)?;
                    ensure_actor_fence_durable_capacity(&state, &session_id, needed)?;
                    let written = self.inner.store.append_entry(&session_id, &receipt)?;
                    state.durable_bytes += written;
                    let session = state.sessions.get_mut(&session_id).unwrap();
                    session.durable_bytes += written;
                    session.actor_id = None;
                    session.actor_generation = Some(previous_actor_generation);
                    session.next_actor_generation = next_generation;
                    session.actor_has_state = false;
                    session.status = SessionStatusV2::RecoveryRequired;
                    apply_receipt_head(session, &receipt);
                }
                Ok(())
            })();
            if let Err(error) = classified {
                isolate_session_recovery_failure(&mut state, &session_id, &error);
            }
        }
        state.reserved_durable_bytes = state
            .sessions
            .values()
            .filter_map(|session| session.recovery_attempt.as_ref())
            .try_fold(0_u64, |total, attempt| {
                total.checked_add(attempt.reserved_bytes)
            })
            .context("hosted recovery reservation accounting overflow")?;
        Ok(())
    }
}

fn isolate_session_recovery_failure(
    state: &mut RuntimeStateV2,
    session_id: &str,
    error: &anyhow::Error,
) {
    state.workers.remove(session_id);
    state.unreadable_sessions.push(format!(
        "{session_id}: session recovery isolated: {error:#}"
    ));
    let Some(session) = state.sessions.get_mut(session_id) else {
        return;
    };
    if matches!(
        session.status,
        SessionStatusV2::Closed | SessionStatusV2::Quarantined
    ) {
        return;
    }
    session.preparation = None;
    session.actor_id = None;
    if session.recovery_attempt.is_some() {
        session.actor_has_state = false;
        session.status = SessionStatusV2::Quarantined;
        return;
    }
    if session.state_tier != SessionStateTierV2::CheckpointRestore {
        session.actor_has_state = false;
    }
    if let Some(actor) = session.actor_generation.as_ref() {
        if let Some(next) = actor.generation().get().checked_add(1) {
            if let Ok(next) = GenerationV1::new(next) {
                session.next_actor_generation = next;
            }
        }
    }
    session.status = SessionStatusV2::RecoveryRequired;
}

fn quarantine_recovery_attempt_locked(
    state: &mut RuntimeStateV2,
    session_id: &str,
    error: &anyhow::Error,
) {
    state.workers.remove(session_id);
    state.unreadable_sessions.push(format!(
        "{session_id}: durable recovery attempt was isolated: {error:#}"
    ));
    if let Some(session) = state.sessions.get_mut(session_id) {
        session.preparation = None;
        session.actor_id = None;
        session.actor_has_state = false;
        session.status = SessionStatusV2::Quarantined;
    }
}

fn spawn_actor(
    runtime: &Arc<RuntimeInnerV2>,
    session_id: &str,
    tier: SessionStateTierV2,
) -> Result<Arc<ActorWorkerV2>> {
    let (sender, receiver) = mpsc::channel();
    let session_id = session_id.to_owned();
    let runtime_weak = Arc::downgrade(runtime);
    let worker_session_id = session_id.clone();
    let join = thread::Builder::new()
        .name(format!(
            "ostadix-v2-{}",
            &session_id[..session_id.len().min(24)]
        ))
        .spawn(move || actor_loop(runtime_weak, worker_session_id, tier, receiver))
        .context("failed to spawn hosted V2 session actor")?;
    let lifecycle = Arc::new(ActorWorkerLifecycleV2 {
        session_id,
        join: Mutex::new(Some(join)),
    });
    let worker = Arc::new(ActorWorkerV2 {
        sender,
        lifecycle: Arc::clone(&lifecycle),
    });
    runtime
        .worker_lifecycles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(lifecycle);
    Ok(worker)
}

fn actor_loop(
    runtime: Weak<RuntimeInnerV2>,
    session_id: String,
    tier: SessionStateTierV2,
    receiver: mpsc::Receiver<ActorCommandV2>,
) {
    let Some(initial) = runtime.upgrade() else {
        return;
    };
    let mut evaluator = new_evaluator(&initial);
    drop(initial);
    let mut scope = HashMap::<String, OValue>::new();
    while let Ok(command) = receiver.recv() {
        match command {
            ActorCommandV2::Prepare { operation, reply } => {
                let Some(inner) = runtime.upgrade() else {
                    let _ = reply.send(Err("hosted runtime stopped during preparation".to_owned()));
                    break;
                };
                if matches!(tier, SessionStateTierV2::Stateless) {
                    evaluator = new_evaluator(&inner);
                    scope.clear();
                }
                let result = evaluator
                    .prepare_placement_fragment(
                        &operation.source_utf8,
                        operation.task_attempt.clone(),
                    )
                    .map_err(|error| format!("{error:#}"));
                let _ = reply.send(result);
            }
            ActorCommandV2::Execute {
                operation,
                prepared,
                actor_generation,
            } => {
                let Some(inner) = runtime.upgrade() else {
                    break;
                };
                if let Err(error) =
                    operation_started(&inner, &session_id, &operation, actor_generation.as_ref())
                {
                    mark_quarantined(
                        &inner,
                        &session_id,
                        &format!("journal start failed: {error:#}"),
                    );
                    continue;
                }
                let disposition =
                    execute_operation(&mut evaluator, &mut scope, &operation, *prepared);
                let (outcome, state_durable, actor_state_touched) = match disposition {
                    ExecutionDispositionV2::Untouched(outcome) => (outcome, true, false),
                    ExecutionDispositionV2::InFlightInfrastructure(message)
                        if tier.needs_live_actor() =>
                    {
                        let Some(actor_generation) = actor_generation.as_ref() else {
                            mark_quarantined(
                                &inner,
                                &session_id,
                                "stateful infrastructure failure has no actor generation",
                            );
                            break;
                        };
                        if let Err(error) = record_ambiguous_actor_loss(
                            &inner,
                            &session_id,
                            &operation,
                            actor_generation,
                            &message,
                        ) {
                            mark_quarantined(
                                &inner,
                                &session_id,
                                &format!("ambiguous actor-loss journal failed: {error:#}"),
                            );
                        }
                        // Never reuse an evaluator whose persistent actor was
                        // retired or whose effects are unknown.
                        break;
                    }
                    ExecutionDispositionV2::InFlightInfrastructure(message) => (
                        OperationOutcomeV2::failed(
                            OperationFailureStageV2::Infrastructure,
                            "backend-infrastructure-failed",
                            message,
                        ),
                        true,
                        false,
                    ),
                    ExecutionDispositionV2::Settled(mut outcome) => {
                        let (state_durable, actor_state_touched) = if matches!(
                            tier,
                            SessionStateTierV2::CheckpointRestore
                        ) {
                            let Some(previous_generation) = actor_generation.as_ref() else {
                                mark_quarantined(
                                    &inner,
                                    &session_id,
                                    "checkpoint session executed without an actor generation",
                                );
                                continue;
                            };
                            match checkpoint_actor(
                                &inner,
                                &session_id,
                                &mut evaluator,
                                &operation,
                                previous_generation,
                            ) {
                                Ok(()) => (true, true),
                                Err(error) => {
                                    let message = format!("{error:#}");
                                    if let Err(journal_error) = record_checkpoint_failure(
                                        &inner,
                                        &session_id,
                                        &operation,
                                        previous_generation,
                                        &message,
                                    ) {
                                        mark_quarantined(
                                            &inner,
                                            &session_id,
                                            &format!(
                                                "checkpoint failure journal append failed: {journal_error:#}"
                                            ),
                                        );
                                        continue;
                                    }
                                    #[cfg(debug_assertions)]
                                    if let Err(barrier_error) =
                                        wait_checkpoint_failure_terminal_barrier_for_test(
                                            &inner,
                                            &session_id,
                                        )
                                    {
                                        mark_quarantined(
                                            &inner,
                                            &session_id,
                                            &format!(
                                                "checkpoint-failure test barrier failed: {barrier_error:#}"
                                            ),
                                        );
                                        continue;
                                    }
                                    outcome = OperationOutcomeV2::failed(
                                        OperationFailureStageV2::Infrastructure,
                                        "state-checkpoint-failed",
                                        message,
                                    );
                                    (false, true)
                                }
                            }
                        } else {
                            (true, true)
                        };
                        (outcome, state_durable, actor_state_touched)
                    }
                };
                if let Err(error) = operation_finished(
                    &inner,
                    &session_id,
                    &operation,
                    outcome,
                    state_durable,
                    actor_state_touched,
                ) {
                    mark_quarantined(
                        &inner,
                        &session_id,
                        &format!("terminal journal append failed: {error:#}"),
                    );
                }
            }
            ActorCommandV2::Recover {
                snapshot,
                snapshot_limit,
                probes,
                deadline,
                reply,
            } => {
                let result = acknowledge_checkpoint_restore(
                    &mut evaluator,
                    snapshot,
                    snapshot_limit,
                    &probes,
                    deadline,
                )
                .map_err(|error| format!("{error:#}"));
                let failed = result.is_err();
                if reply.send(result).is_err() || failed {
                    // The replacement evaluator is never reusable after an
                    // unacknowledged or unobserved restore attempt.
                    break;
                }
                scope.clear();
            }
            #[cfg(debug_assertions)]
            ActorCommandV2::PanicForTest => panic!("injected hosted V2 actor panic"),
            ActorCommandV2::Close => break,
        }
    }
}

fn new_evaluator(inner: &RuntimeInnerV2) -> Evaluator {
    Evaluator::new(inner.config.shim_dir.clone())
        .with_registered_backends(BackendRegistry::global().registered_backend_tags())
        .with_runtime_executable(inner.config.runtime_executable.clone())
}

fn checkpoint_for_session(
    store: &RuntimeStoreV2,
    session: &SessionRecordV2,
) -> Result<Option<EvaluatorStateSnapshotV1>> {
    if session.state_tier != SessionStateTierV2::CheckpointRestore {
        return Ok(None);
    }
    let Some(checkpoint) = &session.checkpoint else {
        if session.actor_has_state {
            bail!("checkpoint/restore session has live-state history but no durable checkpoint");
        }
        return Ok(None);
    };
    let current_actor = session
        .actor_generation
        .as_ref()
        .context("checkpoint session has no actor-generation identity")?;
    if current_actor != &checkpoint.actor_generation
        && (session.status != SessionStatusV2::RecoveryRequired
            || !same_actor_lineage(current_actor, &checkpoint.actor_generation)
            || checkpoint.actor_generation.generation().get() > current_actor.generation().get())
    {
        bail!("durable checkpoint belongs to a future or different actor lineage");
    }
    if checkpoint.snapshot_bytes > session.state_reservation.snapshot_bytes_per_actor() {
        bail!("durable evaluator checkpoint exceeds its authenticated reservation");
    }
    let snapshot = store.read_checkpoint(
        session.session_id.as_str(),
        &checkpoint.snapshot_sha256,
        checkpoint.snapshot_bytes,
    )?;
    if snapshot.snapshot_sha256()? != checkpoint.snapshot_sha256
        || snapshot.encoded_len()? as u64 != checkpoint.snapshot_bytes
    {
        bail!("durable evaluator checkpoint digest or reservation mismatch");
    }
    validate_checkpoint_state_contract(&snapshot, session)?;
    Ok(Some(snapshot))
}

/// Bind an authority-free evaluator snapshot back to the exact hosted session
/// and the current catalog contract before either publishing or consuming a
/// durability claim. Backend checkpoint codec strings are deliberately hashed
/// through the same V2 domain used by the generated catalog.
fn validate_checkpoint_state_contract(
    snapshot: &EvaluatorStateSnapshotV1,
    session: &SessionRecordV2,
) -> Result<()> {
    snapshot.validate()?;
    if session.state_tier != SessionStateTierV2::CheckpointRestore {
        bail!("semantic evaluator checkpoint belongs to a non-checkpoint session");
    }
    let actor_generation = session
        .actor_generation
        .as_ref()
        .context("checkpoint session has no actor-generation identity")?;
    if actor_generation.backend_implementation()
        != &session.placement_identity.backend_implementation
        || actor_generation.target_descriptor() != &session.placement_identity.target_descriptor
    {
        bail!("checkpoint actor generation is not bound to the session implementation");
    }
    let actor_limit: usize = session
        .state_reservation
        .actor_count()
        .try_into()
        .context("checkpoint actor reservation exceeds host address space")?;
    if snapshot.actors.len() > actor_limit {
        bail!(
            "evaluator checkpoint contains {} actors, above the authenticated reservation of {actor_limit}",
            snapshot.actors.len()
        );
    }
    if session.actor_has_state && snapshot.actors.is_empty() {
        bail!("stateful checkpoint contains no evaluator actor state");
    }

    let mut exact_backend = None::<&str>;
    for actor in &snapshot.actors {
        if exact_backend.is_some_and(|backend| backend != actor.canonical_backend) {
            bail!("evaluator checkpoint crosses canonical backend identities");
        }
        exact_backend = Some(actor.canonical_backend.as_str());
        let specification = BackendRegistry::global()
            .get(&actor.canonical_backend)
            .with_context(|| {
                format!(
                    "checkpoint backend `{}` is absent from the current catalog",
                    actor.canonical_backend
                )
            })?;
        if specification.name != actor.canonical_backend
            || actor.checkpoint.backend != actor.canonical_backend
        {
            bail!(
                "checkpoint backend `{}` is not an exact canonical catalog identity",
                actor.canonical_backend
            );
        }
        let support = BackendRegistry::global()
            .state_support_for(&actor.canonical_backend)
            .context("checkpoint backend has no current state-support contract")?;
        let (codec, compatibility) = match support {
            BackendStateSupportV2::SemanticSnapshot {
                codec,
                compatibility,
            } => (codec, compatibility),
            BackendStateSupportV2::Stateless => {
                bail!("checkpoint backend is currently catalogued as stateless")
            }
            BackendStateSupportV2::ExternalPinned { .. } => {
                bail!("checkpoint backend currently requires external pinned state")
            }
        };
        if actor.checkpoint.tier != BackendStateTierV1::SemanticSnapshot {
            bail!("checkpoint backend did not emit SemanticSnapshot state");
        }
        let actual_codec = crate::placement::SemanticDigestV1::hash_bytes(
            BACKEND_STATE_CODEC_NAME_DOMAIN_V2,
            actor.checkpoint.codec.as_bytes(),
        );
        if codec != &actual_codec {
            bail!(
                "checkpoint codec `{}` does not match the current catalog contract",
                actor.checkpoint.codec
            );
        }
        if actor.sandbox_policy_sha256 != actor_generation.sandbox_policy().as_sha256() {
            bail!("checkpoint sandbox is not bound to the hosted actor generation");
        }
        match compatibility {
            SnapshotCompatibilityV2::ExactImplementation => {
                if actor.launch_generation_sha256 != actor_generation.launch_context().as_sha256() {
                    bail!(
                        "checkpoint launch generation is not bound to the exact session implementation"
                    );
                }
            }
            SnapshotCompatibilityV2::CompatibilityClass(_) => {
                bail!("hosted V2 checkpoints do not yet persist a compatibility-class identity")
            }
        }
    }
    Ok(())
}

/// Build the narrow, state-neutral fragments used only to force the current
/// SemanticSnapshot backends across their RestoreV1 receipt boundary. This is
/// intentionally an explicit map: adding a new snapshot codec does not make
/// hosted recovery silently executable until its no-op semantics are reviewed.
fn recovery_probes(
    session_id: &str,
    client_request_id: &str,
    snapshot: &EvaluatorStateSnapshotV1,
    actor_generation: &ActorGenerationIdV1,
) -> Result<Vec<RecoveryProbeV2>> {
    if snapshot.actors.is_empty() {
        bail!("checkpoint contains no persistent actors to acknowledge");
    }
    snapshot
        .actors
        .iter()
        .enumerate()
        .map(|(index, actor)| {
            let body = match (
                actor.canonical_backend.as_str(),
                actor.checkpoint.codec.as_str(),
            ) {
                ("python", "ostadix.python-graph/v1") => "pass",
                ("sql", "ostadix.sqlite-cli-main/v1") => "SELECT NULL",
                (backend, codec) => {
                    bail!(
                        "SemanticSnapshot backend `{backend}` codec `{codec}` has no reviewed recovery probe"
                    )
                }
            };
            let environment = EnvironmentRefV2::persistent(actor.environment_id)
                .context("checkpoint recovery actor environment is not persistent")?;
            let source_utf8 = format!(
                "{}[{}]^({})_{}[{}]",
                actor.canonical_backend,
                actor.environment_id,
                body,
                actor.canonical_backend,
                actor.environment_id
            );
            let task_material = format!(
                "{session_id}\0{client_request_id}\0{index}\0{}\0{}",
                actor.canonical_backend, actor.environment_id
            );
            let task_attempt = TaskAttemptIdV1::new(
                SemanticDigestV1::hash_bytes(
                    "ostadix/hosted/recovery-probe-task/v2",
                    task_material.as_bytes(),
                ),
                GenerationV1::new(1)?,
            );
            Ok(RecoveryProbeV2 {
                source_utf8,
                task_attempt,
                canonical_backend: actor.canonical_backend.clone(),
                environment,
                backend_implementation: actor_generation.backend_implementation().clone(),
                sandbox_policy: SemanticDigestV1::from_sha256(
                    actor.sandbox_policy_sha256.clone(),
                )?,
                launch_generation: SemanticDigestV1::from_sha256(
                    actor.launch_generation_sha256.clone(),
                )?,
            })
        })
        .collect()
}

fn acknowledge_checkpoint_restore(
    evaluator: &mut Evaluator,
    snapshot: EvaluatorStateSnapshotV1,
    snapshot_limit: u64,
    probes: &[RecoveryProbeV2],
    deadline: Instant,
) -> Result<()> {
    evaluator
        .stage_persistent_actor_restore(snapshot, snapshot_limit)
        .context("checkpoint cannot be staged by the replacement evaluator")?;
    for probe in probes {
        if Instant::now() >= deadline {
            bail!("backend restore acknowledgement deadline expired before probe preparation");
        }
        let prepared = evaluator
            .prepare_placement_fragment(&probe.source_utf8, probe.task_attempt.clone())
            .context("failed to prepare state-neutral recovery probe")?;
        let bindings = prepared.bindings();
        if bindings.canonical_backend() != probe.canonical_backend
            || bindings.environment() != probe.environment
            || bindings.backend_implementation_sha256() != &probe.backend_implementation
            || bindings.sandbox_policy_sha256() != &probe.sandbox_policy
            || bindings.backend_launch_generation() != &probe.launch_generation
        {
            bail!(
                "recovery probe backend/environment/sandbox/implementation/launch binding differs from checkpoint"
            );
        }
        let mut scope = HashMap::new();
        evaluator
            .execute_prepared_placement_fragment_until(prepared, &mut scope, deadline)
            .context("state-neutral recovery probe did not receive a backend restore receipt")?;
    }
    let pending = evaluator.pending_persistent_actor_restores();
    if pending != 0 {
        bail!("{pending} checkpoint actor restore(s) did not receive backend acknowledgement");
    }
    Ok(())
}

fn execute_operation(
    evaluator: &mut Evaluator,
    scope: &mut HashMap<String, OValue>,
    operation: &PreparedOperationV2,
    prepared: PreparedPlacementFragmentV1,
) -> ExecutionDispositionV2 {
    let now = match unix_time_ms() {
        Ok(now) => now,
        Err(error) => {
            return ExecutionDispositionV2::Untouched(OperationOutcomeV2::failed(
                OperationFailureStageV2::Infrastructure,
                "clock-unavailable",
                format!("{error:#}"),
            ))
        }
    };
    if now >= operation.deadline_unix_ms {
        return ExecutionDispositionV2::Untouched(OperationOutcomeV2::failed(
            OperationFailureStageV2::Deadline,
            "deadline-expired",
            "operation deadline expired before evaluator entry",
        ));
    }
    if operation.expected_backend_catalog_sha256 != BackendRegistry::global().catalog_sha256() {
        return ExecutionDispositionV2::Untouched(OperationOutcomeV2::failed(
            OperationFailureStageV2::Admission,
            "backend-catalog-mismatch",
            "backend catalog changed after durable admission",
        ));
    }
    // Hosted V2 does not carry or authorize a coordinator-scope package.
    // Persistent continuity lives only in the exact backend actor, so never
    // let residue from an earlier graph frame become ambient command input.
    scope.clear();
    let deadline_wall_now = match unix_time_ms() {
        Ok(now) => now,
        Err(error) => {
            return ExecutionDispositionV2::Untouched(OperationOutcomeV2::failed(
                OperationFailureStageV2::Infrastructure,
                "clock-unavailable",
                format!("{error:#}"),
            ))
        }
    };
    if deadline_wall_now >= operation.deadline_unix_ms {
        return ExecutionDispositionV2::Untouched(OperationOutcomeV2::failed(
            OperationFailureStageV2::Deadline,
            "deadline-expired",
            "operation deadline expired before prepared evaluator dispatch",
        ));
    }
    let deadline =
        match Instant::now().checked_add(Duration::from_millis(
            operation.deadline_unix_ms - deadline_wall_now,
        )) {
            Some(deadline) => deadline,
            None => return ExecutionDispositionV2::Untouched(OperationOutcomeV2::failed(
                OperationFailureStageV2::Infrastructure,
                "deadline-conversion-failed",
                "absolute hosted deadline could not be represented by the local monotonic clock",
            )),
        };
    let value = match evaluator.execute_prepared_placement_fragment_until(prepared, scope, deadline)
    {
        Ok(value) => value,
        Err(error)
            if error
                .downcast_ref::<PreparedPlacementDeadlineExpiredV1>()
                .is_some() =>
        {
            return ExecutionDispositionV2::Untouched(OperationOutcomeV2::failed(
                OperationFailureStageV2::Deadline,
                "deadline-expired",
                format!("{error:#}"),
            ))
        }
        Err(error) if error.downcast_ref::<PreparedPlacementRefusalV1>().is_some() => {
            return ExecutionDispositionV2::Settled(OperationOutcomeV2::failed(
                OperationFailureStageV2::Admission,
                "prepared-authority-refused",
                format!("{error:#}"),
            ))
        }
        Err(error) if crate::process::is_infrastructure_error(&error) => {
            return ExecutionDispositionV2::InFlightInfrastructure(format!("{error:#}"))
        }
        Err(error) => {
            return ExecutionDispositionV2::Settled(OperationOutcomeV2::failed(
                OperationFailureStageV2::Evaluate,
                "evaluation-failed",
                format!("{error:#}"),
            ))
        }
    };
    let finished = match unix_time_ms() {
        Ok(finished) => finished,
        Err(error) => {
            return ExecutionDispositionV2::Settled(OperationOutcomeV2::failed(
                OperationFailureStageV2::Infrastructure,
                "clock-unavailable",
                format!("{error:#}"),
            ))
        }
    };
    if finished > operation.deadline_unix_ms {
        return ExecutionDispositionV2::Settled(OperationOutcomeV2::failed(
            OperationFailureStageV2::Deadline,
            "deadline-exceeded",
            "evaluation completed after its absolute deadline; value suppressed",
        ));
    }
    match canonical_hosted_bytes(&value) {
        Err(error) => ExecutionDispositionV2::Settled(OperationOutcomeV2::failed(
            OperationFailureStageV2::Output,
            "result-encoding-failed",
            format!("{error:#}"),
        )),
        Ok(bytes) if bytes.len() > operation.output_limit_bytes as usize => {
            ExecutionDispositionV2::Settled(OperationOutcomeV2::failed(
                OperationFailureStageV2::Output,
                "result-too-large",
                format!(
                    "serialized result length {} exceeds prepared output limit {}",
                    bytes.len(),
                    operation.output_limit_bytes
                ),
            ))
        }
        Ok(_) => ExecutionDispositionV2::Settled(OperationOutcomeV2::Succeeded { value }),
    }
}

fn operation_started(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    operation: &PreparedOperationV2,
    actor_generation: Option<&ActorGenerationIdV1>,
) -> Result<()> {
    let now = unix_time_ms()?;
    let mut state = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
    let session = state
        .sessions
        .get(session_id)
        .context("session disappeared before operation start")?;
    let operation_record = session
        .operations
        .get(&operation.operation_id)
        .context("operation disappeared before start")?;
    if operation_record.view.status != OperationStatusV2::Accepted {
        bail!("operation is no longer accepted");
    }
    let receipt = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: session.journal_sequence + 1,
        previous_entry_sha256: Some(session.journal_head_sha256.clone()),
        recorded_unix_ms: now,
        event: JournalEventV2::OperationStarted {
            operation_id: operation.operation_id.clone(),
            operation_sha256: operation_record.view.operation_sha256.clone(),
            actor_generation: actor_generation.cloned(),
            started_unix_ms: now,
        },
    })?;
    let needed = inner.store.encoded_frame_bytes(&receipt)?;
    if needed > operation_record.reserved_bytes {
        bail!(
            "operation start record requires {needed} bytes, exceeding reserved terminal headroom {}",
            operation_record.reserved_bytes
        );
    }
    let written = inner.store.append_entry(session_id, &receipt)?;
    if written != needed {
        bail!("encoded operation-start frame length changed");
    }
    state.durable_bytes = state
        .durable_bytes
        .checked_add(written)
        .context("hosted durable-byte accounting overflow")?;
    state.reserved_durable_bytes = state
        .reserved_durable_bytes
        .checked_sub(written)
        .context("operation-start reservation accounting underflow")?;
    let session = state.sessions.get_mut(session_id).unwrap();
    session.durable_bytes = session
        .durable_bytes
        .checked_add(written)
        .context("hosted session-byte accounting overflow")?;
    apply_receipt_head(session, &receipt);
    let operation_record = session.operations.get_mut(&operation.operation_id).unwrap();
    operation_record.view.status = OperationStatusV2::Running;
    operation_record.view.started_unix_ms = Some(now);
    operation_record.reserved_bytes = operation_record
        .reserved_bytes
        .checked_sub(written)
        .context("operation-start terminal reservation underflow")?;
    Ok(())
}

fn checkpoint_actor(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    evaluator: &mut Evaluator,
    operation: &PreparedOperationV2,
    actor_generation: &ActorGenerationIdV1,
) -> Result<()> {
    let snapshot_limit = {
        let mut state = inner
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
        #[cfg(debug_assertions)]
        if state.force_checkpoint_failure_for_test.remove(session_id) {
            bail!("test-injected persistent evaluator checkpoint failure");
        }
        let session = state
            .sessions
            .get(session_id)
            .context("session disappeared before checkpoint")?;
        session.state_reservation.snapshot_bytes_per_actor()
    };
    let snapshot = evaluator
        .checkpoint_persistent_actors(snapshot_limit)
        .context("persistent evaluator checkpoint failed")?;
    persist_actor_checkpoint(inner, session_id, operation, actor_generation, snapshot)
}

#[cfg(debug_assertions)]
fn wait_checkpoint_failure_terminal_barrier_for_test(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
) -> Result<()> {
    let barriers = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?
        .checkpoint_failure_terminal_barrier_for_test
        .remove(session_id);
    if let Some((entered, release)) = barriers {
        entered.wait();
        release.wait();
    }
    Ok(())
}

fn persist_actor_checkpoint(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    operation: &PreparedOperationV2,
    actor_generation: &ActorGenerationIdV1,
    snapshot: EvaluatorStateSnapshotV1,
) -> Result<()> {
    snapshot.validate()?;
    let snapshot_sha256 = snapshot.snapshot_sha256()?;
    let snapshot_bytes: u64 = snapshot
        .encoded_len()?
        .try_into()
        .context("evaluator checkpoint length exceeds u64")?;
    let now = unix_time_ms()?;
    let mut state = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
    let session = state
        .sessions
        .get(session_id)
        .context("session disappeared before checkpoint persistence")?;
    if session.actor_generation.as_ref() != Some(actor_generation) {
        bail!("actor generation changed before checkpoint persistence");
    }
    validate_checkpoint_state_contract(&snapshot, session)?;
    let receipt = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: session.journal_sequence + 1,
        previous_entry_sha256: Some(session.journal_head_sha256.clone()),
        recorded_unix_ms: now,
        event: JournalEventV2::ActorCheckpointed {
            actor_generation: actor_generation.clone(),
            snapshot_sha256: snapshot_sha256.clone(),
            snapshot_bytes,
        },
    })?;
    let checkpoint_frame = inner.store.checkpoint_new_bytes(
        session_id,
        &snapshot_sha256,
        &snapshot,
        session.state_reservation.snapshot_bytes_per_actor(),
    )?;
    let journal_frame = inner.store.encoded_frame_bytes(&receipt)?;
    let needed = checkpoint_frame
        .checked_add(journal_frame)
        .context("checkpoint durable-byte accounting overflow")?;
    ensure_session_durable_capacity(&state, session_id, needed)?;
    let next_durable_after_checkpoint = state
        .durable_bytes
        .checked_add(checkpoint_frame)
        .context("hosted durable-byte accounting overflow")?;
    let next_session_after_checkpoint = session
        .durable_bytes
        .checked_add(checkpoint_frame)
        .context("hosted session-byte accounting overflow")?;
    let checkpoint_written = inner.store.write_checkpoint(
        session_id,
        &snapshot_sha256,
        &snapshot,
        session.state_reservation.snapshot_bytes_per_actor(),
    )?;
    if checkpoint_written != checkpoint_frame {
        bail!(
            "checkpoint durable-byte preflight changed before installation: expected {checkpoint_frame}, wrote {checkpoint_written}"
        );
    }
    // As with operation blobs, the content-addressed checkpoint survives an
    // independent journal failure. Account it before appending the reference
    // so a later zero-delta retry cannot undercharge durable state.
    state.durable_bytes = next_durable_after_checkpoint;
    state.sessions.get_mut(session_id).unwrap().durable_bytes = next_session_after_checkpoint;
    let journal_written = inner.store.append_entry(session_id, &receipt)?;
    state.durable_bytes = state
        .durable_bytes
        .checked_add(journal_written)
        .context("hosted durable-byte accounting overflow")?;
    let session = state.sessions.get_mut(session_id).unwrap();
    session.durable_bytes = session
        .durable_bytes
        .checked_add(journal_written)
        .context("hosted session-byte accounting overflow")?;
    session.checkpoint = Some(DurableCheckpointV2 {
        actor_generation: actor_generation.clone(),
        snapshot_sha256,
        snapshot_bytes,
    });
    apply_receipt_head(session, &receipt);
    if session.operations[&operation.operation_id].view.status != OperationStatusV2::Running {
        bail!("operation stopped running before checkpoint persistence");
    }
    Ok(())
}

fn record_checkpoint_failure(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    operation: &PreparedOperationV2,
    actor_generation: &ActorGenerationIdV1,
    message: &str,
) -> Result<()> {
    let now = unix_time_ms()?;
    let mut state = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
    let session = state
        .sessions
        .get(session_id)
        .context("session disappeared before checkpoint failure record")?;
    if session.actor_generation.as_ref() != Some(actor_generation) {
        bail!("actor generation changed before checkpoint failure record");
    }
    let reserved = session
        .operations
        .get(&operation.operation_id)
        .context("checkpoint failure operation disappeared")?
        .reserved_bytes;
    let receipt = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: session.journal_sequence + 1,
        previous_entry_sha256: Some(session.journal_head_sha256.clone()),
        recorded_unix_ms: now,
        event: JournalEventV2::ActorCheckpointFailed {
            actor_generation: actor_generation.clone(),
            code: "state-checkpoint-failed".to_owned(),
            message: bounded_durable_text(message),
        },
    })?;
    let needed = inner.store.encoded_frame_bytes(&receipt)?;
    if needed > reserved {
        bail!(
            "checkpoint failure record requires {needed} bytes, exceeding reserved terminal headroom {reserved}"
        );
    }
    let next_durable_bytes = state
        .durable_bytes
        .checked_add(needed)
        .context("hosted durable-byte accounting overflow")?;
    let next_reserved_durable_bytes = state
        .reserved_durable_bytes
        .checked_sub(needed)
        .context("checkpoint-failure reservation accounting underflow")?;
    let next_session_bytes = session
        .durable_bytes
        .checked_add(needed)
        .context("hosted session-byte accounting overflow")?;
    let next_operation_reservation = reserved
        .checked_sub(needed)
        .context("checkpoint-failure operation reservation underflow")?;
    let written = inner.store.append_entry(session_id, &receipt)?;
    debug_assert_eq!(written, needed, "encoded journal frame length changed");
    state.durable_bytes = next_durable_bytes;
    state.reserved_durable_bytes = next_reserved_durable_bytes;
    let session = state.sessions.get_mut(session_id).unwrap();
    session.durable_bytes = next_session_bytes;
    // ActorCheckpointFailed is an intermediate record: the operation remains
    // Running until its signed terminal disposition publishes whether the
    // actor state is durable. Keeping Executing here prevents Reset/Close from
    // interleaving between the two records.
    session.status = SessionStatusV2::Executing;
    session.actor_has_state = true;
    apply_receipt_head(session, &receipt);
    session
        .operations
        .get_mut(&operation.operation_id)
        .unwrap()
        .reserved_bytes = next_operation_reservation;
    Ok(())
}

fn record_ambiguous_actor_loss(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    operation: &PreparedOperationV2,
    actor_generation: &ActorGenerationIdV1,
    reason: &str,
) -> Result<()> {
    let now = unix_time_ms()?;
    let mut state = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
    let session = state
        .sessions
        .get(session_id)
        .context("session disappeared before ambiguous actor-loss record")?;
    if session.actor_generation.as_ref() != Some(actor_generation) {
        bail!("actor generation changed before ambiguous actor-loss record");
    }
    let operation_record = session
        .operations
        .get(&operation.operation_id)
        .context("operation disappeared before ambiguous actor-loss record")?;
    if operation_record.view.status != OperationStatusV2::Running {
        bail!("ambiguous actor-loss operation is no longer running");
    }
    let operation_sha256 = operation_record.view.operation_sha256.clone();
    let reserved = operation_record.reserved_bytes;
    let next_actor_generation = GenerationV1::new(
        actor_generation
            .generation()
            .get()
            .checked_add(1)
            .context("actor generation overflow after infrastructure failure")?,
    )?;
    let lost = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: session.journal_sequence + 1,
        previous_entry_sha256: Some(session.journal_head_sha256.clone()),
        recorded_unix_ms: now,
        event: JournalEventV2::ActorStateLost {
            previous_actor_generation: actor_generation.clone(),
            next_actor_generation,
            reason: bounded_durable_text(reason),
        },
    })?;
    let interrupted = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: lost.entry.sequence + 1,
        previous_entry_sha256: Some(lost.entry_sha256.clone()),
        recorded_unix_ms: now,
        event: JournalEventV2::OperationInterrupted {
            operation_id: operation.operation_id.clone(),
            operation_sha256,
            classification: OperationStatusV2::Ambiguous,
            reason: bounded_durable_text(reason),
        },
    })?;
    let lost_bytes = inner.store.encoded_frame_bytes(&lost)?;
    let interrupted_bytes = inner.store.encoded_frame_bytes(&interrupted)?;
    let needed = lost_bytes
        .checked_add(interrupted_bytes)
        .context("ambiguous actor-loss journal capacity overflow")?;
    if needed > reserved {
        bail!(
            "ambiguous actor-loss records require {needed} bytes, exceeding reserved terminal headroom {reserved}"
        );
    }
    let lost_written = inner.store.append_entry(session_id, &lost)?;
    // ActorStateLost is independently durable. Reflect its exact head, bytes,
    // generation fence, and consumed operation escrow before attempting the
    // following OperationInterrupted append. A zero-byte failure of that
    // second append must never leave the runtime claiming the prior head.
    state.durable_bytes = state
        .durable_bytes
        .checked_add(lost_written)
        .context("hosted durable-byte accounting overflow")?;
    state.reserved_durable_bytes = state
        .reserved_durable_bytes
        .checked_sub(lost_written)
        .context("actor-state-loss reservation accounting underflow")?;
    state.workers.remove(session_id);
    {
        let session = state.sessions.get_mut(session_id).unwrap();
        session.durable_bytes = session
            .durable_bytes
            .checked_add(lost_written)
            .context("hosted session-byte accounting overflow")?;
        session.actor_id = None;
        // Keep the lost generation as the recovery origin and preserve the
        // last good checkpoint. Liveness is represented by actor_id and
        // actor_has_state, while next_actor_generation fences replacement.
        session.actor_generation = Some(actor_generation.clone());
        session.next_actor_generation = next_actor_generation;
        session.actor_has_state = false;
        session.status = SessionStatusV2::RecoveryRequired;
        apply_receipt_head(session, &lost);
        session
            .operations
            .get_mut(&operation.operation_id)
            .unwrap()
            .reserved_bytes = reserved
            .checked_sub(lost_written)
            .context("actor-state-loss operation reservation underflow")?;
    }

    let interrupted_written = inner
        .store
        .append_entry(session_id, &interrupted)
        .context("actor state was durably lost but operation interruption append failed")?;
    let residual_reservation =
        state.sessions[session_id].operations[&operation.operation_id].reserved_bytes;
    state.durable_bytes = state
        .durable_bytes
        .checked_add(interrupted_written)
        .context("hosted durable-byte accounting overflow")?;
    state.reserved_durable_bytes = state
        .reserved_durable_bytes
        .checked_sub(residual_reservation)
        .context("ambiguous interruption reservation accounting underflow")?;
    let session = state.sessions.get_mut(session_id).unwrap();
    session.durable_bytes = session
        .durable_bytes
        .checked_add(interrupted_written)
        .context("hosted session-byte accounting overflow")?;
    apply_receipt_head(session, &interrupted);
    let operation_record = session.operations.get_mut(&operation.operation_id).unwrap();
    operation_record.view.status = OperationStatusV2::Ambiguous;
    operation_record.view.finished_unix_ms = None;
    operation_record.view.outcome = None;
    operation_record.reserved_bytes = 0;
    Ok(())
}

fn operation_finished(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    operation: &PreparedOperationV2,
    outcome: OperationOutcomeV2,
    state_durable: bool,
    actor_state_touched: bool,
) -> Result<()> {
    let now = unix_time_ms()?;
    let mut state = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
    let session = state
        .sessions
        .get(session_id)
        .context("session disappeared before operation completion")?;
    let operation_record = session
        .operations
        .get(&operation.operation_id)
        .context("operation disappeared before completion")?;
    if session.status != SessionStatusV2::Executing
        || operation_record.view.status != OperationStatusV2::Running
    {
        bail!("operation completion is no longer the active executing transition");
    }
    let digest = operation_record.view.operation_sha256.clone();
    let reserved = operation_record.reserved_bytes;
    let receipt = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: session.journal_sequence + 1,
        previous_entry_sha256: Some(session.journal_head_sha256.clone()),
        recorded_unix_ms: now,
        event: JournalEventV2::OperationTerminal {
            operation_id: operation.operation_id.clone(),
            operation_sha256: digest,
            finished_unix_ms: now,
            outcome: outcome.clone(),
            state_durable,
            actor_state_touched,
        },
    })?;
    let terminal_bytes = inner.store.encoded_frame_bytes(&receipt)?;
    if terminal_bytes > reserved {
        bail!(
            "operation terminal record requires {terminal_bytes} bytes, exceeding reserved headroom {reserved}"
        );
    }
    let next_durable_bytes = state
        .durable_bytes
        .checked_add(terminal_bytes)
        .context("hosted durable-byte accounting overflow")?;
    let next_reserved_durable_bytes = state
        .reserved_durable_bytes
        .checked_sub(reserved)
        .context("operation-terminal reservation accounting underflow")?;
    let next_session_bytes = session
        .durable_bytes
        .checked_add(terminal_bytes)
        .context("hosted session-byte accounting overflow")?;
    let written = inner.store.append_entry(session_id, &receipt)?;
    debug_assert_eq!(
        written, terminal_bytes,
        "encoded journal frame length changed"
    );
    state.durable_bytes = next_durable_bytes;
    state.reserved_durable_bytes = next_reserved_durable_bytes;
    let session = state.sessions.get_mut(session_id).unwrap();
    session.durable_bytes = next_session_bytes;
    apply_receipt_head(session, &receipt);
    session.status = if state_durable {
        SessionStatusV2::Ready
    } else {
        SessionStatusV2::RecoveryRequired
    };
    if actor_state_touched && session.state_tier.needs_live_actor() {
        session.actor_has_state = true;
    }
    let operation_record = session.operations.get_mut(&operation.operation_id).unwrap();
    operation_record.view.status = match outcome {
        OperationOutcomeV2::Succeeded { .. } => OperationStatusV2::Succeeded,
        OperationOutcomeV2::Failed { .. } => OperationStatusV2::Failed,
    };
    operation_record.view.finished_unix_ms = Some(now);
    operation_record.view.outcome = Some(outcome);
    operation_record.reserved_bytes = 0;
    Ok(())
}

fn fence_missing_worker_locked(
    inner: &Arc<RuntimeInnerV2>,
    state: &mut RuntimeStateV2,
    session_id: &str,
    reason: &str,
) -> Result<()> {
    let now = unix_time_ms()?;
    let session = state
        .sessions
        .get(session_id)
        .context("session disappeared while fencing its missing worker")?;
    let previous_actor_generation = session
        .actor_generation
        .clone()
        .context("stateful missing worker has no established actor generation")?;
    if session.actor_has_state && session.state_tier == SessionStateTierV2::CheckpointRestore {
        checkpoint_for_session(&inner.store, session)?
            .context("checkpoint session lost its worker without a durable checkpoint")?;
    }
    let next_actor_generation =
        successor_actor_generation(&previous_actor_generation)?.generation();
    let event = if session.actor_has_state {
        JournalEventV2::ActorStateLost {
            previous_actor_generation: previous_actor_generation.clone(),
            next_actor_generation,
            reason: bounded_durable_text(reason),
        }
    } else {
        JournalEventV2::ActorGenerationRetired {
            previous_actor_generation: previous_actor_generation.clone(),
            next_actor_generation,
            reason: bounded_durable_text(reason),
        }
    };
    let receipt = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: session
            .journal_sequence
            .checked_add(1)
            .context("hosted journal sequence overflow")?,
        previous_entry_sha256: Some(session.journal_head_sha256.clone()),
        recorded_unix_ms: now,
        event,
    })?;
    let needed = inner.store.encoded_frame_bytes(&receipt)?;
    ensure_actor_fence_durable_capacity(state, session_id, needed)?;
    let written = inner.store.append_entry(session_id, &receipt)?;
    if written != needed {
        bail!("encoded missing-worker fence frame length changed");
    }
    state.durable_bytes = state
        .durable_bytes
        .checked_add(written)
        .context("hosted durable-byte accounting overflow")?;
    state.workers.remove(session_id);
    let session = state.sessions.get_mut(session_id).unwrap();
    session.durable_bytes = session
        .durable_bytes
        .checked_add(written)
        .context("hosted session-byte accounting overflow")?;
    session.actor_id = None;
    session.actor_has_state = false;
    session.next_actor_generation = next_actor_generation;
    if matches!(
        receipt.entry.event,
        JournalEventV2::ActorGenerationRetired { .. }
    ) {
        session.actor_generation = None;
        session.status = SessionStatusV2::Ready;
    } else {
        session.actor_generation = Some(previous_actor_generation);
        session.status = SessionStatusV2::RecoveryRequired;
    }
    apply_receipt_head(session, &receipt);
    Ok(())
}

fn interrupt_before_start(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    reason: &str,
) -> Result<()> {
    let now = unix_time_ms()?;
    let mut state = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
    let session = state
        .sessions
        .get(session_id)
        .context("session disappeared")?;
    let Some(operation) = session
        .operations
        .values()
        .find(|operation| operation.view.status == OperationStatusV2::Accepted)
    else {
        return Ok(());
    };
    let operation_id = operation.view.operation_id.clone();
    let digest = operation.view.operation_sha256.clone();
    let reserved = operation.reserved_bytes;
    let interrupted = inner.store.issue_journal_entry(JournalEntryV2 {
        schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
        session_id: session_id.to_owned(),
        sequence: session.journal_sequence + 1,
        previous_entry_sha256: Some(session.journal_head_sha256.clone()),
        recorded_unix_ms: now,
        event: JournalEventV2::OperationInterrupted {
            operation_id: operation_id.clone(),
            operation_sha256: digest,
            classification: OperationStatusV2::NotStarted,
            reason: bounded_durable_text(reason),
        },
    })?;
    let actor_fence = if session.state_tier.needs_live_actor() {
        let previous = session
            .actor_generation
            .as_ref()
            .context("stateful accepted operation has no actor generation")?;
        let next_actor_generation = GenerationV1::new(
            previous
                .generation()
                .get()
                .checked_add(1)
                .context("actor generation overflow before execution start")?,
        )?;
        let event = if session.actor_has_state {
            JournalEventV2::ActorStateLost {
                previous_actor_generation: previous.clone(),
                next_actor_generation,
                reason: bounded_durable_text(reason),
            }
        } else {
            JournalEventV2::ActorGenerationRetired {
                previous_actor_generation: previous.clone(),
                next_actor_generation,
                reason: bounded_durable_text(reason),
            }
        };
        Some(inner.store.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: session_id.to_owned(),
            sequence: interrupted.entry.sequence + 1,
            previous_entry_sha256: Some(interrupted.entry_sha256.clone()),
            recorded_unix_ms: now,
            event,
        })?)
    } else {
        None
    };
    let interrupted_bytes = inner.store.encoded_frame_bytes(&interrupted)?;
    let fence_bytes = actor_fence
        .as_ref()
        .map(|receipt| inner.store.encoded_frame_bytes(receipt))
        .transpose()?
        .unwrap_or(0);
    let failed_actor_next_generation = actor_fence.as_ref().map(|fence| match &fence.entry.event {
        JournalEventV2::ActorGenerationRetired {
            next_actor_generation,
            ..
        }
        | JournalEventV2::ActorStateLost {
            next_actor_generation,
            ..
        } => *next_actor_generation,
        _ => unreachable!("pre-start actor fence has one of two event shapes"),
    });
    let needed = interrupted_bytes
        .checked_add(fence_bytes)
        .context("pre-start interruption journal capacity overflow")?;
    if needed > reserved {
        bail!(
            "pre-start interruption records require {needed} bytes, exceeding reserved terminal headroom {reserved}"
        );
    }
    let reserved_after_failed_interruption = state
        .reserved_durable_bytes
        .checked_sub(reserved)
        .context("failed pre-start interruption reservation accounting underflow")?;
    let interrupted_written = match inner.store.append_entry(session_id, &interrupted) {
        Ok(written) => written,
        Err(error) => {
            // OperationAccepted remains the durable head. The physical actor
            // that owned its generation is already gone, so do not expose a
            // live Ready/Executing state or retain unusable terminal escrow.
            // Restart reconstruction will classify and durably fence the
            // accepted-before-start generation from the authoritative head.
            state.reserved_durable_bytes = reserved_after_failed_interruption;
            state.workers.remove(session_id);
            state.unreadable_sessions.push(format!(
                "{session_id}: accepted operation could not publish its pre-start interruption: {error:#}"
            ));
            let session = state.sessions.get_mut(session_id).unwrap();
            session.status = SessionStatusV2::Quarantined;
            if let Some(next_actor_generation) = failed_actor_next_generation {
                // This process must not present or reuse the evaluator whose
                // command channel was lost. The durable Accepted head still
                // carries the prior generation; restart will append its
                // authoritative signed retirement/loss fence.
                session.actor_id = None;
                session.actor_has_state = false;
                session.next_actor_generation = next_actor_generation;
            }
            session
                .operations
                .get_mut(&operation_id)
                .unwrap()
                .reserved_bytes = 0;
            return Err(error)
                .context("accepted operation interruption append failed before actor fencing");
        }
    };
    // OperationInterrupted is independently durable. Apply it before the
    // actor fence so a failure of the second append cannot leave a stale head
    // or uncharged bytes in the live runtime.
    state.durable_bytes = state
        .durable_bytes
        .checked_add(interrupted_written)
        .context("hosted durable-byte accounting overflow")?;
    state.reserved_durable_bytes = state
        .reserved_durable_bytes
        .checked_sub(interrupted_written)
        .context("pre-start interruption reservation accounting underflow")?;
    {
        let session = state.sessions.get_mut(session_id).unwrap();
        session.durable_bytes = session
            .durable_bytes
            .checked_add(interrupted_written)
            .context("hosted session-byte accounting overflow")?;
        apply_receipt_head(session, &interrupted);
        let operation = session.operations.get_mut(&operation_id).unwrap();
        operation.view.status = OperationStatusV2::NotStarted;
        operation.reserved_bytes = reserved
            .checked_sub(interrupted_written)
            .context("pre-start operation reservation underflow")?;
    }

    let fence_written = if let Some(fence) = actor_fence.as_ref() {
        match inner.store.append_entry(session_id, fence) {
            Ok(written) => written,
            Err(error) => {
                let residual_reservation =
                    state.sessions[session_id].operations[&operation_id].reserved_bytes;
                state.reserved_durable_bytes = state
                    .reserved_durable_bytes
                    .checked_sub(residual_reservation)
                    .context("failed actor-fence reservation accounting underflow")?;
                state.workers.remove(session_id);
                state.unreadable_sessions.push(format!(
                    "{session_id}: operation interruption is durable but actor-generation fence append failed: {error:#}"
                ));
                let session = state.sessions.get_mut(session_id).unwrap();
                session.status = SessionStatusV2::Quarantined;
                session
                    .operations
                    .get_mut(&operation_id)
                    .unwrap()
                    .reserved_bytes = 0;
                return Err(error).context(
                    "operation interruption is durable but actor-generation fence append failed",
                );
            }
        }
    } else {
        0
    };
    let residual_reservation = state.sessions[session_id].operations[&operation_id].reserved_bytes;
    state.durable_bytes = state
        .durable_bytes
        .checked_add(fence_written)
        .context("hosted durable-byte accounting overflow")?;
    state.reserved_durable_bytes = state
        .reserved_durable_bytes
        .checked_sub(residual_reservation)
        .context("pre-start terminal reservation accounting underflow")?;
    let session = state.sessions.get_mut(session_id).unwrap();
    session.durable_bytes = session
        .durable_bytes
        .checked_add(fence_written)
        .context("hosted session-byte accounting overflow")?;
    session
        .operations
        .get_mut(&operation_id)
        .unwrap()
        .reserved_bytes = 0;
    if let Some(fence) = actor_fence {
        match &fence.entry.event {
            JournalEventV2::ActorGenerationRetired {
                next_actor_generation,
                ..
            } => {
                session.actor_id = None;
                session.actor_generation = None;
                session.next_actor_generation = *next_actor_generation;
                session.actor_has_state = false;
                session.status = SessionStatusV2::Ready;
            }
            JournalEventV2::ActorStateLost {
                previous_actor_generation,
                next_actor_generation,
                ..
            } => {
                session.actor_id = None;
                session.actor_generation = Some(previous_actor_generation.clone());
                session.next_actor_generation = *next_actor_generation;
                session.actor_has_state = false;
                session.status = SessionStatusV2::RecoveryRequired;
            }
            _ => unreachable!("pre-start actor fence has one of two event shapes"),
        }
        apply_receipt_head(session, &fence);
    } else {
        session.status = SessionStatusV2::Ready;
    }
    Ok(())
}

fn mark_quarantined(inner: &Arc<RuntimeInnerV2>, session_id: &str, reason: &str) {
    let Ok(mut state) = inner.state.lock() else {
        return;
    };
    let reserved = state
        .sessions
        .get(session_id)
        .map(|session| {
            session
                .operations
                .values()
                .map(|operation| operation.reserved_bytes)
                .sum::<u64>()
        })
        .unwrap_or(0);
    state.reserved_durable_bytes = state.reserved_durable_bytes.saturating_sub(reserved);
    state.workers.remove(session_id);
    state
        .unreadable_sessions
        .push(format!("{session_id}: {reason}"));
    if let Some(session) = state.sessions.get_mut(session_id) {
        session.status = SessionStatusV2::Quarantined;
        for operation in session.operations.values_mut() {
            operation.reserved_bytes = 0;
        }
    }
}

fn reconstruct_session(
    node_id: &str,
    store: &RuntimeStoreV2,
    entries: &[SignedJournalEntryV2],
) -> Result<SessionRecordV2> {
    let first = entries.first().context("session journal is empty")?;
    let JournalEventV2::SessionOpened {
        request_sha256,
        principal_sha256,
        bearer_salt,
        bearer_hash,
        capability_commitment,
        state_tier,
        state_session,
        state_quota_generation,
        state_quota_limits,
        state_reservation,
        placement_identity,
        placement_lease_sha256,
        placement_lease_nonce,
        client_request_id,
    } = &first.entry.event
    else {
        bail!("session journal does not begin with SessionOpened");
    };
    validate_sha256_v2("open request_sha256", request_sha256)?;
    let salt = decode_fixed_hex::<32>("bearer_salt", bearer_salt)?;
    validate_sha256_v2("bearer_hash", bearer_hash)?;
    let mut session = SessionRecordV2 {
        session_id: first.entry.session_id.clone(),
        node_id: node_id.to_owned(),
        principal_sha256: principal_sha256.clone(),
        bearer_salt: salt,
        bearer_hash: bearer_hash.clone(),
        open_capability_commitment: capability_commitment.clone(),
        open_request_sha256: request_sha256.clone(),
        open_placement_lease_sha256: placement_lease_sha256.clone(),
        open_placement_lease_nonce: placement_lease_nonce.clone(),
        open_client_request_id: client_request_id.clone(),
        open_receipt: first.clone(),
        state_tier: *state_tier,
        state_session: state_session.clone(),
        state_quota_generation: *state_quota_generation,
        state_quota_limits: state_quota_limits.clone(),
        state_reservation: state_reservation.clone(),
        status: SessionStatusV2::Ready,
        next_client_sequence: 1,
        actor_id: None,
        actor_generation: None,
        next_actor_generation: GenerationV1::new(1).expect("hosted actor generations start at one"),
        actor_has_state: false,
        placement_identity: placement_identity.clone(),
        checkpoint: None,
        recovery_attempt: None,
        durable_bytes: 0,
        operations: BTreeMap::new(),
        commits: BTreeMap::new(),
        journal_sequence: first.entry.sequence,
        journal_head_sha256: first.entry_sha256.clone(),
        head_receipt: first.clone(),
        created_unix_ms: first.entry.recorded_unix_ms,
        updated_unix_ms: first.entry.recorded_unix_ms,
        preparation: None,
    };
    for receipt in &entries[1..] {
        if session.status == SessionStatusV2::Closed {
            bail!("session journal contains an event after SessionClosed");
        }
        match &receipt.entry.event {
            JournalEventV2::SessionOpened { .. } => bail!("duplicate SessionOpened record"),
            JournalEventV2::OperationAccepted {
                client_sequence,
                client_request_id,
                request_sha256,
                operation_id,
                task_attempt,
                operation_sha256,
                source_sha256,
                actor_id,
                actor_generation,
                ..
            } => {
                let prepared = store
                    .read_operation(&session.session_id, operation_id)
                    .with_context(|| {
                        format!(
                            "accepted operation `{operation_id}` has no valid immutable operation blob"
                        )
                    })?;
                if prepared.operation_id != *operation_id
                    || prepared.task_attempt != *task_attempt
                    || prepared.source_sha256 != *source_sha256
                    || prepared.sha256()? != *operation_sha256
                {
                    bail!(
                        "accepted operation `{operation_id}` differs from its immutable operation blob"
                    );
                }
                let reserved_bytes = prepared
                    .output_limit_bytes
                    .checked_add(TERMINAL_RECORD_OVERHEAD_RESERVATION)
                    .context("reconstructed operation reservation overflow")?;
                match (actor_id, actor_generation) {
                    (Some(actor_id), Some(actor)) => {
                        if let Some(current) = &session.actor_generation {
                            if current != actor || session.actor_id.as_ref() != Some(actor_id) {
                                bail!("accepted operation switches actor generation");
                            }
                        } else {
                            if actor.generation() != session.next_actor_generation {
                                bail!("accepted operation skips the next actor generation");
                            }
                            session.actor_id = Some(actor_id.clone());
                            session.actor_generation = Some(actor.clone());
                        }
                    }
                    (None, None) if session.state_tier == SessionStateTierV2::Stateless => {}
                    _ => bail!("accepted operation has an invalid actor binding"),
                }
                session.operations.insert(
                    operation_id.clone(),
                    OperationRecordV2 {
                        view: OperationViewV2 {
                            operation_id: operation_id.clone(),
                            task_attempt: task_attempt.clone(),
                            operation_sha256: operation_sha256.clone(),
                            status: OperationStatusV2::Accepted,
                            accepted_unix_ms: receipt.entry.recorded_unix_ms,
                            started_unix_ms: None,
                            finished_unix_ms: None,
                            outcome: None,
                        },
                        reserved_bytes,
                    },
                );
                session.status = SessionStatusV2::Executing;
                record_commit(
                    &mut session,
                    *client_sequence,
                    client_request_id.clone(),
                    request_sha256.clone(),
                    receipt.clone(),
                )?;
            }
            JournalEventV2::OperationStarted {
                operation_id,
                actor_generation,
                started_unix_ms,
                ..
            } => {
                if actor_generation != &session.actor_generation {
                    bail!("operation start names a different actor generation");
                }
                let operation = session
                    .operations
                    .get_mut(operation_id)
                    .context("started record names an unknown operation")?;
                let started_bytes = store.encoded_frame_bytes(receipt)?;
                operation.reserved_bytes = operation
                    .reserved_bytes
                    .checked_sub(started_bytes)
                    .context("operation-start record exceeds its durable terminal reservation")?;
                operation.view.status = OperationStatusV2::Running;
                operation.view.started_unix_ms = Some(*started_unix_ms);
                session.status = SessionStatusV2::Executing;
            }
            JournalEventV2::ActorCheckpointed {
                actor_generation,
                snapshot_sha256,
                snapshot_bytes,
            } => {
                validate_sha256_v2("snapshot_sha256", snapshot_sha256)?;
                if session.actor_generation.as_ref() != Some(actor_generation) {
                    bail!("checkpoint record names a stale actor generation");
                }
                session.checkpoint = Some(DurableCheckpointV2 {
                    actor_generation: actor_generation.clone(),
                    snapshot_sha256: snapshot_sha256.clone(),
                    snapshot_bytes: *snapshot_bytes,
                });
                session.actor_has_state = true;
            }
            JournalEventV2::ActorCheckpointFailed {
                actor_generation, ..
            } => {
                if session.actor_generation.as_ref() != Some(actor_generation) {
                    bail!("checkpoint failure names a stale actor generation");
                }
                let operation = session
                    .operations
                    .values_mut()
                    .find(|operation| operation.view.status == OperationStatusV2::Running)
                    .context("checkpoint failure has no running operation")?;
                let failure_bytes = store.encoded_frame_bytes(receipt)?;
                operation.reserved_bytes = operation
                    .reserved_bytes
                    .checked_sub(failure_bytes)
                    .context("checkpoint-failure record exceeds terminal reservation")?;
                session.status = SessionStatusV2::Executing;
                session.actor_has_state = true;
            }
            JournalEventV2::OperationTerminal {
                operation_id,
                finished_unix_ms,
                outcome,
                state_durable,
                actor_state_touched,
                ..
            } => {
                if session.status != SessionStatusV2::Executing {
                    bail!("operation terminal appears outside an executing transition");
                }
                let operation = session
                    .operations
                    .get_mut(operation_id)
                    .context("terminal record names an unknown operation")?;
                if operation.view.status != OperationStatusV2::Running {
                    bail!("operation terminal does not settle the active running operation");
                }
                operation.view.status = match outcome {
                    OperationOutcomeV2::Succeeded { .. } => OperationStatusV2::Succeeded,
                    OperationOutcomeV2::Failed { .. } => OperationStatusV2::Failed,
                };
                operation.view.finished_unix_ms = Some(*finished_unix_ms);
                operation.view.outcome = Some(outcome.clone());
                operation.reserved_bytes = 0;
                session.status = if *state_durable {
                    SessionStatusV2::Ready
                } else {
                    SessionStatusV2::RecoveryRequired
                };
                if *actor_state_touched && session.state_tier.needs_live_actor() {
                    session.actor_has_state = true;
                }
            }
            JournalEventV2::OperationInterrupted {
                operation_id,
                classification,
                ..
            } => {
                let operation = session
                    .operations
                    .get_mut(operation_id)
                    .context("interruption record names an unknown operation")?;
                operation.view.status = *classification;
                operation.reserved_bytes = 0;
                session.status = if *classification == OperationStatusV2::Ambiguous {
                    SessionStatusV2::RecoveryRequired
                } else {
                    SessionStatusV2::Ready
                };
            }
            JournalEventV2::ActorStateLost {
                previous_actor_generation,
                next_actor_generation,
                ..
            } => {
                if session.actor_generation.as_ref() != Some(previous_actor_generation)
                    || next_actor_generation.get()
                        != previous_actor_generation
                            .generation()
                            .get()
                            .saturating_add(1)
                {
                    bail!("actor-state-loss record has an invalid generation transition");
                }
                session.actor_id = None;
                session.actor_generation = Some(previous_actor_generation.clone());
                session.next_actor_generation = *next_actor_generation;
                session.actor_has_state = false;
                session.status = SessionStatusV2::RecoveryRequired;
            }
            JournalEventV2::ActorGenerationRetired {
                previous_actor_generation,
                next_actor_generation,
                ..
            } => {
                if session.actor_generation.as_ref() != Some(previous_actor_generation)
                    || session.actor_has_state
                    || next_actor_generation.get()
                        != previous_actor_generation
                            .generation()
                            .get()
                            .saturating_add(1)
                {
                    bail!("actor-retirement record has an invalid generation transition");
                }
                session.actor_id = None;
                session.actor_generation = None;
                session.next_actor_generation = *next_actor_generation;
                session.actor_has_state = false;
                session.status = SessionStatusV2::Ready;
            }
            JournalEventV2::ActorRestored {
                previous_actor_generation,
                actor_generation,
                actor_id,
                snapshot_sha256,
                snapshot_bytes,
            } => {
                if session.actor_generation.as_ref() != Some(previous_actor_generation)
                    || successor_actor_generation(previous_actor_generation)? != *actor_generation
                {
                    bail!("actor restore record has an invalid generation transition");
                }
                let checkpoint = session
                    .checkpoint
                    .as_mut()
                    .context("actor restore has no durable checkpoint")?;
                if checkpoint.snapshot_sha256 != *snapshot_sha256
                    || checkpoint.snapshot_bytes != *snapshot_bytes
                {
                    bail!("actor restore record names a different checkpoint");
                }
                checkpoint.actor_generation = actor_generation.clone();
                session.actor_id = Some(actor_id.clone());
                session.actor_generation = Some(actor_generation.clone());
                session.next_actor_generation = actor_generation.generation();
                session.actor_has_state = true;
                session.status = SessionStatusV2::Ready;
            }
            JournalEventV2::SessionReset {
                client_sequence,
                client_request_id,
                request_sha256,
                previous_actor_generation,
                next_actor_generation,
                ..
            } => {
                if session.operations.values().any(|operation| {
                    matches!(
                        operation.view.status,
                        OperationStatusV2::Accepted | OperationStatusV2::Running
                    )
                }) {
                    bail!("session reset appears while an operation is accepted or running");
                }
                if previous_actor_generation != &session.actor_generation {
                    bail!("session reset names a different prior actor generation");
                }
                session.actor_id = None;
                session.actor_generation = None;
                session.next_actor_generation = *next_actor_generation;
                session.actor_has_state = false;
                session.checkpoint = None;
                session.status = SessionStatusV2::Ready;
                record_commit(
                    &mut session,
                    *client_sequence,
                    client_request_id.clone(),
                    request_sha256.clone(),
                    receipt.clone(),
                )?;
            }
            JournalEventV2::RecoveryAttemptStarted {
                client_sequence,
                client_request_id,
                request_sha256,
                warrant_sha256,
                placement_lease_sha256,
                placement_lease_nonce,
                trigger,
                previous_actor_generation,
                attempted_actor_generation,
                checkpoint_sha256,
                checkpoint_bytes,
            } => {
                if session.recovery_attempt.is_some()
                    || session.status != SessionStatusV2::RecoveryRequired
                    || session.next_client_sequence != *client_sequence
                    || session.actor_id.is_some()
                    || session.actor_has_state
                    || session.actor_generation.as_ref() != Some(previous_actor_generation)
                    || successor_actor_generation(previous_actor_generation)?
                        != *attempted_actor_generation
                    || session.next_actor_generation != attempted_actor_generation.generation()
                {
                    bail!(
                        "recovery-attempt allocation has invalid session or generation coordinates"
                    );
                }
                validate_identifier_v2("recovery client_request_id", client_request_id)?;
                validate_sha256_v2("recovery request_sha256", request_sha256)?;
                validate_sha256_v2("recovery warrant_sha256", warrant_sha256)?;
                validate_sha256_v2("recovery placement_lease_sha256", placement_lease_sha256)?;
                validate_sha256_v2("recovery placement_lease_nonce", placement_lease_nonce)?;
                validate_recovery_trigger(&session, trigger)?;
                let checkpoint = session
                    .checkpoint
                    .as_ref()
                    .context("recovery-attempt allocation has no durable checkpoint")?;
                if checkpoint.snapshot_sha256 != *checkpoint_sha256
                    || checkpoint.snapshot_bytes != *checkpoint_bytes
                    || !same_actor_lineage(previous_actor_generation, &checkpoint.actor_generation)
                    || checkpoint.actor_generation.generation().get()
                        > previous_actor_generation.generation().get()
                {
                    bail!("recovery-attempt allocation names a different durable checkpoint");
                }
                session.recovery_attempt = Some(RecoveryAttemptV2 {
                    receipt_sha256: receipt.entry_sha256.clone(),
                    client_sequence: *client_sequence,
                    client_request_id: client_request_id.clone(),
                    request_sha256: request_sha256.clone(),
                    warrant_sha256: warrant_sha256.clone(),
                    placement_lease_sha256: placement_lease_sha256.clone(),
                    placement_lease_nonce: placement_lease_nonce.clone(),
                    trigger: trigger.clone(),
                    previous_actor_generation: previous_actor_generation.clone(),
                    attempted_actor_generation: attempted_actor_generation.clone(),
                    checkpoint_sha256: checkpoint_sha256.clone(),
                    checkpoint_bytes: *checkpoint_bytes,
                    reserved_bytes: RECOVERY_TERMINAL_HEADROOM_RESERVATION,
                });
                session.actor_generation = Some(attempted_actor_generation.clone());
                session.next_actor_generation =
                    successor_actor_generation(attempted_actor_generation)?.generation();
                session.actor_id = None;
                session.actor_has_state = false;
            }
            JournalEventV2::RecoveryCommitted {
                client_sequence,
                client_request_id,
                request_sha256,
                warrant_sha256,
                placement_lease_sha256,
                placement_lease_nonce,
                recovery_attempt_sha256,
                trigger,
                previous_actor_generation,
                actor_generation,
                actor_id,
                checkpoint_sha256,
                checkpoint_bytes,
                ..
            } => {
                if session.state_tier != SessionStateTierV2::CheckpointRestore {
                    bail!("recovery commit appears on a non-checkpoint session");
                }
                let attempt = session
                    .recovery_attempt
                    .as_ref()
                    .context("recovery commit has no durable attempt allocation")?;
                if recovery_attempt_sha256 != &attempt.receipt_sha256
                    || receipt.entry.previous_entry_sha256.as_ref() != Some(&attempt.receipt_sha256)
                    || *client_sequence != attempt.client_sequence
                    || client_request_id != &attempt.client_request_id
                    || request_sha256 != &attempt.request_sha256
                    || warrant_sha256 != &attempt.warrant_sha256
                    || placement_lease_sha256 != &attempt.placement_lease_sha256
                    || placement_lease_nonce != &attempt.placement_lease_nonce
                    || trigger != &attempt.trigger
                    || previous_actor_generation != &attempt.previous_actor_generation
                    || actor_generation != &attempt.attempted_actor_generation
                    || session.actor_generation.as_ref() != Some(actor_generation)
                {
                    bail!("recovery commit differs from its durable attempt allocation");
                }
                match trigger {
                    RecoveryTriggerV2::AmbiguousOperation {
                        operation_id,
                        operation_sha256,
                        ..
                    } => {
                        let operation = session
                            .operations
                            .get(operation_id)
                            .context("recovery commit names an unknown operation")?;
                        if operation.view.status != OperationStatusV2::Ambiguous
                            || operation.view.operation_sha256 != *operation_sha256
                        {
                            bail!("recovery commit does not resolve the exact ambiguous operation");
                        }
                    }
                    RecoveryTriggerV2::ActorLost {
                        previous_actor_generation: trigger_previous,
                        checkpoint_sha256: trigger_checkpoint_sha256,
                        checkpoint_bytes: trigger_checkpoint_bytes,
                        recovery_required_head_sha256: _,
                    } => {
                        let checkpoint = session
                            .checkpoint
                            .as_ref()
                            .context("actor-loss recovery commit has no checkpoint")?;
                        if trigger_previous != previous_actor_generation
                            || !same_actor_lineage(trigger_previous, &checkpoint.actor_generation)
                            || checkpoint.actor_generation.generation().get()
                                > trigger_previous.generation().get()
                            || checkpoint.snapshot_sha256 != trigger_checkpoint_sha256.as_str()
                            || checkpoint.snapshot_bytes != *trigger_checkpoint_bytes
                            || session.actor_id.is_some()
                            || session.actor_has_state
                        {
                            bail!(
                                "actor-loss recovery commit differs from its exact fenced checkpoint trigger"
                            );
                        }
                    }
                }
                match (
                    session.checkpoint.as_mut(),
                    checkpoint_sha256,
                    checkpoint_bytes,
                ) {
                    (Some(checkpoint), Some(expected_sha256), Some(expected_bytes))
                        if checkpoint.snapshot_sha256 == *expected_sha256
                            && checkpoint.snapshot_bytes == *expected_bytes =>
                    {
                        checkpoint.actor_generation = actor_generation.clone();
                    }
                    (None, None, None) => {}
                    _ => bail!("recovery commit checkpoint does not match durable state"),
                }
                if let RecoveryTriggerV2::AmbiguousOperation { operation_id, .. } = trigger {
                    let operation = session
                        .operations
                        .get_mut(operation_id)
                        .expect("ambiguous recovery operation was validated");
                    operation.view.status = OperationStatusV2::Failed;
                    operation.view.finished_unix_ms = Some(receipt.entry.recorded_unix_ms);
                    operation.view.outcome = Some(OperationOutcomeV2::failed(
                        OperationFailureStageV2::Infrastructure,
                        "ambiguous-attempt-recovered",
                        "attempt outcome was ambiguous; authenticated recovery restored the last durable actor checkpoint without replay",
                    ));
                    operation.reserved_bytes = 0;
                }
                session.actor_id = Some(actor_id.clone());
                session.actor_generation = Some(actor_generation.clone());
                session.next_actor_generation = actor_generation.generation();
                session.actor_has_state = session.checkpoint.is_some();
                session.status = SessionStatusV2::Ready;
                session.recovery_attempt = None;
                record_commit(
                    &mut session,
                    *client_sequence,
                    client_request_id.clone(),
                    request_sha256.clone(),
                    receipt.clone(),
                )?;
            }
            JournalEventV2::RecoveryRefused {
                client_sequence,
                client_request_id,
                request_sha256,
                warrant_sha256,
                placement_lease_sha256,
                placement_lease_nonce,
                recovery_attempt_sha256,
                attempted_actor_generation,
                ..
            } => {
                match (
                    session.recovery_attempt.as_ref(),
                    recovery_attempt_sha256,
                    attempted_actor_generation,
                ) {
                    (Some(attempt), Some(attempt_sha256), Some(attempted))
                        if attempt_sha256 == &attempt.receipt_sha256
                            && receipt.entry.previous_entry_sha256.as_ref()
                                == Some(&attempt.receipt_sha256)
                            && *client_sequence == attempt.client_sequence
                            && client_request_id == &attempt.client_request_id
                            && request_sha256 == &attempt.request_sha256
                            && warrant_sha256 == &attempt.warrant_sha256
                            && placement_lease_sha256 == &attempt.placement_lease_sha256
                            && placement_lease_nonce == &attempt.placement_lease_nonce
                            && attempted == &attempt.attempted_actor_generation => {}
                    (None, None, None) => {}
                    _ => bail!("recovery refusal differs from its durable attempt allocation"),
                }
                session.recovery_attempt = None;
                session.status = SessionStatusV2::RecoveryRequired;
                record_commit(
                    &mut session,
                    *client_sequence,
                    client_request_id.clone(),
                    request_sha256.clone(),
                    receipt.clone(),
                )?;
            }
            JournalEventV2::PlacementLeaseRefused { .. }
            | JournalEventV2::ClosedSessionGcAuthorized { .. }
            | JournalEventV2::ClosedSessionGcCompleted { .. }
            | JournalEventV2::JournalTailRepaired { .. } => {
                bail!("session journal contains a placement-authority refusal")
            }
            JournalEventV2::SessionClosed {
                client_sequence,
                client_request_id,
                request_sha256,
                ..
            } => {
                if session.operations.values().any(|operation| {
                    matches!(
                        operation.view.status,
                        OperationStatusV2::Accepted | OperationStatusV2::Running
                    )
                }) {
                    bail!("session close appears while an operation is accepted or running");
                }
                session.status = SessionStatusV2::Closed;
                for operation in session.operations.values_mut() {
                    operation.reserved_bytes = 0;
                }
                session.recovery_attempt = None;
                record_commit(
                    &mut session,
                    *client_sequence,
                    client_request_id.clone(),
                    request_sha256.clone(),
                    receipt.clone(),
                )?;
            }
        }
        apply_receipt_head(&mut session, receipt);
    }
    Ok(session)
}

fn authenticate_locked(
    state: &RuntimeStateV2,
    principal_sha256: &str,
    credentials: &SessionCredentialsV2,
) -> Result<()> {
    validate_sha256_v2("principal_sha256", principal_sha256)?;
    credentials.validate()?;
    let session = state.sessions.get(&credentials.session_id).ok_or_else(|| {
        reject(
            "session-not-found",
            "session does not exist on this node",
            false,
        )
    })?;
    if !constant_time_eq(
        session.principal_sha256.as_bytes(),
        principal_sha256.as_bytes(),
    ) {
        return Err(reject(
            "session-principal-mismatch",
            "session is bound to a different authenticated client certificate",
            false,
        ));
    }
    let bearer = decode_fixed_hex::<32>("session bearer", &credentials.bearer)?;
    let actual = salted_bearer_hash(&session.bearer_salt, &bearer);
    if !constant_time_eq(actual.as_bytes(), session.bearer_hash.as_bytes()) {
        return Err(reject(
            "session-bearer-invalid",
            "session bearer capability is invalid",
            false,
        ));
    }
    Ok(())
}

fn duplicate_open_response(
    state: &RuntimeStateV2,
    principal_sha256: &str,
    request: &OpenSessionRequestV2,
    proposed_bearer: &[u8; 32],
    open_request_sha256: &str,
    placement_lease_sha256: &str,
    placement_lease_nonce: &str,
) -> Result<Option<HostedResponseV2>> {
    let Some(session) = state.sessions.get(&request.proposed_capability.session_id) else {
        return Ok(None);
    };
    let command = &request.placement_lease.command;
    let actual_bearer_hash = salted_bearer_hash(&session.bearer_salt, proposed_bearer);
    let exact = constant_time_eq(
        session.principal_sha256.as_bytes(),
        principal_sha256.as_bytes(),
    ) && session.open_request_sha256 == open_request_sha256
        && session.open_client_request_id == request.client_request_id
        && session.state_tier == request.state_tier
        && session.state_session == command.state_session
        && session.state_quota_generation == command.state_quota_generation
        && session.state_quota_limits == command.state_quota_limits
        && session.state_reservation == command.state_reservation
        && session.open_capability_commitment == request.capability_commitment
        && session.open_placement_lease_sha256 == placement_lease_sha256
        && session.open_placement_lease_nonce == placement_lease_nonce
        && constant_time_eq(
            session.bearer_hash.as_bytes(),
            actual_bearer_hash.as_bytes(),
        );
    if !exact {
        return Err(reject(
            "open-retry-conflict",
            "state session was already opened by different principal, request, lease, tier, or capability bytes",
            false,
        ));
    }
    Ok(Some(HostedResponseV2::SessionOpened {
        capability: request.proposed_capability.clone(),
        receipt: session.open_receipt.clone(),
    }))
}

fn clear_preparation(
    inner: &Arc<RuntimeInnerV2>,
    session_id: &str,
    reservation: &PreparationReservationV2,
) -> Result<()> {
    let mut state = inner
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("hosted V2 state lock is poisoned"))?;
    if let Some(session) = state.sessions.get_mut(session_id) {
        if session.preparation.as_ref() == Some(reservation) {
            session.preparation = None;
        }
    }
    Ok(())
}

fn recovery_coordinates_unchanged(
    session: &SessionRecordV2,
    reservation: &PreparationReservationV2,
    request: &RecoverSessionRequestV2,
) -> bool {
    session.preparation.as_ref() == Some(reservation)
        && session.status == SessionStatusV2::RecoveryRequired
        && session.next_client_sequence == request.client_sequence
        && session.journal_head_sha256 == reservation.journal_head_sha256
        && request.warrant.evidence_sha256 == session.journal_head_sha256
        && reservation.operation_id == request.warrant.warrant_id
        && validate_recovery_trigger(session, &request.warrant.trigger).is_ok()
}

fn recovery_attempt_coordinates_unchanged(
    session: &SessionRecordV2,
    reservation: &PreparationReservationV2,
    request: &RecoverSessionRequestV2,
    attempted_actor_generation: &ActorGenerationIdV1,
) -> bool {
    let Some(attempt) = session.recovery_attempt.as_ref() else {
        return false;
    };
    let checkpoint_matches = session.checkpoint.as_ref().is_some_and(|checkpoint| {
        checkpoint.snapshot_sha256 == attempt.checkpoint_sha256
            && checkpoint.snapshot_bytes == attempt.checkpoint_bytes
    });
    session.preparation.as_ref() == Some(reservation)
        && session.status == SessionStatusV2::RecoveryRequired
        && session.next_client_sequence == request.client_sequence
        && session.journal_head_sha256 == attempt.receipt_sha256
        && reservation.operation_id == request.warrant.warrant_id
        && attempt.client_sequence == request.client_sequence
        && attempt.client_request_id == request.client_request_id
        && attempt.request_sha256 == reservation.request_sha256
        && attempt.trigger == request.warrant.trigger
        && attempt.attempted_actor_generation == *attempted_actor_generation
        && checkpoint_matches
        && session.actor_generation.as_ref() == Some(attempted_actor_generation)
        && session.actor_id.is_none()
        && !session.actor_has_state
}

fn validate_recovery_trigger(session: &SessionRecordV2, trigger: &RecoveryTriggerV2) -> Result<()> {
    match trigger {
        RecoveryTriggerV2::AmbiguousOperation {
            operation_id,
            operation_sha256,
            ..
        } => {
            let operation = session
                .operations
                .get(operation_id)
                .filter(|operation| operation.view.status == OperationStatusV2::Ambiguous)
                .ok_or_else(|| {
                    reject(
                        "recovery-warrant-mismatch",
                        "recovery warrant does not identify an ambiguous session operation",
                        false,
                    )
                })?;
            if operation.view.operation_sha256 != *operation_sha256 {
                return Err(reject(
                    "recovery-warrant-mismatch",
                    "recovery warrant operation digest mismatch",
                    false,
                ));
            }
        }
        RecoveryTriggerV2::ActorLost {
            previous_actor_generation,
            checkpoint_sha256,
            checkpoint_bytes,
            recovery_required_head_sha256,
        } => {
            let checkpoint = session.checkpoint.as_ref().ok_or_else(|| {
                reject(
                    "recovery-warrant-mismatch",
                    "actor-loss recovery has no durable checkpoint",
                    false,
                )
            })?;
            let next = successor_actor_generation(previous_actor_generation)?;
            if session.state_tier != SessionStateTierV2::CheckpointRestore
                || session.status != SessionStatusV2::RecoveryRequired
                || session.actor_id.is_some()
                || session.actor_has_state
                || session.actor_generation.as_ref() != Some(previous_actor_generation)
                || session.next_actor_generation != next.generation()
                || !same_actor_lineage(previous_actor_generation, &checkpoint.actor_generation)
                || checkpoint.actor_generation.generation().get()
                    > previous_actor_generation.generation().get()
                || checkpoint.snapshot_sha256 != checkpoint_sha256.as_str()
                || checkpoint.snapshot_bytes != *checkpoint_bytes
                || session.journal_head_sha256 != recovery_required_head_sha256.as_str()
            {
                return Err(reject(
                    "recovery-warrant-mismatch",
                    "actor-loss recovery trigger differs from the exact fenced generation, checkpoint, or signed journal head",
                    false,
                ));
            }
        }
    }
    Ok(())
}

fn duplicate_commit(
    state: &RuntimeStateV2,
    session_id: &str,
    sequence: u64,
    request_id: &str,
    request_sha256: &str,
) -> Result<Option<SignedJournalEntryV2>> {
    let session = &state.sessions[session_id];
    if sequence >= session.next_client_sequence {
        return Ok(None);
    }
    let committed = session.commits.get(&sequence).ok_or_else(|| {
        reject(
            "sequence-conflict",
            "client sequence is older than the retained mutation ledger",
            false,
        )
    })?;
    if committed.request_id != request_id || committed.request_sha256 != request_sha256 {
        return Err(reject(
            "sequence-conflict",
            "client sequence was already committed for different request bytes",
            false,
        ));
    }
    Ok(Some(committed.receipt.clone()))
}

fn require_next_sequence(state: &RuntimeStateV2, session_id: &str, sequence: u64) -> Result<()> {
    let expected = state.sessions[session_id].next_client_sequence;
    if sequence > expected {
        return Err(reject(
            "sequence-gap",
            format!("expected client sequence {expected}, received {sequence}"),
            true,
        ));
    }
    if sequence < expected {
        return Err(reject(
            "sequence-conflict",
            format!("client sequence {sequence} was already consumed"),
            false,
        ));
    }
    Ok(())
}

fn record_commit(
    session: &mut SessionRecordV2,
    sequence: u64,
    request_id: String,
    request_sha256: String,
    receipt: SignedJournalEntryV2,
) -> Result<()> {
    if sequence != session.next_client_sequence {
        bail!("internal hosted V2 mutation sequence discontinuity");
    }
    session.commits.insert(
        sequence,
        ClientCommitV2 {
            request_id,
            request_sha256,
            receipt,
        },
    );
    session.next_client_sequence = session
        .next_client_sequence
        .checked_add(1)
        .context("hosted V2 client sequence overflow")?;
    Ok(())
}

fn apply_receipt_head(session: &mut SessionRecordV2, receipt: &SignedJournalEntryV2) {
    session.journal_sequence = receipt.entry.sequence;
    session.journal_head_sha256 = receipt.entry_sha256.clone();
    session.updated_unix_ms = receipt.entry.recorded_unix_ms;
    session.head_receipt = receipt.clone();
}

fn session_view(session: &SessionRecordV2, observed: u64) -> SessionViewV2 {
    SessionViewV2 {
        schema: HOSTED_SESSION_SCHEMA_V2.to_owned(),
        session_id: session.session_id.clone(),
        node_id: session.node_id.clone(),
        principal_sha256: session.principal_sha256.clone(),
        state_tier: session.state_tier,
        status: session.status,
        next_client_sequence: session.next_client_sequence,
        actor: actor_observation(session, observed),
        operations: session
            .operations
            .iter()
            .map(|(id, operation)| (id.clone(), operation.view.clone()))
            .collect(),
        journal_head_sha256: session.journal_head_sha256.clone(),
        created_unix_ms: session.created_unix_ms,
        updated_unix_ms: session.updated_unix_ms,
    }
}

fn actor_observation(session: &SessionRecordV2, observed: u64) -> ActorObservationV2 {
    let health = match session.status {
        SessionStatusV2::Ready => ActorHealthV2::Ready,
        SessionStatusV2::Executing | SessionStatusV2::Closing => ActorHealthV2::Busy,
        SessionStatusV2::RecoveryRequired => ActorHealthV2::RecoveryRequired,
        SessionStatusV2::Quarantined => ActorHealthV2::Quarantined,
        SessionStatusV2::Closed => ActorHealthV2::Closed,
    };
    ActorObservationV2 {
        actor_id: session.actor_id.clone(),
        actor_generation: session.actor_generation.clone(),
        next_actor_generation: session.next_actor_generation,
        state_tier: session.state_tier,
        retained: session.status != SessionStatusV2::Closed,
        health,
        checkpoint_sha256: session
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.snapshot_sha256.clone()),
        checkpoint_bytes: session
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.snapshot_bytes),
        observed_unix_ms: observed,
    }
}

fn ensure_session_durable_capacity(
    state: &RuntimeStateV2,
    session_id: &str,
    additional: u64,
) -> Result<()> {
    let session = state
        .sessions
        .get(session_id)
        .context("hosted session disappeared during capacity check")?;
    let terminal_reservations = session
        .operations
        .values()
        .try_fold(0_u64, |total, operation| {
            total.checked_add(operation.reserved_bytes)
        })
        .context("hosted terminal reservation arithmetic overflow")?;
    let projected = session
        .durable_bytes
        .checked_add(terminal_reservations)
        .and_then(|value| value.checked_add(additional))
        .and_then(|value| value.checked_add(SESSION_CLOSE_HEADROOM_RESERVATION))
        .and_then(|value| value.checked_add(ACTOR_FENCE_HEADROOM_RESERVATION))
        .context("hosted session-byte quota arithmetic overflow")?;
    if projected > session.state_reservation.state_bytes() {
        return Err(quota_rejection("state bytes per session"));
    }
    Ok(())
}

fn ensure_actor_fence_durable_capacity(
    state: &RuntimeStateV2,
    session_id: &str,
    additional: u64,
) -> Result<()> {
    if additional > ACTOR_FENCE_HEADROOM_RESERVATION {
        bail!(
            "actor fence requires {additional} bytes, exceeding reserved control headroom {ACTOR_FENCE_HEADROOM_RESERVATION}"
        );
    }
    let session = state
        .sessions
        .get(session_id)
        .context("hosted session disappeared during actor-fence capacity check")?;
    let terminal_reservations = session
        .operations
        .values()
        .try_fold(0_u64, |total, operation| {
            total.checked_add(operation.reserved_bytes)
        })
        .context("hosted terminal reservation arithmetic overflow")?;
    let projected = session
        .durable_bytes
        .checked_add(terminal_reservations)
        .and_then(|value| value.checked_add(additional))
        .and_then(|value| value.checked_add(SESSION_CLOSE_HEADROOM_RESERVATION))
        .context("hosted actor-fence quota arithmetic overflow")?;
    if projected > session.state_reservation.state_bytes() {
        return Err(quota_rejection("state bytes per session"));
    }
    Ok(())
}

fn ensure_close_durable_capacity(
    state: &RuntimeStateV2,
    session_id: &str,
    close_record_bytes: u64,
) -> Result<()> {
    if close_record_bytes > SESSION_CLOSE_HEADROOM_RESERVATION {
        bail!(
            "session close record requires {close_record_bytes} bytes, exceeding reserved control headroom {SESSION_CLOSE_HEADROOM_RESERVATION}"
        );
    }
    let session = state
        .sessions
        .get(session_id)
        .context("hosted session disappeared during close capacity check")?;
    let terminal_reservations = session
        .operations
        .values()
        .try_fold(0_u64, |total, operation| {
            total.checked_add(operation.reserved_bytes)
        })
        .context("hosted terminal reservation arithmetic overflow")?;
    let projected = session
        .durable_bytes
        .checked_add(terminal_reservations)
        .and_then(|value| value.checked_add(close_record_bytes))
        .context("hosted close quota arithmetic overflow")?;
    if projected > session.state_reservation.state_bytes() {
        return Err(quota_rejection("state bytes per session"));
    }
    Ok(())
}

fn reserved_state_capacity(state: &RuntimeStateV2) -> Result<u64> {
    let session_bytes = state
        .sessions
        .values()
        .try_fold(0_u64, |total, session| {
            total.checked_add(session.durable_bytes)
        })
        .context("hosted session-byte accounting overflow")?;
    let authority_and_metadata_bytes = state
        .durable_bytes
        .checked_sub(session_bytes)
        .context("hosted root durable bytes are below reconstructed session bytes")?;
    let closed_bytes = state
        .sessions
        .values()
        .filter(|session| session.status == SessionStatusV2::Closed)
        .try_fold(0_u64, |total, session| {
            total.checked_add(session.durable_bytes)
        })
        .context("closed hosted state-byte accounting overflow")?;
    state
        .state_bytes_reserved
        .checked_add(closed_bytes)
        .and_then(|value| value.checked_add(authority_and_metadata_bytes))
        .and_then(|value| value.checked_add(state.authority_control_headroom_bytes))
        .context("hosted state-byte reservation accounting overflow")
}

fn open_admission_basis(state: &RuntimeStateV2) -> Result<OpenAdmissionBasisV2> {
    let active_sessions = state
        .sessions
        .values()
        .filter(|session| session.status != SessionStatusV2::Closed)
        .count()
        .try_into()
        .context("hosted open-session count exceeds protocol range")?;
    Ok(OpenAdmissionBasisV2 {
        active_sessions,
        reserved_state_bytes: reserved_state_capacity(state)?,
    })
}

fn open_freshness_deadline(request: &OpenSessionRequestV2) -> Result<u64> {
    request
        .placement_lease
        .state_capacity_observation
        .as_ref()
        .context("open-session placement proof omits state-capacity evidence")?;
    Ok(placement_freshness_deadline(&request.placement_lease))
}

fn placement_freshness_deadline(lease: &SignedPlacementLeaseV2) -> u64 {
    let mut deadline = [
        lease.authority.expires_at().get(),
        lease.evidence.node_profile.expires_at().get(),
        lease.evidence.capacity_observation.expires_at().get(),
    ]
    .into_iter()
    .min()
    .expect("placement freshness set is nonempty");
    if let Some(state_capacity) = lease.state_capacity_observation.as_ref() {
        deadline = deadline.min(state_capacity.expires_at().get());
    }
    for warrant in &lease.evidence.warrants {
        if let Some(expires_at) = warrant.expires_at() {
            deadline = deadline.min(expires_at.get());
        }
    }
    deadline
}

fn successor_actor_generation(actor: &ActorGenerationIdV1) -> Result<ActorGenerationIdV1> {
    let next = actor
        .generation()
        .get()
        .checked_add(1)
        .context("hosted actor generation overflow")?;
    Ok(ActorGenerationIdV1::new(
        actor.logical_environment().clone(),
        actor.backend_implementation().clone(),
        actor.target_descriptor().clone(),
        actor.sandbox_policy().clone(),
        actor.launch_context().clone(),
        GenerationV1::new(next).context("hosted actor generation must be nonzero")?,
    ))
}

fn same_actor_lineage(left: &ActorGenerationIdV1, right: &ActorGenerationIdV1) -> bool {
    left.logical_environment() == right.logical_environment()
        && left.backend_implementation() == right.backend_implementation()
        && left.target_descriptor() == right.target_descriptor()
        && left.sandbox_policy() == right.sandbox_policy()
        && left.launch_context() == right.launch_context()
}

fn quota_rejection(name: &str) -> anyhow::Error {
    reject(
        "quota-exceeded",
        format!("hosted V2 {name} hard quota is exhausted; no session was evicted"),
        true,
    )
}

fn error_response(error: anyhow::Error) -> HostedResponseV2 {
    if error.downcast_ref::<HostedV2RuntimeClosedV2>().is_some() {
        return HostedResponseV2::Error {
            error: HostedProtocolErrorV2::new("runtime-closed", format!("{error:#}"), false),
        };
    }
    if let Some(rejection) = error.downcast_ref::<HostedV2Rejection>() {
        return HostedResponseV2::Error {
            error: HostedProtocolErrorV2::new(
                rejection.code,
                rejection.message.clone(),
                rejection.retryable,
            ),
        };
    }
    if error
        .downcast_ref::<DurableStoreReopenRequiredV2>()
        .is_some()
    {
        return HostedResponseV2::Error {
            error: HostedProtocolErrorV2::new("store-reopen-required", format!("{error:#}"), false),
        };
    }
    HostedResponseV2::Error {
        error: HostedProtocolErrorV2::new("internal-error", format!("{error:#}"), true),
    }
}

fn random_32() -> Result<[u8; 32]> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).context("failed to obtain entropy for hosted V2 capability")?;
    Ok(bytes)
}

fn fresh_identifier(prefix: &str) -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).context("failed to obtain entropy for hosted V2 identity")?;
    Ok(format!("{prefix}-{}", hex::encode(bytes)))
}
