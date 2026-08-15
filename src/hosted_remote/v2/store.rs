use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use thiserror::Error;

use crate::backend::state::EvaluatorStateSnapshotV1;

use sha2::{Digest, Sha256};

use super::super::protocol::{
    canonical_hosted_bytes, read_hosted_frame, unix_time_ms, write_hosted_frame,
    MAX_HOSTED_FRAME_BYTES,
};
use super::crypto::{ensure_private_directory_v2, sync_directory, HostedNodeSignerV2};
use super::protocol::{
    validate_identifier_v2, validate_sha256_v2, JournalEntryV2, JournalEventV2,
    PreparedOperationV2, SignedJournalEntryV2, HOSTED_JOURNAL_ENTRY_SCHEMA_V2,
};

const SESSIONS_DIRECTORY: &str = "sessions";
const SESSION_STAGING_DIRECTORY: &str = "session-staging";
const GC_TOMBSTONES_DIRECTORY: &str = "gc-tombstones";
const OPERATIONS_DIRECTORY: &str = "operations";
const CHECKPOINTS_DIRECTORY: &str = "checkpoints";
const JOURNAL_FILE: &str = "journal.cborseq";
const AUTHORITY_JOURNAL_FILE: &str = "placement-authority.cborseq";
const STATE_LOCK_FILE: &str = ".exclusive-runtime.lock";
pub const AUTHORITY_JOURNAL_ID_V2: &str = "placement-authority";
/// Runtime admission must leave this much of the signed total-state quota
/// unused so the two bounded authority frames required by GC stay reachable.
pub const CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2: u64 = 16 * 1024;

/// A durable-store mutation reached a state whose crash durability cannot be
/// proved from the still-open file descriptors and exact canonical bytes.
/// Callers must stop mutating this store and reopen it so startup validation
/// can classify a complete frame, repair an incomplete tail, or fail closed.
#[derive(Debug, Error)]
#[error("hosted V2 durable store must be reopened: {detail}")]
pub struct DurableStoreReopenRequiredV2 {
    detail: String,
}

impl DurableStoreReopenRequiredV2 {
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalAppendFaultPointV2 {
    AfterWrite,
    AfterFileSync,
    BeforeReconcileFileSync,
    AfterParentSync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionInstallFaultPointV2 {
    BeforePublish,
    AfterRename,
    BeforePublishedReconcile,
    AfterSessionsParentSync,
    AfterStagingParentSync,
    BeforeUnpublishedCleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClosedSessionGcFaultPointV2 {
    JournalRenamePublished,
    DuplicateSourceUnlinked,
    SessionDirectoryUnlinked,
}

fn no_closed_session_gc_fault(_: ClosedSessionGcFaultPointV2, _: &Path) -> Result<()> {
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImmutableBlobKindV2 {
    Operation,
    Checkpoint,
}

impl ImmutableBlobKindV2 {
    fn label(self) -> &'static str {
        match self {
            Self::Operation => "operation",
            Self::Checkpoint => "checkpoint",
        }
    }

    fn staging_prefix(self) -> &'static str {
        match self {
            Self::Operation => "blob-operation-",
            Self::Checkpoint => "blob-checkpoint-",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct JournalHeadV2 {
    next_sequence: u64,
    head_sha256: Option<String>,
    bytes: u64,
}

#[derive(Debug)]
struct CachedJournalV2 {
    path: PathBuf,
    head: Mutex<JournalHeadV2>,
}

#[derive(Debug, Default)]
struct JournalCacheV2 {
    journals: Mutex<BTreeMap<String, Arc<CachedJournalV2>>>,
    validated_scans: AtomicU64,
}

#[derive(Debug)]
struct JournalScanV2 {
    entries: Vec<SignedJournalEntryV2>,
    head: JournalHeadV2,
    corruption: Option<String>,
    torn_tail: Option<TornJournalTailV2>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TornJournalTailV2 {
    old_bytes: u64,
    new_bytes: u64,
    recovered_head_sha256: Option<String>,
}

#[derive(Debug)]
pub struct JournalReadV2 {
    pub entries: Vec<SignedJournalEntryV2>,
    pub corruption: Option<String>,
}

#[derive(Clone)]
struct PendingClosedSessionGcV2 {
    authorization: SignedJournalEntryV2,
    session_id: String,
    terminal_journal_head_sha256: String,
    expected_reclaimed_bytes: u64,
    retained_journal_sha256: String,
    retained_journal_bytes: u64,
}

/// Capability-first durable state.  Source and result records are protected by
/// filesystem ownership (0700 directories, 0600 files); they are deliberately
/// not claimed to be encrypted at rest.
#[derive(Clone)]
pub struct DurableSessionStoreV2 {
    root: PathBuf,
    sessions: PathBuf,
    session_staging: PathBuf,
    gc_tombstones: PathBuf,
    signer: HostedNodeSignerV2,
    _lock: Arc<StateRootLockV2>,
    journals: Arc<JournalCacheV2>,
    pending_blob_publications: Arc<Mutex<BTreeMap<PathBuf, PathBuf>>>,
    reopen_required: Arc<AtomicBool>,
    authority_control_bytes: Arc<AtomicU64>,
    #[cfg(any(test, debug_assertions))]
    injected_append_failure_countdown: Arc<AtomicU64>,
}

struct StateRootLockV2 {
    // The open file description owns the advisory lock for the entire store
    // lifetime.  The marker itself is deliberately persistent: unlinking a
    // live lock file creates a second inode that another process can lock.
    _file: File,
}

impl std::fmt::Debug for DurableSessionStoreV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableSessionStoreV2")
            .field("root", &self.root)
            .field("signer", &self.signer)
            .finish_non_exhaustive()
    }
}

impl DurableSessionStoreV2 {
    pub fn open(root: impl Into<PathBuf>, signer: HostedNodeSignerV2) -> Result<Self> {
        let root = root.into();
        ensure_private_directory_v2(&root)?;
        let lock_path = root.join(STATE_LOCK_FILE);
        let mut lock_options = OpenOptions::new();
        lock_options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            lock_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut lock_file = lock_options.open(&lock_path).with_context(|| {
            format!(
                "failed to open hosted V2 state-root lock for `{}`",
                root.display()
            )
        })?;
        if !lock_file.metadata()?.is_file() {
            bail!(
                "hosted V2 state-root lock `{}` is not a regular file",
                lock_path.display()
            );
        }
        lock_file.try_lock_exclusive().with_context(|| {
            format!(
                "hosted V2 state root `{}` is already locked by a node or admin process",
                root.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            lock_file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        lock_file.set_len(0)?;
        writeln!(lock_file, "pid={}", std::process::id())?;
        lock_file.sync_all()?;
        sync_directory(&root)?;
        let lock = Arc::new(StateRootLockV2 { _file: lock_file });
        let sessions = root.join(SESSIONS_DIRECTORY);
        ensure_private_directory_v2(&sessions)?;
        let session_staging = root.join(SESSION_STAGING_DIRECTORY);
        ensure_private_directory_v2(&session_staging)?;
        reconcile_unpublished_session_staging(&session_staging)?;
        let gc_tombstones = root.join(GC_TOMBSTONES_DIRECTORY);
        ensure_private_directory_v2(&gc_tombstones)?;
        let authority_journal = root.join(AUTHORITY_JOURNAL_FILE);
        if authority_journal.exists() {
            require_regular_file(&authority_journal)?;
        } else {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            options
                .open(&authority_journal)
                .context("failed to create hosted V2 placement-authority journal")?
                .sync_all()?;
            sync_directory(&root)?;
        }
        let store = Self {
            root,
            sessions,
            session_staging,
            gc_tombstones,
            signer,
            _lock: lock,
            journals: Arc::new(JournalCacheV2::default()),
            pending_blob_publications: Arc::new(Mutex::new(BTreeMap::new())),
            reopen_required: Arc::new(AtomicBool::new(false)),
            authority_control_bytes: Arc::new(AtomicU64::new(0)),
            #[cfg(any(test, debug_assertions))]
            injected_append_failure_countdown: Arc::new(AtomicU64::new(0)),
        };
        store.initialize_journals()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn signer(&self) -> &HostedNodeSignerV2 {
        &self.signer
    }

    pub fn is_reopen_required(&self) -> bool {
        self.reopen_required.load(Ordering::Acquire)
    }

    /// Authority-control headroom still available after all validated durable
    /// placement-authority frames. Runtime capacity reconstruction must reserve
    /// this remainder rather than adding a fresh 16 KiB on every restart.
    pub fn remaining_authority_control_headroom_bytes(&self) -> u64 {
        CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
            .saturating_sub(self.authority_control_bytes.load(Ordering::Acquire))
    }

    /// Arm one zero-byte journal-append failure after the requested number of
    /// otherwise attempted public appends. This exists only in debug builds so
    /// integration tests can exercise a failed second frame in a multi-record
    /// transition without exposing a production fault-injection surface.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn inject_append_failure_after_successes_for_test(
        &self,
        successful_appends_before_failure: u64,
    ) -> Result<()> {
        let countdown = successful_appends_before_failure
            .checked_add(1)
            .context("hosted V2 append-fault countdown overflow")?;
        if self
            .injected_append_failure_countdown
            .compare_exchange(0, countdown, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            bail!("a hosted V2 append fault is already armed");
        }
        Ok(())
    }

    /// Atomically poison this store without touching the filesystem. Debug
    /// integration tests use this to close view/mutation race windows around
    /// runtime lock acquisition.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn inject_reopen_required_for_test(&self) {
        self.reopen_required.store(true, Ordering::Release);
    }

    fn require_mutations_available(&self) -> Result<()> {
        if self.is_reopen_required() {
            return Err(DurableStoreReopenRequiredV2 {
                detail: "a previous durability ambiguity poisoned this store instance".to_owned(),
            }
            .into());
        }
        Ok(())
    }

    fn poison_reopen_required(&self, detail: impl Into<String>) -> anyhow::Error {
        self.reopen_required.store(true, Ordering::Release);
        DurableStoreReopenRequiredV2 {
            detail: detail.into(),
        }
        .into()
    }

    /// Atomically publish a new session together with its first signed journal
    /// entry. No final session directory is visible until all private
    /// subdirectories and the fsynced first frame exist in staging.
    pub fn install_session(
        &self,
        session_id: &str,
        first_entry: &SignedJournalEntryV2,
    ) -> Result<u64> {
        self.require_mutations_available()?;
        self.install_session_inner(session_id, first_entry, |_, _| Ok(()))
    }

    fn install_session_inner<F>(
        &self,
        session_id: &str,
        first_entry: &SignedJournalEntryV2,
        mut fault: F,
    ) -> Result<u64>
    where
        F: FnMut(SessionInstallFaultPointV2, &Path) -> Result<()>,
    {
        self.require_mutations_available()?;
        validate_sha256_v2("session_id", session_id)?;
        first_entry.verify()?;
        let authority = self.read_authority_journal()?;
        if let Some(corruption) = authority.corruption {
            bail!("refusing session installation with corrupt placement authority: {corruption}");
        }
        if authority
            .entries
            .iter()
            .any(|entry| entry.entry.event.retired_session_id() == Some(session_id))
        {
            bail!("refusing to resurrect retired hosted V2 session `{session_id}`");
        }
        if first_entry.signer_public_key != self.signer.public_key_hex()
            || first_entry.entry.session_id != session_id
            || first_entry.entry.sequence != 1
            || first_entry.entry.previous_entry_sha256.is_some()
            || !matches!(
                first_entry.entry.event,
                JournalEventV2::SessionOpened { .. }
            )
        {
            bail!("atomic session install requires this node's exact first SessionOpened receipt");
        }
        let final_directory = self.session_directory(session_id);
        match fs::symlink_metadata(&final_directory) {
            Ok(_) => {
                if self.journal_is_registered(session_id)? {
                    bail!("refusing to replace existing hosted V2 session `{session_id}`");
                }
                return self
                    .finish_published_session_install(
                        session_id,
                        first_entry,
                        &final_directory,
                        &mut fault,
                    )
                    .map_err(|error| {
                        self.poison_reopen_required(format!(
                            "cannot resume exact published session `{session_id}`: {error:#}"
                        ))
                    });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).context("failed to create hosted session staging identity")?;
        let staged_directory = self
            .session_staging
            .join(format!("install-{session_id}-{}", hex::encode(random)));
        create_private_directory_new(&staged_directory)?;

        let outcome = (|| -> Result<u64> {
            create_private_directory_new(&staged_directory.join(OPERATIONS_DIRECTORY))
                .context("failed to create staged hosted V2 operations directory")?;
            create_private_directory_new(&staged_directory.join(CHECKPOINTS_DIRECTORY))
                .context("failed to create staged hosted V2 checkpoints directory")?;
            let journal = staged_directory.join(JOURNAL_FILE);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            let file = options.open(&journal).with_context(|| {
                format!(
                    "failed to create staged session journal `{}`",
                    journal.display()
                )
            })?;
            let mut writer = BufWriter::new(file);
            write_hosted_frame(&mut writer, first_entry)?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            let written = fs::metadata(&journal)?.len();
            sync_directory(&staged_directory.join(OPERATIONS_DIRECTORY))?;
            sync_directory(&staged_directory.join(CHECKPOINTS_DIRECTORY))?;
            sync_directory(&staged_directory)?;
            sync_directory(&self.session_staging)?;
            fault(SessionInstallFaultPointV2::BeforePublish, &staged_directory)?;
            match fs::symlink_metadata(&final_directory) {
                Ok(_) => bail!("refusing to replace existing hosted V2 session `{session_id}`"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            fs::rename(&staged_directory, &final_directory).with_context(|| {
                format!("failed to publish hosted V2 session `{session_id}` atomically")
            })?;
            if let Err(error) = fault(SessionInstallFaultPointV2::AfterRename, &final_directory) {
                // The rename already made the complete staged tree visible.
                // Revalidate and re-drive both parent barriers rather than
                // reporting an ordinary failure that an exact retry cannot
                // distinguish from a conflicting pre-existing session.
                self.finish_published_session_install(
                    session_id,
                    first_entry,
                    &final_directory,
                    &mut fault,
                )
                .map_err(|reconcile| {
                    self.poison_reopen_required(format!(
                        "session `{session_id}` publication became ambiguous after rename ({error:#}); reconciliation failed: {reconcile:#}"
                    ))
                })?;
                return Ok(written);
            }
            self.finish_published_session_install(
                session_id,
                first_entry,
                &final_directory,
                &mut fault,
            )
            .map_err(|error| {
                self.poison_reopen_required(format!(
                    "cannot prove published session `{session_id}` durable: {error:#}"
                ))
            })?;
            Ok(written)
        })();

        if outcome.is_err() {
            let cleanup = (|| -> Result<()> {
                match fs::symlink_metadata(&staged_directory) {
                    Ok(_) => {
                        fault(
                            SessionInstallFaultPointV2::BeforeUnpublishedCleanup,
                            &staged_directory,
                        )?;
                        fs::remove_dir_all(&staged_directory).with_context(|| {
                            format!(
                                "failed to remove unpublished hosted session staging directory `{}`",
                                staged_directory.display()
                            )
                        })?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error)
                            .context("cannot inspect unpublished session staging path")
                    }
                }
                prove_path_absent(&staged_directory)
                    .context("unpublished session staging directory remains after cleanup")?;
                sync_directory(&self.session_staging)
            })();
            if let Err(error) = cleanup {
                return Err(self.poison_reopen_required(format!(
                    "cannot prove unpublished session staging cleanup: {error:#}"
                )));
            }
        }
        outcome
    }

    fn journal_is_registered(&self, journal_id: &str) -> Result<bool> {
        Ok(
            lock_mutex(&self.journals.journals, "hosted V2 journal cache")?
                .contains_key(journal_id),
        )
    }

    fn finish_published_session_install<F>(
        &self,
        session_id: &str,
        first_entry: &SignedJournalEntryV2,
        final_directory: &Path,
        fault: &mut F,
    ) -> Result<u64>
    where
        F: FnMut(SessionInstallFaultPointV2, &Path) -> Result<()>,
    {
        fault(
            SessionInstallFaultPointV2::BeforePublishedReconcile,
            final_directory,
        )?;
        let head = validate_exact_published_session(
            session_id,
            final_directory,
            first_entry,
            &self.signer,
        )?;
        let journal_path = final_directory.join(JOURNAL_FILE);
        let mut journal_options = OpenOptions::new();
        journal_options.read(true);
        #[cfg(unix)]
        journal_options.custom_flags(libc::O_NOFOLLOW);
        journal_options
            .open(&journal_path)?
            .sync_all()
            .context("failed to re-sync published session journal")?;
        sync_directory(&final_directory.join(OPERATIONS_DIRECTORY))?;
        sync_directory(&final_directory.join(CHECKPOINTS_DIRECTORY))?;
        sync_directory(final_directory)?;

        if let Err(first) = sync_directory(&self.sessions) {
            sync_directory(&self.sessions)
                .with_context(|| format!("failed to retry sessions-parent sync after: {first}"))?;
        }
        if fault(
            SessionInstallFaultPointV2::AfterSessionsParentSync,
            &self.sessions,
        )
        .is_err()
        {
            sync_directory(&self.sessions)
                .context("failed to retry published-session parent sync")?;
        }
        if let Err(first) = sync_directory(&self.session_staging) {
            sync_directory(&self.session_staging).with_context(|| {
                format!("failed to retry session-staging parent sync after: {first}")
            })?;
        }
        if fault(
            SessionInstallFaultPointV2::AfterStagingParentSync,
            &self.session_staging,
        )
        .is_err()
        {
            sync_directory(&self.session_staging)
                .context("failed to retry session-staging parent sync")?;
        }
        self.register_journal(session_id, &journal_path, head.clone())?;
        Ok(head.bytes)
    }

    pub fn list_session_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.sessions)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if entry.file_type()?.is_symlink() || !metadata.is_dir() {
                bail!(
                    "hosted V2 sessions directory contains non-directory `{}`",
                    entry.path().display()
                );
            }
            let id = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("hosted V2 session name is not UTF-8"))?;
            validate_sha256_v2("session_id", &id)?;
            ids.push(id);
        }
        ids.sort();
        Ok(ids)
    }

    pub fn read_journal(&self, session_id: &str) -> Result<JournalReadV2> {
        validate_sha256_v2("session_id", session_id)?;
        let path = self.journal_path(session_id);
        self.read_journal_file(session_id, &path)
    }

    pub fn read_authority_journal(&self) -> Result<JournalReadV2> {
        self.read_journal_file(AUTHORITY_JOURNAL_ID_V2, &self.authority_journal_path())
    }

    /// Load the permanent signed session journal retained by one GC
    /// authorization. Runtime replay derives the retired session identity and
    /// every consumed placement nonce from these already-validated entries.
    pub fn read_closed_session_gc_archive(
        &self,
        authorization: &JournalEventV2,
    ) -> Result<JournalReadV2> {
        let JournalEventV2::ClosedSessionGcAuthorized {
            session_id,
            terminal_journal_head_sha256,
            retained_journal_sha256,
            retained_journal_bytes,
            ..
        } = authorization
        else {
            bail!("closed-session GC archives require a GC authorization event");
        };
        let scan = validate_retained_gc_journal(
            session_id,
            &self.gc_archive_path(session_id)?,
            &self.signer,
            terminal_journal_head_sha256,
            retained_journal_sha256,
            *retained_journal_bytes,
        )?;
        Ok(JournalReadV2 {
            entries: scan.entries,
            corruption: None,
        })
    }

    /// Validate every journal exactly once while this process exclusively owns
    /// the state root. Only an incomplete final frame is repaired; a complete
    /// invalid frame, signature, or hash-chain link aborts startup unchanged.
    fn initialize_journals(&self) -> Result<()> {
        let authority_entries =
            self.initialize_journal(AUTHORITY_JOURNAL_ID_V2, &self.authority_journal_path())?;
        let pending_gc_session = pending_gc_session_id(&authority_entries)?;
        let retired_sessions = self.initialize_gc_archives(&authority_entries)?;
        for session_id in self.list_session_ids()? {
            if retired_sessions.contains(&session_id)
                && pending_gc_session.as_deref() != Some(&session_id)
            {
                bail!("retired hosted V2 session `{session_id}` was resurrected on disk");
            }
            let path = self.journal_path(&session_id);
            if pending_gc_session.as_deref() == Some(&session_id)
                && matches!(fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
            {
                // A signed GC authorization permits restart to finish removing
                // the exact session even if an interrupted recursive delete
                // already removed its journal.
                continue;
            }
            self.initialize_journal(&session_id, &path)?;
        }
        Ok(())
    }

    fn initialize_gc_archives(
        &self,
        authority: &[SignedJournalEntryV2],
    ) -> Result<BTreeSet<String>> {
        let mut authorizations = BTreeMap::<String, &JournalEventV2>::new();
        let mut completed = BTreeSet::new();
        for entry in authority {
            match &entry.entry.event {
                event @ JournalEventV2::ClosedSessionGcAuthorized { session_id, .. } => {
                    if authorizations.insert(session_id.clone(), event).is_some() {
                        bail!("placement-authority journal retires one session more than once");
                    }
                }
                JournalEventV2::ClosedSessionGcCompleted { session_id, .. } => {
                    completed.insert(session_id.clone());
                }
                _ => {}
            }
        }
        let mut archived = BTreeSet::new();
        for entry in fs::read_dir(&self.gc_tombstones)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("GC archive filename is not UTF-8"))?;
            let Some(session_id) = name.strip_suffix(".cborseq") else {
                bail!("GC archive directory contains unknown entry `{name}`");
            };
            validate_sha256_v2("GC archive filename", session_id)?;
            require_regular_file(&path)?;
            let authorization = authorizations.get(session_id).with_context(|| {
                format!("GC archive `{session_id}` has no signed authorization")
            })?;
            self.read_closed_session_gc_archive(authorization)?;
            archived.insert(session_id.to_owned());
        }
        for (session_id, authorization) in &authorizations {
            if !archived.contains(session_id) {
                if completed.contains(session_id) || !self.journal_path(session_id).is_file() {
                    bail!("signed GC authorization for `{session_id}` has no retained journal");
                }
                let JournalEventV2::ClosedSessionGcAuthorized {
                    terminal_journal_head_sha256,
                    retained_journal_sha256,
                    retained_journal_bytes,
                    ..
                } = authorization
                else {
                    unreachable!("authorization map contains only GC authorizations")
                };
                validate_retained_gc_journal(
                    session_id,
                    &self.journal_path(session_id),
                    &self.signer,
                    terminal_journal_head_sha256,
                    retained_journal_sha256,
                    *retained_journal_bytes,
                )?;
            }
        }
        Ok(authorizations.into_keys().collect())
    }

    fn initialize_journal(
        &self,
        journal_id: &str,
        path: &Path,
    ) -> Result<Vec<SignedJournalEntryV2>> {
        let JournalScanV2 {
            entries,
            mut head,
            corruption,
            torn_tail,
        } = self.scan_journal_file(journal_id, path)?;
        if let Some(corruption) = corruption {
            bail!("refusing to open corrupt hosted V2 journal `{journal_id}`: {corruption}");
        }
        if journal_id != AUTHORITY_JOURNAL_ID_V2 && entries.is_empty() {
            bail!(
                "refusing hosted V2 session journal `{journal_id}` without a validated first frame"
            );
        }
        let repair = torn_tail;
        if let Some(torn) = &repair {
            self.truncate_torn_journal_tail(path, torn)?;
            head.bytes = torn.new_bytes;
        }
        if journal_id == AUTHORITY_JOURNAL_ID_V2 {
            self.restore_authority_control_headroom(authority_control_debit(&entries)?)?;
        }
        self.register_journal(journal_id, path, head)?;
        if let Some(torn) = repair {
            self.record_journal_tail_repair(journal_id, &torn)?;
        }
        Ok(entries)
    }

    fn scan_journal_file(&self, journal_id: &str, path: &Path) -> Result<JournalScanV2> {
        self.journals
            .validated_scans
            .fetch_add(1, Ordering::Relaxed);
        scan_validated_journal(journal_id, path, &self.signer)
    }

    fn register_journal(&self, journal_id: &str, path: &Path, head: JournalHeadV2) -> Result<()> {
        let mut journals = lock_mutex(&self.journals.journals, "hosted V2 journal cache")?;
        if journals.contains_key(journal_id) {
            bail!("hosted V2 journal `{journal_id}` is already registered");
        }
        journals.insert(
            journal_id.to_owned(),
            Arc::new(CachedJournalV2 {
                path: path.to_owned(),
                head: Mutex::new(head),
            }),
        );
        Ok(())
    }

    fn cached_journal(&self, journal_id: &str, path: &Path) -> Result<Arc<CachedJournalV2>> {
        let journals = lock_mutex(&self.journals.journals, "hosted V2 journal cache")?;
        let cached = journals
            .get(journal_id)
            .cloned()
            .with_context(|| format!("hosted V2 journal `{journal_id}` is not registered"))?;
        if cached.path != path {
            bail!("hosted V2 journal cache path mismatch for `{journal_id}`");
        }
        Ok(cached)
    }

    fn unregister_journal(&self, journal_id: &str, path: &Path) -> Result<()> {
        let mut journals = lock_mutex(&self.journals.journals, "hosted V2 journal cache")?;
        if let Some(cached) = journals.remove(journal_id) {
            if cached.path != path {
                bail!("hosted V2 journal cache path mismatch for `{journal_id}`");
            }
        }
        Ok(())
    }

    fn truncate_torn_journal_tail(&self, path: &Path, torn: &TornJournalTailV2) -> Result<()> {
        require_regular_file(path)?;
        let mut options = OpenOptions::new();
        options.write(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path).with_context(|| {
            format!("failed to open torn hosted V2 journal `{}`", path.display())
        })?;
        if file.metadata()?.len() != torn.old_bytes || torn.new_bytes >= torn.old_bytes {
            bail!("hosted V2 journal changed while its torn tail was being repaired");
        }
        file.set_len(torn.new_bytes)?;
        file.sync_all()?;
        sync_directory(path.parent().context("journal path has no parent")?)
    }

    fn record_journal_tail_repair(
        &self,
        journal_id: &str,
        torn: &TornJournalTailV2,
    ) -> Result<SignedJournalEntryV2> {
        if journal_id != AUTHORITY_JOURNAL_ID_V2 {
            validate_sha256_v2("repaired journal_id", journal_id)?;
        }
        let authority_path = self.authority_journal_path();
        let cached = self.cached_journal(AUTHORITY_JOURNAL_ID_V2, &authority_path)?;
        let mut head = lock_mutex(&cached.head, "hosted V2 authority-journal head")?;
        let receipt = self.signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: AUTHORITY_JOURNAL_ID_V2.to_owned(),
            sequence: head.next_sequence,
            previous_entry_sha256: head.head_sha256.clone(),
            recorded_unix_ms: unix_time_ms()?,
            event: JournalEventV2::JournalTailRepaired {
                journal_id: journal_id.to_owned(),
                old_bytes: torn.old_bytes,
                new_bytes: torn.new_bytes,
                recovered_head_sha256: torn.recovered_head_sha256.clone(),
            },
        })?;
        let frame_bytes = canonical_hosted_frame(&receipt)?.len() as u64;
        let previous = self
            .begin_authority_control_transition(&receipt.entry.event, frame_bytes)?
            .context("journal-tail repair must consume emergency authority headroom")?;
        if let Err(error) = self.append_journal_file_locked(&authority_path, &receipt, &mut head) {
            if !self.is_reopen_required() {
                self.rollback_authority_control_transition(previous);
            }
            return Err(error);
        }
        Ok(receipt)
    }

    fn restore_authority_control_headroom(&self, debit_bytes: u64) -> Result<()> {
        if debit_bytes > CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 {
            bail!(
                "durable placement-authority journal requires {debit_bytes} bytes of outstanding emergency headroom, exceeding its fixed {}-byte budget",
                CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
            );
        }
        self.authority_control_bytes
            .store(debit_bytes, Ordering::Release);
        Ok(())
    }

    fn begin_authority_control_transition(
        &self,
        event: &JournalEventV2,
        frame_bytes: u64,
    ) -> Result<Option<u64>> {
        if !is_emergency_authority_control_event(event) {
            return Ok(None);
        }
        self.authority_control_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |consumed| {
                authority_control_debit_after_event(consumed, event, frame_bytes)
                    .filter(|next| *next <= CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2)
            })
            .map(Some)
            .map_err(|consumed| {
                anyhow::anyhow!(
                    "signed authority control transition cannot fit its {frame_bytes}-byte frame in the fixed {}-byte durable headroom (current debit {consumed})",
                    CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2,
                )
            })
    }

    fn rollback_authority_control_transition(&self, previous: u64) {
        self.authority_control_bytes
            .store(previous, Ordering::Release);
    }

    #[cfg(test)]
    fn validated_journal_scan_count(&self) -> u64 {
        self.journals.validated_scans.load(Ordering::Relaxed)
    }

    fn read_journal_file(&self, journal_id: &str, path: &Path) -> Result<JournalReadV2> {
        let cached = self.cached_journal(journal_id, path)?;
        let head = lock_mutex(&cached.head, "hosted V2 journal head")?;
        let JournalScanV2 {
            entries,
            head: scanned_head,
            mut corruption,
            torn_tail,
        } = self.scan_journal_file(journal_id, path)?;
        if let Some(torn) = torn_tail {
            corruption = Some(format!(
                "incomplete final hosted V2 journal frame at byte {} (file has {} bytes)",
                torn.new_bytes, torn.old_bytes
            ));
        } else if corruption.is_none() && scanned_head != *head {
            corruption = Some(
                "hosted V2 journal changed outside its exclusive durable-store owner".to_owned(),
            );
        }
        Ok(JournalReadV2 {
            entries,
            corruption,
        })
    }

    pub fn append_entry(&self, session_id: &str, entry: &SignedJournalEntryV2) -> Result<u64> {
        validate_sha256_v2("session_id", session_id)?;
        self.append_journal_file(session_id, &self.journal_path(session_id), entry)
    }

    pub fn append_authority_entry(&self, entry: &SignedJournalEntryV2) -> Result<u64> {
        self.append_journal_file(
            AUTHORITY_JOURNAL_ID_V2,
            &self.authority_journal_path(),
            entry,
        )
    }

    fn append_journal_file(
        &self,
        journal_id: &str,
        path: &Path,
        entry: &SignedJournalEntryV2,
    ) -> Result<u64> {
        self.require_mutations_available()?;
        entry.verify()?;
        if entry.entry.session_id != journal_id {
            bail!("refusing to append a journal entry to a different session");
        }
        if entry.signer_public_key != self.signer.public_key_hex() {
            bail!("refusing to append a journal entry signed by another node key");
        }
        validate_journal_event_location(journal_id, &entry.entry.event)?;
        self.validate_gc_completion_credit(&entry.entry.event)?;
        #[cfg(any(test, debug_assertions))]
        if let Ok(previous) = self.injected_append_failure_countdown.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |remaining| remaining.checked_sub(1),
        ) {
            if previous == 1 {
                bail!("injected zero-byte hosted V2 journal append failure");
            }
        }
        let cached = self.cached_journal(journal_id, path)?;
        let mut head = lock_mutex(&cached.head, "hosted V2 journal head")?;
        let authority_transition = if journal_id == AUTHORITY_JOURNAL_ID_V2 {
            let frame_bytes = canonical_hosted_frame(entry)?.len() as u64;
            self.begin_authority_control_transition(&entry.entry.event, frame_bytes)?
        } else {
            None
        };
        let result = self.append_journal_file_locked(path, entry, &mut head);
        if result.is_err() && !self.is_reopen_required() {
            if let Some(previous) = authority_transition {
                self.rollback_authority_control_transition(previous);
            }
        }
        result
    }

    fn validate_gc_completion_credit(&self, event: &JournalEventV2) -> Result<()> {
        let JournalEventV2::ClosedSessionGcCompleted {
            session_id,
            terminal_journal_head_sha256,
            reclaimed_bytes,
        } = event
        else {
            return Ok(());
        };
        let pending = self
            .pending_closed_session_gc()?
            .context("GC completion has no preceding authorization")?;
        if pending.session_id != *session_id
            || pending.terminal_journal_head_sha256 != *terminal_journal_head_sha256
            || pending.expected_reclaimed_bytes != *reclaimed_bytes
        {
            bail!("GC completion does not match its authorization");
        }
        Ok(())
    }

    fn append_journal_file_locked(
        &self,
        path: &Path,
        entry: &SignedJournalEntryV2,
        head: &mut JournalHeadV2,
    ) -> Result<u64> {
        self.append_journal_file_locked_inner(path, entry, head, |_, _| Ok(()))
    }

    fn append_journal_file_locked_inner<F>(
        &self,
        path: &Path,
        entry: &SignedJournalEntryV2,
        head: &mut JournalHeadV2,
        mut fault: F,
    ) -> Result<u64>
    where
        F: FnMut(JournalAppendFaultPointV2, &Path) -> Result<()>,
    {
        if entry.entry.sequence != head.next_sequence
            || entry.entry.previous_entry_sha256 != head.head_sha256
        {
            bail!("refusing non-contiguous hosted V2 journal append");
        }
        require_regular_file(path)?;
        let frame = canonical_hosted_frame(entry)?;
        let next_sequence = head
            .next_sequence
            .checked_add(1)
            .context("hosted V2 journal sequence overflow")?;
        let mut options = OpenOptions::new();
        options.append(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            bail!("hosted V2 journal append target is not a regular file");
        }
        let before = metadata.len();
        if before != head.bytes {
            bail!(
                "hosted V2 journal byte length differs from its validated cached head; restart is required"
            );
        }
        let expected_after = before
            .checked_add(frame.len() as u64)
            .context("hosted V2 journal byte length overflow")?;
        if let Err(error) = file.write_all(&frame) {
            return self.reconcile_appended_frame(
                path,
                entry,
                head,
                &file,
                before,
                expected_after,
                &frame,
                anyhow::Error::from(error),
                &mut fault,
            );
        }
        if let Err(error) = fault(JournalAppendFaultPointV2::AfterWrite, path) {
            return self.reconcile_appended_frame(
                path,
                entry,
                head,
                &file,
                before,
                expected_after,
                &frame,
                error,
                &mut fault,
            );
        }
        if let Err(error) = file.sync_all() {
            return self.reconcile_appended_frame(
                path,
                entry,
                head,
                &file,
                before,
                expected_after,
                &frame,
                anyhow::Error::from(error),
                &mut fault,
            );
        }
        if let Err(error) = fault(JournalAppendFaultPointV2::AfterFileSync, path) {
            return self.reconcile_appended_frame(
                path,
                entry,
                head,
                &file,
                before,
                expected_after,
                &frame,
                error,
                &mut fault,
            );
        }
        verify_exact_appended_frame(path, before, expected_after, &frame).map_err(|error| {
            self.poison_reopen_required(format!(
                "file-synced journal append `{}` cannot be revalidated: {error:#}",
                path.display()
            ))
        })?;

        // The journal's directory entry already existed and was not changed
        // by this append. Once the exact new tail and file fsync are proven, a
        // directory-fsync failure cannot make that append ambiguous.
        let parent = path.parent().context("journal path has no parent")?;
        let _ = sync_directory(parent);
        let _ = fault(JournalAppendFaultPointV2::AfterParentSync, parent);
        head.next_sequence = next_sequence;
        head.head_sha256 = Some(entry.entry_sha256.clone());
        head.bytes = expected_after;
        Ok(frame.len() as u64)
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_appended_frame<F>(
        &self,
        path: &Path,
        entry: &SignedJournalEntryV2,
        head: &mut JournalHeadV2,
        file: &File,
        before: u64,
        expected_after: u64,
        frame: &[u8],
        cause: anyhow::Error,
        fault: &mut F,
    ) -> Result<u64>
    where
        F: FnMut(JournalAppendFaultPointV2, &Path) -> Result<()>,
    {
        let observed = fs::metadata(path)
            .map(|metadata| metadata.len())
            .map_err(|error| {
                self.poison_reopen_required(format!(
                    "cannot inspect journal after append error ({cause:#}): {error}"
                ))
            })?;
        if observed == before {
            return Err(cause).context("journal append failed before changing durable bytes");
        }
        verify_exact_appended_frame(path, before, expected_after, frame).map_err(|error| {
            self.poison_reopen_required(format!(
                "journal append failed ({cause:#}) and its tail is not the exact canonical frame: {error:#}"
            ))
        })?;
        fault(JournalAppendFaultPointV2::BeforeReconcileFileSync, path).map_err(|error| {
            self.poison_reopen_required(format!(
                "journal tail is exact after append error ({cause:#}) but reconciliation was interrupted: {error:#}"
            ))
        })?;
        file.sync_all().map_err(|error| {
            self.poison_reopen_required(format!(
                "journal tail is exact after append error ({cause:#}) but file sync cannot prove durability: {error}"
            ))
        })?;
        verify_exact_appended_frame(path, before, expected_after, frame).map_err(|error| {
            self.poison_reopen_required(format!(
                "journal changed while reconciling an exact appended frame: {error:#}"
            ))
        })?;
        let parent = path.parent().context("journal path has no parent")?;
        let _ = sync_directory(parent);
        head.next_sequence = head
            .next_sequence
            .checked_add(1)
            .context("hosted V2 journal sequence overflow")?;
        head.head_sha256 = Some(entry.entry_sha256.clone());
        head.bytes = expected_after;
        Ok(frame.len() as u64)
    }

    pub fn write_operation(
        &self,
        session_id: &str,
        operation: &PreparedOperationV2,
    ) -> Result<u64> {
        validate_sha256_v2("session_id", session_id)?;
        operation.validate()?;
        let path = self.operation_path(session_id, &operation.operation_id)?;
        let frame = canonical_hosted_frame(operation)?;
        self.install_immutable_blob(session_id, ImmutableBlobKindV2::Operation, &path, &frame)
    }

    /// Return the exact durable-byte delta that `write_operation` will add.
    /// This makes a retry after blob publication but before OperationAccepted
    /// quota-neutral while preserving fail-closed content-address conflicts.
    pub fn operation_new_bytes(
        &self,
        session_id: &str,
        operation: &PreparedOperationV2,
    ) -> Result<u64> {
        validate_sha256_v2("session_id", session_id)?;
        operation.validate()?;
        let path = self.operation_path(session_id, &operation.operation_id)?;
        let frame = canonical_hosted_frame(operation)?;
        if exact_existing_blob(&path, &frame, ImmutableBlobKindV2::Operation)? {
            Ok(0)
        } else {
            Ok(frame.len() as u64)
        }
    }

    pub fn read_operation(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<PreparedOperationV2> {
        let path = self.operation_path(session_id, operation_id)?;
        require_regular_file(&path)?;
        let mut reader = BufReader::new(File::open(&path)?);
        let operation = read_hosted_frame::<_, PreparedOperationV2>(&mut reader)?
            .context("durable operation file is empty")?;
        if read_hosted_frame::<_, PreparedOperationV2>(&mut reader)?.is_some() {
            bail!("durable operation file contains more than one frame");
        }
        operation.validate()?;
        if operation.operation_id != operation_id {
            bail!("durable operation identity does not match its filename");
        }
        Ok(operation)
    }

    /// Persist one immutable, actor-generation-addressed evaluator snapshot.
    /// Checkpoints are never overwritten or evicted automatically.
    pub fn checkpoint_new_bytes(
        &self,
        session_id: &str,
        actor_generation_sha256: &str,
        snapshot: &EvaluatorStateSnapshotV1,
        max_snapshot_payload_bytes: u64,
    ) -> Result<u64> {
        validate_sha256_v2("session_id", session_id)?;
        validate_sha256_v2("actor_generation_sha256", actor_generation_sha256)?;
        snapshot.validate()?;
        let path = self.checkpoint_path(session_id, actor_generation_sha256)?;
        let frame = canonical_checkpoint_frame(snapshot, max_snapshot_payload_bytes)?;
        if exact_existing_blob(&path, &frame, ImmutableBlobKindV2::Checkpoint)? {
            Ok(0)
        } else {
            Ok(frame.len() as u64)
        }
    }

    /// Persist one immutable, actor-generation-addressed evaluator snapshot.
    /// Existing content-addressed bytes are accepted only when the complete
    /// framed bytes exactly equal this canonical snapshot, and then contribute
    /// zero new bytes.
    pub fn write_checkpoint(
        &self,
        session_id: &str,
        actor_generation_sha256: &str,
        snapshot: &EvaluatorStateSnapshotV1,
        max_snapshot_payload_bytes: u64,
    ) -> Result<u64> {
        validate_sha256_v2("session_id", session_id)?;
        validate_sha256_v2("actor_generation_sha256", actor_generation_sha256)?;
        snapshot.validate()?;
        let path = self.checkpoint_path(session_id, actor_generation_sha256)?;
        let frame = canonical_checkpoint_frame(snapshot, max_snapshot_payload_bytes)?;
        self.install_immutable_blob(session_id, ImmutableBlobKindV2::Checkpoint, &path, &frame)
    }

    /// Install one canonical immutable frame without ever streaming bytes
    /// through its public final pathname. The staging file and final hard link
    /// are on the same filesystem, and `hard_link` supplies atomic no-clobber
    /// publication: a concurrent or retried writer can observe only absence or
    /// the complete, already-fsynced frame.
    fn install_immutable_blob(
        &self,
        session_id: &str,
        kind: ImmutableBlobKindV2,
        final_path: &Path,
        frame: &[u8],
    ) -> Result<u64> {
        self.require_mutations_available()?;
        let pending = lock_mutex(
            &self.pending_blob_publications,
            "hosted V2 pending blob publications",
        )?
        .get(final_path)
        .cloned();
        match exact_existing_blob(final_path, frame, kind) {
            Ok(true) => {
                if let Some(staged_path) = pending {
                    return self.finish_published_immutable_blob(
                        kind,
                        final_path,
                        &staged_path,
                        frame,
                    );
                }
                return Ok(0);
            }
            Ok(false) if pending.is_some() => {
                return Err(self.poison_reopen_required(format!(
                    "pending published {} disappeared before reconciliation",
                    kind.label()
                )));
            }
            Ok(false) => {}
            Err(error) if pending.is_some() => {
                return Err(self.poison_reopen_required(format!(
                    "pending published {} cannot be revalidated: {error:#}",
                    kind.label()
                )));
            }
            Err(error) => return Err(error),
        }

        let final_parent = final_path
            .parent()
            .with_context(|| format!("{} path has no parent", kind.label()))?;
        ensure_private_directory_v2(final_parent)?;
        let staged_path = self.stage_immutable_blob(session_id, kind, frame)?;

        match fs::hard_link(&staged_path, final_path) {
            Ok(()) => {
                lock_mutex(
                    &self.pending_blob_publications,
                    "hosted V2 pending blob publications",
                )?
                .insert(final_path.to_owned(), staged_path.clone());
                self.finish_published_immutable_blob(kind, final_path, &staged_path, frame)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let exact = exact_existing_blob(final_path, frame, kind);
                let cleanup = self.remove_staged_blob(&staged_path);
                match (exact, cleanup) {
                    (Ok(true), Ok(())) => Ok(0),
                    (Ok(false), Ok(())) => bail!(
                        "published {} `{}` disappeared during no-clobber validation",
                        kind.label(),
                        final_path.display()
                    ),
                    (Err(error), Ok(())) => Err(error),
                    (Ok(_), Err(cleanup_error)) => Err(self.poison_reopen_required(format!(
                        "cannot prove staged {} cleanup after no-clobber publication: {cleanup_error:#}",
                        kind.label()
                    ))),
                    (Err(error), Err(cleanup_error)) => Err(self.poison_reopen_required(format!(
                        "cannot validate no-clobber {} ({error:#}) or prove staged cleanup: {cleanup_error:#}",
                        kind.label()
                    ))),
                }
            }
            Err(error) => {
                self.remove_staged_blob(&staged_path).map_err(|cleanup_error| {
                    self.poison_reopen_required(format!(
                        "{} publication failed ({error}); staged cleanup cannot be proved: {cleanup_error:#}",
                        kind.label()
                    ))
                })?;
                Err(error).with_context(|| {
                    format!(
                        "failed to publish durable {} `{}` without replacement",
                        kind.label(),
                        final_path.display()
                    )
                })
            }
        }
    }

    fn finish_published_immutable_blob(
        &self,
        kind: ImmutableBlobKindV2,
        final_path: &Path,
        staged_path: &Path,
        frame: &[u8],
    ) -> Result<u64> {
        let final_exact = exact_existing_blob(final_path, frame, kind).map_err(|error| {
            self.poison_reopen_required(format!(
                "published {} cannot be revalidated: {error:#}",
                kind.label()
            ))
        })?;
        if !final_exact {
            return Err(self.poison_reopen_required(format!(
                "published {} `{}` disappeared during reconciliation",
                kind.label(),
                final_path.display()
            )));
        }
        let staged_exists = match fs::symlink_metadata(staged_path) {
            Ok(metadata) => {
                require_private_staging_entry(staged_path, &metadata, false).map_err(|error| {
                    self.poison_reopen_required(format!(
                        "pending staged {} is not a private regular file: {error:#}",
                        kind.label()
                    ))
                })?;
                let staged_exact =
                    exact_existing_blob(staged_path, frame, kind).map_err(|error| {
                        self.poison_reopen_required(format!(
                            "pending staged {} cannot be revalidated: {error:#}",
                            kind.label()
                        ))
                    })?;
                if !staged_exact {
                    return Err(self.poison_reopen_required(format!(
                        "staged {} differs from its exact published bytes",
                        kind.label()
                    )));
                }
                require_same_file_identity(final_path, staged_path).map_err(|error| {
                    self.poison_reopen_required(format!(
                        "published and staged {} paths are not the same immutable inode: {error:#}",
                        kind.label()
                    ))
                })?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(self.poison_reopen_required(format!(
                    "pending staged {} cannot be inspected: {error}",
                    kind.label()
                )));
            }
        };
        let final_parent = final_path
            .parent()
            .with_context(|| format!("{} path has no parent", kind.label()))?;
        if let Err(first) = sync_directory(final_parent) {
            sync_directory(final_parent).map_err(|second| {
                self.poison_reopen_required(format!(
                    "cannot prove published {} durable after parent-sync errors ({first}; {second})",
                    kind.label()
                ))
            })?;
        }
        if staged_exists {
            self.remove_staged_blob(staged_path).map_err(|error| {
                self.poison_reopen_required(format!(
                    "published {} is durable but its staging link cannot be removed: {error:#}",
                    kind.label()
                ))
            })?;
        }
        lock_mutex(
            &self.pending_blob_publications,
            "hosted V2 pending blob publications",
        )?
        .remove(final_path);
        Ok(frame.len() as u64)
    }

    fn stage_immutable_blob(
        &self,
        session_id: &str,
        kind: ImmutableBlobKindV2,
        bytes: &[u8],
    ) -> Result<PathBuf> {
        validate_sha256_v2("session_id", session_id)?;
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .with_context(|| format!("failed to create {} staging identity", kind.label()))?;
        let staged_path = self.session_staging.join(format!(
            "{}{session_id}-{}.stage",
            kind.staging_prefix(),
            hex::encode(random)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(&staged_path).with_context(|| {
            format!(
                "failed to create private staged {} `{}`",
                kind.label(),
                staged_path.display()
            )
        })?;
        let staged = (|| -> Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            sync_directory(&self.session_staging)
        })();
        if let Err(error) = staged {
            self.remove_staged_blob(&staged_path)
                .map_err(|cleanup_error| {
                    self.poison_reopen_required(format!(
                        "failed to stage {} ({error:#}); cleanup cannot be proved: {cleanup_error:#}",
                        kind.label()
                    ))
                })?;
            return Err(error).with_context(|| {
                format!(
                    "failed to make staged {} `{}` durable",
                    kind.label(),
                    staged_path.display()
                )
            });
        }
        Ok(staged_path)
    }

    fn remove_staged_blob(&self, staged_path: &Path) -> Result<()> {
        match fs::remove_file(staged_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to remove immutable-blob staging file `{}`",
                        staged_path.display()
                    )
                });
            }
        }
        prove_path_absent(staged_path).with_context(|| {
            format!(
                "immutable-blob staging file `{}` remains after cleanup",
                staged_path.display()
            )
        })?;
        sync_directory(&self.session_staging)
    }

    pub fn read_checkpoint(
        &self,
        session_id: &str,
        actor_generation_sha256: &str,
        expected_snapshot_payload_bytes: u64,
    ) -> Result<EvaluatorStateSnapshotV1> {
        let path = self.checkpoint_path(session_id, actor_generation_sha256)?;
        read_canonical_checkpoint(&path, expected_snapshot_payload_bytes)
    }

    pub fn encoded_frame_bytes<T: serde::Serialize>(&self, value: &T) -> Result<u64> {
        let payload = canonical_hosted_bytes(value)?;
        let length: u64 = payload
            .len()
            .try_into()
            .context("hosted frame is too large")?;
        length
            .checked_add(4)
            .context("hosted frame length overflow")
    }

    pub fn durable_bytes(&self) -> Result<u64> {
        tree_bytes(&self.root)
    }

    pub fn sessions_durable_bytes(&self) -> Result<u64> {
        tree_bytes(&self.sessions)
    }

    pub fn session_durable_bytes(&self, session_id: &str) -> Result<u64> {
        validate_sha256_v2("session_id", session_id)?;
        tree_bytes(&self.session_directory(session_id))
    }

    /// Explicit, offline operator garbage collection. The two durable phases
    /// are independently callable so an operator can resume after a crash.
    pub fn gc_closed_session(&self, session_id: &str) -> Result<SignedJournalEntryV2> {
        self.authorize_closed_session_gc(session_id)?;
        self.complete_authorized_closed_session_gc(session_id)
    }

    /// Durably anchor intent to remove one exact, already-closed session.
    /// Repeating this after a crash returns the outstanding authorization.
    pub fn authorize_closed_session_gc(&self, session_id: &str) -> Result<SignedJournalEntryV2> {
        self.require_mutations_available()?;
        validate_sha256_v2("session_id", session_id)?;
        if let Some(pending) = self.pending_closed_session_gc()? {
            if pending.session_id == session_id {
                return Ok(pending.authorization);
            }
            bail!(
                "closed-session GC for `{}` must be completed before authorizing `{session_id}`",
                pending.session_id
            );
        }
        let directory = self.session_directory(session_id);
        let metadata = fs::symlink_metadata(&directory).with_context(|| {
            format!("closed hosted session `{session_id}` does not exist for explicit GC")
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("hosted GC target is not a real session directory");
        }
        let journal = self.read_journal(session_id)?;
        if let Some(corruption) = journal.corruption {
            bail!("refusing to garbage-collect corrupt session journal: {corruption}");
        }
        let terminal = journal
            .entries
            .last()
            .context("refusing to garbage-collect an empty session journal")?;
        if !matches!(&terminal.entry.event, JournalEventV2::SessionClosed { .. }) {
            bail!("explicit GC is permitted only for a durably closed session");
        }
        let terminal_head = terminal.entry_sha256.clone();
        let journal_path = self.journal_path(session_id);
        let (retained_journal_bytes, retained_journal_sha256) = regular_file_sha256(&journal_path)?;
        let reclaimed_bytes = tree_bytes(&directory)?
            .checked_sub(retained_journal_bytes)
            .context("closed-session journal exceeds its containing directory byte count")?;
        let hard_total_bytes = session_hard_total_bytes(&journal.entries)?;
        let authority = self.read_authority_journal()?;
        if let Some(corruption) = authority.corruption {
            bail!("refusing GC with a corrupt placement-authority journal: {corruption}");
        }
        let sequence = authority.entries.len() as u64 + 1;
        let previous = authority
            .entries
            .last()
            .map(|entry| entry.entry_sha256.clone());
        let authorized = self.signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: AUTHORITY_JOURNAL_ID_V2.to_owned(),
            sequence,
            previous_entry_sha256: previous,
            recorded_unix_ms: unix_time_ms()?,
            event: JournalEventV2::ClosedSessionGcAuthorized {
                session_id: session_id.to_owned(),
                terminal_journal_head_sha256: terminal_head.clone(),
                expected_reclaimed_bytes: reclaimed_bytes,
                retained_journal_sha256,
                retained_journal_bytes,
            },
        })?;
        let completion_preview = self.signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: AUTHORITY_JOURNAL_ID_V2.to_owned(),
            sequence: sequence
                .checked_add(1)
                .context("placement-authority journal sequence overflow")?,
            previous_entry_sha256: Some(authorized.entry_sha256.clone()),
            recorded_unix_ms: unix_time_ms()?,
            event: JournalEventV2::ClosedSessionGcCompleted {
                session_id: session_id.to_owned(),
                terminal_journal_head_sha256: terminal_head,
                reclaimed_bytes,
            },
        })?;
        let authorized_bytes = canonical_hosted_frame(&authorized)?.len() as u64;
        let completion_bytes = canonical_hosted_frame(&completion_preview)?.len() as u64;
        let current_control_debit = CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
            - self.remaining_authority_control_headroom_bytes();
        let authorized_control_debit = current_control_debit
            .checked_add(authorized_bytes)
            .context("GC authorization control-byte accounting overflow")?;
        if authorized_control_debit > CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 {
            bail!("GC authorization cannot fit the remaining durable authority-control headroom");
        }
        let completed_control_debit = authority_control_debit_after_event(
            authorized_control_debit,
            &completion_preview.entry.event,
            completion_bytes,
        )
        .context("GC completion control-byte accounting overflow")?;
        if completed_control_debit > CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 {
            bail!("GC completion cannot fit the durable authority-control headroom after its signed reclamation credit");
        }

        let durable_before = self.durable_bytes()?;
        let durable_after_authorized = durable_before
            .checked_add(authorized_bytes)
            .context("GC authorization total-state quota accounting overflow")?;
        if durable_after_authorized > hard_total_bytes {
            bail!("closed-session GC authorization would exceed signed total-state quota");
        }
        let durable_after_completed = durable_after_authorized
            .checked_sub(reclaimed_bytes)
            .and_then(|value| value.checked_add(completion_bytes))
            .context("GC completion total-state quota accounting overflow")?;
        if durable_after_completed > hard_total_bytes {
            bail!("closed-session GC completion would exceed signed total-state quota after reclamation");
        }
        self.append_authority_entry(&authorized)?;
        Ok(authorized)
    }

    /// Finish the currently authorized removal. If a previous process already
    /// removed all or part of the directory, the signed intent is sufficient
    /// to finish the exact validated path and append the missing completion.
    pub fn complete_authorized_closed_session_gc(
        &self,
        session_id: &str,
    ) -> Result<SignedJournalEntryV2> {
        self.complete_authorized_closed_session_gc_inner(session_id, no_closed_session_gc_fault)
    }

    fn complete_authorized_closed_session_gc_inner<F>(
        &self,
        session_id: &str,
        mut fault: F,
    ) -> Result<SignedJournalEntryV2>
    where
        F: FnMut(ClosedSessionGcFaultPointV2, &Path) -> Result<()>,
    {
        self.require_mutations_available()?;
        validate_sha256_v2("session_id", session_id)?;
        let pending = self
            .pending_closed_session_gc()?
            .context("no unfinished closed-session GC authorization exists")?;
        if pending.session_id != session_id {
            bail!(
                "unfinished closed-session GC names `{}`, not `{session_id}`",
                pending.session_id
            );
        }
        self.retain_closed_session_gc_journal_inner(&pending, &mut fault)?;
        let directory = self.session_directory(session_id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("hosted GC target is not a real session directory");
                }
                fs::remove_dir_all(&directory).with_context(|| {
                    format!("failed to remove closed hosted session `{session_id}`")
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fault(
            ClosedSessionGcFaultPointV2::SessionDirectoryUnlinked,
            &directory,
        )?;
        prove_path_absent(&directory)
            .context("closed hosted session directory remains after GC deletion")?;
        // This barrier and cache retirement are unconditional. A retry after a
        // crash may enter with the directory already absent, but Completed must
        // never be signed while the old name can reappear or its cached journal
        // is still presented as current by this process.
        sync_directory(&self.sessions)?;
        self.unregister_journal(session_id, &self.journal_path(session_id))?;

        let authority = self.read_authority_journal()?;
        if let Some(corruption) = authority.corruption {
            bail!(
                "refusing GC completion with a corrupt placement-authority journal: {corruption}"
            );
        }
        let sequence = authority.entries.len() as u64 + 1;
        let previous = authority
            .entries
            .last()
            .map(|entry| entry.entry_sha256.clone());
        let completed = self.signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: AUTHORITY_JOURNAL_ID_V2.to_owned(),
            sequence,
            previous_entry_sha256: previous,
            recorded_unix_ms: unix_time_ms()?,
            event: JournalEventV2::ClosedSessionGcCompleted {
                session_id: session_id.to_owned(),
                terminal_journal_head_sha256: pending.terminal_journal_head_sha256.clone(),
                reclaimed_bytes: pending.expected_reclaimed_bytes,
            },
        })?;
        let archive = self.read_closed_session_gc_archive(&pending.authorization.entry.event)?;
        let hard_total_bytes = session_hard_total_bytes(&archive.entries)?;
        let completion_bytes = canonical_hosted_frame(&completed)?.len() as u64;
        if self
            .durable_bytes()?
            .checked_add(completion_bytes)
            .context("GC completion total-state quota accounting overflow")?
            > hard_total_bytes
        {
            bail!("GC completion would exceed the retired session's signed total-state quota");
        }
        self.append_authority_entry(&completed)?;
        Ok(completed)
    }

    #[cfg(test)]
    fn retain_closed_session_gc_journal(&self, pending: &PendingClosedSessionGcV2) -> Result<()> {
        let mut no_fault = no_closed_session_gc_fault;
        self.retain_closed_session_gc_journal_inner(pending, &mut no_fault)
    }

    fn retain_closed_session_gc_journal_inner<F>(
        &self,
        pending: &PendingClosedSessionGcV2,
        fault: &mut F,
    ) -> Result<()>
    where
        F: FnMut(ClosedSessionGcFaultPointV2, &Path) -> Result<()>,
    {
        let source = self.journal_path(&pending.session_id);
        let archive = self.gc_archive_path(&pending.session_id)?;
        let source_exists = path_exists(&source)?;
        let archive_exists = path_exists(&archive)?;
        if archive_exists {
            validate_retained_gc_journal(
                &pending.session_id,
                &archive,
                &self.signer,
                &pending.terminal_journal_head_sha256,
                &pending.retained_journal_sha256,
                pending.retained_journal_bytes,
            )?;
            // The archive may have been published by an earlier attempt whose
            // parent barrier failed. Re-drive it on every retry.
            sync_directory(&self.gc_tombstones)?;
            if source_exists {
                validate_retained_gc_journal(
                    &pending.session_id,
                    &source,
                    &self.signer,
                    &pending.terminal_journal_head_sha256,
                    &pending.retained_journal_sha256,
                    pending.retained_journal_bytes,
                )?;
                fs::remove_file(&source).with_context(|| {
                    format!(
                        "failed to remove duplicate live GC journal for `{}`",
                        pending.session_id
                    )
                })?;
                fault(
                    ClosedSessionGcFaultPointV2::DuplicateSourceUnlinked,
                    &source,
                )?;
            }
            self.sync_live_gc_source_absence(&source)?;
            return Ok(());
        }
        if !source_exists {
            bail!("signed GC authorization has neither a live nor retained session journal");
        }
        validate_retained_gc_journal(
            &pending.session_id,
            &source,
            &self.signer,
            &pending.terminal_journal_head_sha256,
            &pending.retained_journal_sha256,
            pending.retained_journal_bytes,
        )?;
        fs::rename(&source, &archive).with_context(|| {
            format!(
                "failed to atomically retain GC journal for `{}`",
                pending.session_id
            )
        })?;
        fault(
            ClosedSessionGcFaultPointV2::JournalRenamePublished,
            &archive,
        )?;
        validate_retained_gc_journal(
            &pending.session_id,
            &archive,
            &self.signer,
            &pending.terminal_journal_head_sha256,
            &pending.retained_journal_sha256,
            pending.retained_journal_bytes,
        )?;
        sync_directory(&self.gc_tombstones)?;
        self.sync_live_gc_source_absence(&source)?;
        Ok(())
    }

    fn sync_live_gc_source_absence(&self, source: &Path) -> Result<()> {
        prove_path_absent(source).context("live GC journal source still exists")?;
        let source_parent = source
            .parent()
            .context("live session journal has no parent directory")?;
        match fs::symlink_metadata(source_parent) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("live GC journal parent is not a real directory");
                }
                sync_directory(source_parent)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                prove_path_absent(source_parent)
                    .context("deleted GC session directory unexpectedly reappeared")?;
                sync_directory(&self.sessions)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn pending_closed_session_gc(&self) -> Result<Option<PendingClosedSessionGcV2>> {
        let journal = self.read_authority_journal()?;
        if let Some(corruption) = journal.corruption {
            bail!("placement-authority journal is corrupt: {corruption}");
        }
        let mut pending: Option<PendingClosedSessionGcV2> = None;
        for entry in journal.entries {
            match &entry.entry.event {
                JournalEventV2::ClosedSessionGcAuthorized {
                    session_id,
                    terminal_journal_head_sha256,
                    expected_reclaimed_bytes,
                    retained_journal_sha256,
                    retained_journal_bytes,
                } => {
                    if pending.is_some() {
                        bail!("placement-authority journal contains overlapping GC intents");
                    }
                    pending = Some(PendingClosedSessionGcV2 {
                        authorization: entry.clone(),
                        session_id: session_id.clone(),
                        terminal_journal_head_sha256: terminal_journal_head_sha256.clone(),
                        expected_reclaimed_bytes: *expected_reclaimed_bytes,
                        retained_journal_sha256: retained_journal_sha256.clone(),
                        retained_journal_bytes: *retained_journal_bytes,
                    });
                }
                JournalEventV2::ClosedSessionGcCompleted {
                    session_id,
                    terminal_journal_head_sha256,
                    reclaimed_bytes,
                } => {
                    let active = pending
                        .take()
                        .context("GC completion has no preceding authorization")?;
                    if active.session_id != *session_id
                        || active.terminal_journal_head_sha256 != *terminal_journal_head_sha256
                        || active.expected_reclaimed_bytes != *reclaimed_bytes
                    {
                        bail!("GC completion does not match its authorization");
                    }
                }
                _ => {}
            }
        }
        Ok(pending)
    }

    fn session_directory(&self, session_id: &str) -> PathBuf {
        self.sessions.join(session_id)
    }

    fn journal_path(&self, session_id: &str) -> PathBuf {
        self.session_directory(session_id).join(JOURNAL_FILE)
    }

    fn authority_journal_path(&self) -> PathBuf {
        self.root.join(AUTHORITY_JOURNAL_FILE)
    }

    fn gc_archive_path(&self, session_id: &str) -> Result<PathBuf> {
        validate_sha256_v2("session_id", session_id)?;
        Ok(self.gc_tombstones.join(format!("{session_id}.cborseq")))
    }

    fn operation_path(&self, session_id: &str, operation_id: &str) -> Result<PathBuf> {
        validate_sha256_v2("session_id", session_id)?;
        validate_identifier_v2("operation_id", operation_id)?;
        Ok(self
            .session_directory(session_id)
            .join(OPERATIONS_DIRECTORY)
            .join(format!("{operation_id}.cbor")))
    }

    fn checkpoint_path(&self, session_id: &str, actor_generation_sha256: &str) -> Result<PathBuf> {
        validate_sha256_v2("session_id", session_id)?;
        validate_sha256_v2("actor_generation_sha256", actor_generation_sha256)?;
        Ok(self
            .session_directory(session_id)
            .join(CHECKPOINTS_DIRECTORY)
            .join(format!("{actor_generation_sha256}.cbor")))
    }
}

fn is_emergency_authority_control_event(event: &JournalEventV2) -> bool {
    matches!(
        event,
        JournalEventV2::ClosedSessionGcAuthorized { .. }
            | JournalEventV2::ClosedSessionGcCompleted { .. }
            | JournalEventV2::JournalTailRepaired { .. }
    )
}

fn authority_control_debit_after_event(
    current: u64,
    event: &JournalEventV2,
    frame_bytes: u64,
) -> Option<u64> {
    let appended = current.checked_add(frame_bytes)?;
    Some(match event {
        JournalEventV2::ClosedSessionGcCompleted {
            reclaimed_bytes, ..
        } => appended.saturating_sub(*reclaimed_bytes),
        _ => appended,
    })
}

fn authority_control_debit(entries: &[SignedJournalEntryV2]) -> Result<u64> {
    let mut debit = 0_u64;
    let mut pending_gc: Option<(String, String, u64)> = None;
    for entry in entries {
        match &entry.entry.event {
            JournalEventV2::ClosedSessionGcAuthorized {
                session_id,
                terminal_journal_head_sha256,
                expected_reclaimed_bytes,
                ..
            } => {
                if pending_gc.is_some() {
                    bail!("placement-authority journal contains overlapping GC intents");
                }
                pending_gc = Some((
                    session_id.clone(),
                    terminal_journal_head_sha256.clone(),
                    *expected_reclaimed_bytes,
                ));
            }
            JournalEventV2::ClosedSessionGcCompleted {
                session_id,
                terminal_journal_head_sha256,
                reclaimed_bytes,
            } => {
                let pending = pending_gc
                    .take()
                    .context("GC completion has no preceding authorization")?;
                if pending.0 != *session_id
                    || pending.1 != *terminal_journal_head_sha256
                    || pending.2 != *reclaimed_bytes
                {
                    bail!("GC completion does not match its authorization");
                }
            }
            _ => {}
        }
        if is_emergency_authority_control_event(&entry.entry.event) {
            let frame_bytes = canonical_hosted_frame(entry)?.len() as u64;
            debit = authority_control_debit_after_event(debit, &entry.entry.event, frame_bytes)
                .context("durable authority-control debit overflow")?;
            if debit > CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 {
                bail!(
                    "durable authority-control debit {debit} exceeds its fixed {}-byte headroom",
                    CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
                );
            }
        }
    }
    Ok(debit)
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect path `{}`", path.display()))
        }
    }
}

fn prove_path_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!("path `{}` is still present", path.display()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to prove path `{}` absent", path.display()))
        }
    }
}

fn validate_exact_published_session(
    session_id: &str,
    directory: &Path,
    first_entry: &SignedJournalEntryV2,
    signer: &HostedNodeSignerV2,
) -> Result<JournalHeadV2> {
    let directory_metadata = fs::symlink_metadata(directory)?;
    require_private_staging_entry(directory, &directory_metadata, true)?;
    let expected = BTreeSet::from([
        OPERATIONS_DIRECTORY.to_owned(),
        CHECKPOINTS_DIRECTORY.to_owned(),
        JOURNAL_FILE.to_owned(),
    ]);
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        actual.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("published session entry is not UTF-8"))?,
        );
    }
    if actual != expected {
        bail!("published session tree differs from the exact initial layout");
    }

    for child in [OPERATIONS_DIRECTORY, CHECKPOINTS_DIRECTORY] {
        let path = directory.join(child);
        let metadata = fs::symlink_metadata(&path)?;
        require_private_staging_entry(&path, &metadata, true)?;
        if fs::read_dir(&path)?.next().transpose()?.is_some() {
            bail!("published initial session `{child}` directory is not empty");
        }
    }
    let journal_path = directory.join(JOURNAL_FILE);
    let journal_metadata = fs::symlink_metadata(&journal_path)?;
    require_private_staging_entry(&journal_path, &journal_metadata, false)?;
    let scan = scan_validated_journal(session_id, &journal_path, signer)?;
    if let Some(corruption) = scan.corruption {
        bail!("published initial session journal is corrupt: {corruption}");
    }
    if scan.torn_tail.is_some()
        || scan.entries.len() != 1
        || scan.entries.first() != Some(first_entry)
        || scan.head.next_sequence != 2
        || scan.head.head_sha256.as_deref() != Some(first_entry.entry_sha256.as_str())
    {
        bail!("published session does not contain the exact first receipt");
    }
    Ok(scan.head)
}

fn scan_validated_journal(
    journal_id: &str,
    path: &Path,
    signer: &HostedNodeSignerV2,
) -> Result<JournalScanV2> {
    require_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open hosted V2 journal `{}`", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("hosted V2 journal scan target is not a regular file");
    }
    let old_bytes = metadata.len();
    let mut offset = 0_u64;
    let mut entries = Vec::new();
    let mut expected_sequence = 1_u64;
    let mut expected_previous: Option<String> = None;

    while offset < old_bytes {
        let frame_start = offset;
        let remaining = old_bytes - frame_start;
        if remaining < 4 {
            return Ok(torn_journal_scan(
                entries,
                expected_sequence,
                expected_previous,
                old_bytes,
                frame_start,
            ));
        }

        let mut length_bytes = [0_u8; 4];
        file.read_exact(&mut length_bytes)?;
        offset += 4;
        let payload_length = u32::from_be_bytes(length_bytes) as u64;
        if payload_length > MAX_HOSTED_FRAME_BYTES as u64 {
            return Ok(corrupt_journal_scan(
                entries,
                expected_sequence,
                expected_previous,
                old_bytes,
                format!(
                    "complete frame length {payload_length} exceeds maximum {MAX_HOSTED_FRAME_BYTES} at byte {frame_start}"
                ),
            ));
        }
        let frame_end = offset
            .checked_add(payload_length)
            .context("hosted V2 journal frame length overflow")?;
        if frame_end > old_bytes {
            return Ok(torn_journal_scan(
                entries,
                expected_sequence,
                expected_previous,
                old_bytes,
                frame_start,
            ));
        }

        let mut frame = Vec::with_capacity(4 + payload_length as usize);
        frame.extend_from_slice(&length_bytes);
        let payload_start = frame.len();
        frame.resize(payload_start + payload_length as usize, 0);
        file.read_exact(&mut frame[payload_start..])?;
        offset = frame_end;

        let entry = match read_hosted_frame::<_, SignedJournalEntryV2>(&mut Cursor::new(frame)) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                return Ok(corrupt_journal_scan(
                    entries,
                    expected_sequence,
                    expected_previous,
                    old_bytes,
                    format!("complete frame at byte {frame_start} decoded as empty"),
                ))
            }
            Err(error) => {
                return Ok(corrupt_journal_scan(
                    entries,
                    expected_sequence,
                    expected_previous,
                    old_bytes,
                    format!("complete invalid frame at byte {frame_start}: {error:#}"),
                ))
            }
        };
        if let Err(error) = entry.verify() {
            return Ok(corrupt_journal_scan(
                entries,
                expected_sequence,
                expected_previous,
                old_bytes,
                format!("complete invalid signed frame at byte {frame_start}: {error:#}"),
            ));
        }
        if entry.signer_public_key != signer.public_key_hex() {
            return Ok(corrupt_journal_scan(
                entries,
                expected_sequence,
                expected_previous,
                old_bytes,
                format!("frame at byte {frame_start} was signed by a different node key"),
            ));
        }
        if entry.entry.session_id != journal_id
            || entry.entry.sequence != expected_sequence
            || entry.entry.previous_entry_sha256 != expected_previous
        {
            return Ok(corrupt_journal_scan(
                entries,
                expected_sequence,
                expected_previous,
                old_bytes,
                format!("journal sequence or hash-chain discontinuity at byte {frame_start}"),
            ));
        }
        if let Err(error) = validate_journal_event_location(journal_id, &entry.entry.event) {
            return Ok(corrupt_journal_scan(
                entries,
                expected_sequence,
                expected_previous,
                old_bytes,
                format!("invalid journal event location or shape: {error:#}"),
            ));
        }

        expected_sequence = match expected_sequence.checked_add(1) {
            Some(sequence) => sequence,
            None => {
                return Ok(corrupt_journal_scan(
                    entries,
                    expected_sequence,
                    expected_previous,
                    old_bytes,
                    "hosted V2 journal sequence overflow".to_owned(),
                ))
            }
        };
        expected_previous = Some(entry.entry_sha256.clone());
        entries.push(entry);
    }

    if file.stream_position()? != old_bytes || file.metadata()?.len() != old_bytes {
        bail!("hosted V2 journal changed while it was being validated");
    }
    Ok(JournalScanV2 {
        entries,
        head: JournalHeadV2 {
            next_sequence: expected_sequence,
            head_sha256: expected_previous,
            bytes: old_bytes,
        },
        corruption: None,
        torn_tail: None,
    })
}

fn torn_journal_scan(
    entries: Vec<SignedJournalEntryV2>,
    next_sequence: u64,
    head_sha256: Option<String>,
    old_bytes: u64,
    new_bytes: u64,
) -> JournalScanV2 {
    JournalScanV2 {
        entries,
        head: JournalHeadV2 {
            next_sequence,
            head_sha256: head_sha256.clone(),
            bytes: new_bytes,
        },
        corruption: None,
        torn_tail: Some(TornJournalTailV2 {
            old_bytes,
            new_bytes,
            recovered_head_sha256: head_sha256,
        }),
    }
}

fn corrupt_journal_scan(
    entries: Vec<SignedJournalEntryV2>,
    next_sequence: u64,
    head_sha256: Option<String>,
    bytes: u64,
    corruption: String,
) -> JournalScanV2 {
    JournalScanV2 {
        entries,
        head: JournalHeadV2 {
            next_sequence,
            head_sha256,
            bytes,
        },
        corruption: Some(corruption),
        torn_tail: None,
    }
}

fn canonical_hosted_frame<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let payload = canonical_hosted_bytes(value)?;
    canonical_frame_from_payload(payload)
}

fn verify_exact_appended_frame(
    path: &Path,
    before: u64,
    expected_after: u64,
    frame: &[u8],
) -> Result<()> {
    require_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut reader = options.open(path)?;
    let metadata = reader.metadata()?;
    if !metadata.is_file() || metadata.len() != expected_after {
        bail!(
            "journal length {} differs from exact appended length {expected_after}",
            metadata.len()
        );
    }
    reader.seek(std::io::SeekFrom::Start(before))?;
    let mut actual = Vec::new();
    actual
        .try_reserve_exact(frame.len())
        .map_err(|error| anyhow::anyhow!("failed to reserve journal-tail buffer: {error}"))?;
    actual.resize(frame.len(), 0);
    reader.read_exact(&mut actual)?;
    if actual != frame {
        bail!("journal tail differs from the exact canonical appended frame");
    }
    if reader.stream_position()? != expected_after || reader.metadata()?.len() != expected_after {
        bail!("journal changed while its appended tail was being revalidated");
    }
    Ok(())
}

fn canonical_checkpoint_frame(
    snapshot: &EvaluatorStateSnapshotV1,
    max_snapshot_payload_bytes: u64,
) -> Result<Vec<u8>> {
    let payload = snapshot.canonical_bytes()?;
    let payload_bytes: u64 = payload
        .len()
        .try_into()
        .context("evaluator checkpoint payload length exceeds u64")?;
    if payload_bytes > max_snapshot_payload_bytes {
        bail!(
            "evaluator checkpoint payload length {payload_bytes} exceeds authenticated limit {max_snapshot_payload_bytes}"
        );
    }
    canonical_frame_from_payload(payload)
}

fn canonical_frame_from_payload(payload: Vec<u8>) -> Result<Vec<u8>> {
    let length: u32 = payload
        .len()
        .try_into()
        .context("canonical frame payload exceeds its four-byte length prefix")?;
    let frame_capacity = payload
        .len()
        .checked_add(4)
        .context("canonical frame length exceeds this process's address space")?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(frame_capacity)
        .map_err(|error| anyhow::anyhow!("failed to reserve bounded frame buffer: {error}"))?;
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn read_canonical_checkpoint(
    path: &Path,
    expected_snapshot_payload_bytes: u64,
) -> Result<EvaluatorStateSnapshotV1> {
    let expected_frame_bytes = expected_snapshot_payload_bytes
        .checked_add(4)
        .context("durable evaluator checkpoint frame length overflow")?;
    let expected_payload_len: usize = expected_snapshot_payload_bytes
        .try_into()
        .context("durable evaluator checkpoint exceeds this process's address space")?;
    let _: u32 = expected_payload_len
        .try_into()
        .context("durable evaluator checkpoint exceeds its four-byte length prefix")?;

    require_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).with_context(|| {
        format!(
            "failed to open durable evaluator checkpoint `{}` safely",
            path.display()
        )
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != expected_frame_bytes {
        bail!(
            "durable evaluator checkpoint `{}` byte length {} does not match signed expectation {expected_frame_bytes}",
            path.display(),
            metadata.len()
        );
    }

    let mut length_prefix = [0_u8; 4];
    file.read_exact(&mut length_prefix)
        .context("durable evaluator checkpoint has an incomplete length prefix")?;
    let declared_payload_bytes = u32::from_be_bytes(length_prefix) as u64;
    if declared_payload_bytes != expected_snapshot_payload_bytes {
        bail!(
            "durable evaluator checkpoint declares {declared_payload_bytes} payload bytes but its signed receipt expects {expected_snapshot_payload_bytes}"
        );
    }

    let mut payload = Vec::new();
    payload
        .try_reserve_exact(expected_payload_len)
        .map_err(|error| anyhow::anyhow!("failed to reserve bounded checkpoint buffer: {error}"))?;
    payload.resize(expected_payload_len, 0);
    file.read_exact(&mut payload)
        .context("durable evaluator checkpoint has an incomplete payload")?;
    let mut extra = [0_u8; 1];
    if file.read(&mut extra)? != 0 {
        bail!("durable evaluator checkpoint contains bytes after its signed payload");
    }

    let snapshot: EvaluatorStateSnapshotV1 = crate::wire::decode_message(&payload)
        .context("durable evaluator checkpoint payload is invalid")?;
    snapshot.validate()?;
    if snapshot.canonical_bytes()? != payload {
        bail!("durable evaluator checkpoint payload is not canonical Ostadix CBOR");
    }
    Ok(snapshot)
}

fn regular_file_sha256(path: &Path) -> Result<(u64, String)> {
    require_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open `{}` for durable digest", path.display()))?;
    let expected_bytes = file.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut read_bytes = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        read_bytes = read_bytes
            .checked_add(count as u64)
            .context("durable digest byte count overflow")?;
    }
    if read_bytes != expected_bytes || file.metadata()?.len() != expected_bytes {
        bail!("durable file changed while its digest was computed");
    }
    Ok((expected_bytes, hex::encode(digest.finalize())))
}

fn validate_retained_gc_journal(
    session_id: &str,
    path: &Path,
    signer: &HostedNodeSignerV2,
    terminal_journal_head_sha256: &str,
    expected_sha256: &str,
    expected_bytes: u64,
) -> Result<JournalScanV2> {
    validate_sha256_v2("retired session_id", session_id)?;
    validate_sha256_v2("terminal_journal_head_sha256", terminal_journal_head_sha256)?;
    validate_sha256_v2("retained_journal_sha256", expected_sha256)?;
    if expected_bytes == 0 {
        bail!("retained GC journal must not be empty");
    }
    let (actual_bytes, actual_sha256) = regular_file_sha256(path)?;
    if actual_bytes != expected_bytes || actual_sha256 != expected_sha256 {
        bail!("retained GC journal bytes do not match signed authorization");
    }
    let scan = scan_validated_journal(session_id, path, signer)?;
    if let Some(corruption) = &scan.corruption {
        bail!("retained GC journal is corrupt: {corruption}");
    }
    if scan.torn_tail.is_some() || scan.head.bytes != expected_bytes {
        bail!("retained GC journal has a torn or mismatched tail");
    }
    let terminal = scan
        .entries
        .last()
        .context("retained GC journal is empty")?;
    if !matches!(terminal.entry.event, JournalEventV2::SessionClosed { .. })
        || terminal.entry_sha256 != terminal_journal_head_sha256
    {
        bail!("retained GC journal does not match its signed terminal head");
    }
    Ok(scan)
}

fn session_hard_total_bytes(entries: &[SignedJournalEntryV2]) -> Result<u64> {
    let first = entries
        .first()
        .context("session journal is empty while reading its signed quota")?;
    let JournalEventV2::SessionOpened {
        state_quota_limits, ..
    } = &first.entry.event
    else {
        bail!("session journal does not begin with SessionOpened");
    };
    Ok(state_quota_limits.max_state_bytes_total())
}

fn exact_existing_blob(path: &Path, expected: &[u8], kind: ImmutableBlobKindV2) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect durable {} `{}`",
                    kind.label(),
                    path.display()
                )
            })
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "content-addressed {} `{}` exists as a non-regular path",
            kind.label(),
            path.display()
        );
    }
    if metadata.len() != expected.len() as u64 {
        bail!(
            "content-addressed {} `{}` exists with different canonical bytes",
            kind.label(),
            path.display()
        );
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path).with_context(|| {
        format!(
            "content-addressed {} `{}` exists but cannot be read safely",
            kind.label(),
            path.display()
        )
    })?;
    if !file.metadata()?.is_file() {
        bail!(
            "content-addressed {} `{}` changed to a non-regular path",
            kind.label(),
            path.display()
        );
    }
    let mut file = file;
    let mut offset = 0_usize;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < expected.len() {
        let count = buffer.len().min(expected.len() - offset);
        file.read_exact(&mut buffer[..count])?;
        if buffer[..count] != expected[offset..offset + count] {
            bail!(
                "content-addressed {} `{}` exists with different canonical bytes",
                kind.label(),
                path.display()
            );
        }
        offset += count;
    }
    let mut extra = [0_u8; 1];
    if file.read(&mut extra)? != 0 {
        bail!(
            "content-addressed {} `{}` exists with different canonical bytes",
            kind.label(),
            path.display()
        );
    }
    Ok(true)
}

#[cfg(unix)]
fn require_same_file_identity(left: &Path, right: &Path) -> Result<()> {
    let left = fs::symlink_metadata(left)?;
    let right = fs::symlink_metadata(right)?;
    if left.dev() != right.dev() || left.ino() != right.ino() {
        bail!("paths name different filesystem objects");
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_same_file_identity(_left: &Path, _right: &Path) -> Result<()> {
    // The in-memory pending-publication map is populated only after this
    // store's successful hard-link call. Platforms without stable std file
    // identities still revalidate both complete canonical byte strings.
    Ok(())
}

fn lock_mutex<'a, T>(mutex: &'a Mutex<T>, label: &str) -> Result<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| anyhow::anyhow!("{label} mutex was poisoned"))
}

fn validate_journal_event_location(journal_id: &str, event: &JournalEventV2) -> Result<()> {
    match event {
        JournalEventV2::ClosedSessionGcAuthorized {
            session_id,
            terminal_journal_head_sha256,
            expected_reclaimed_bytes: _,
            retained_journal_sha256,
            retained_journal_bytes,
        } => {
            if journal_id != AUTHORITY_JOURNAL_ID_V2 {
                bail!("ClosedSessionGcAuthorized is permitted only in the placement-authority journal");
            }
            validate_sha256_v2("retired session_id", session_id)?;
            validate_sha256_v2("terminal_journal_head_sha256", terminal_journal_head_sha256)?;
            validate_sha256_v2("retained_journal_sha256", retained_journal_sha256)?;
            if *retained_journal_bytes == 0 {
                bail!("signed retained GC journal must not be empty");
            }
        }
        JournalEventV2::ClosedSessionGcCompleted {
            session_id,
            terminal_journal_head_sha256,
            reclaimed_bytes: _,
        } => {
            if journal_id != AUTHORITY_JOURNAL_ID_V2 {
                bail!(
                    "ClosedSessionGcCompleted is permitted only in the placement-authority journal"
                );
            }
            validate_sha256_v2("retired session_id", session_id)?;
            validate_sha256_v2("terminal_journal_head_sha256", terminal_journal_head_sha256)?;
        }
        JournalEventV2::JournalTailRepaired {
            journal_id: repaired_journal_id,
            old_bytes,
            new_bytes,
            recovered_head_sha256,
        } => {
            if journal_id != AUTHORITY_JOURNAL_ID_V2 {
                bail!("JournalTailRepaired is permitted only in the placement-authority journal");
            }
            if repaired_journal_id != AUTHORITY_JOURNAL_ID_V2 {
                validate_sha256_v2("repaired journal_id", repaired_journal_id)?;
            }
            if new_bytes >= old_bytes {
                bail!("JournalTailRepaired does not describe a strict truncation");
            }
            if (*new_bytes == 0) != recovered_head_sha256.is_none() {
                bail!("JournalTailRepaired recovered head does not match its retained byte prefix");
            }
            if let Some(recovered_head) = recovered_head_sha256 {
                validate_sha256_v2("recovered_head_sha256", recovered_head)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn pending_gc_session_id(entries: &[SignedJournalEntryV2]) -> Result<Option<String>> {
    let mut pending: Option<(String, String, u64)> = None;
    for entry in entries {
        match &entry.entry.event {
            JournalEventV2::ClosedSessionGcAuthorized {
                session_id,
                terminal_journal_head_sha256,
                expected_reclaimed_bytes,
                ..
            } => {
                if pending.is_some() {
                    bail!("placement-authority journal contains overlapping GC intents");
                }
                pending = Some((
                    session_id.clone(),
                    terminal_journal_head_sha256.clone(),
                    *expected_reclaimed_bytes,
                ));
            }
            JournalEventV2::ClosedSessionGcCompleted {
                session_id,
                terminal_journal_head_sha256,
                reclaimed_bytes,
            } => {
                let active = pending
                    .take()
                    .context("GC completion has no preceding authorization")?;
                if active.0 != *session_id
                    || active.1 != *terminal_journal_head_sha256
                    || active.2 != *reclaimed_bytes
                {
                    bail!("GC completion does not match its authorization");
                }
            }
            _ => {}
        }
    }
    Ok(pending.map(|(session_id, _, _)| session_id))
}

pub fn default_hosted_v2_state_dir() -> PathBuf {
    if let Some(root) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(root).join("ostadix").join("hosted-v2");
    }
    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("ostadix")
            .join("hosted-v2");
    }
    PathBuf::from(".").join(".ostadix").join("hosted-v2")
}

pub fn default_hosted_v2_node_key_path() -> PathBuf {
    default_hosted_v2_state_dir().join("node-signing-key.v2")
}

fn create_private_directory_new(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).with_context(|| {
        format!(
            "refusing to replace hosted V2 directory `{}`",
            path.display()
        )
    })
}

fn require_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect hosted V2 file `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("hosted V2 path `{}` is not a regular file", path.display());
    }
    Ok(())
}

fn reconcile_unpublished_session_staging(staging: &Path) -> Result<()> {
    let mut removed_any = false;
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("hosted session staging name is not UTF-8"))?;
        let metadata = fs::symlink_metadata(&path)?;
        if let Some(rest) = name.strip_prefix("install-") {
            require_private_staging_entry(&path, &metadata, true)?;
            let Some((session_id, random)) = rest.split_once('-') else {
                bail!("hosted session staging contains malformed entry `{name}`");
            };
            validate_sha256_v2("staged session_id", session_id)?;
            if !is_lower_hex(random, 32) {
                bail!("hosted session staging contains malformed entry `{name}`");
            }
            fs::remove_dir_all(&path).with_context(|| {
                format!(
                    "failed to remove unpublished hosted session staging directory `{}`",
                    path.display()
                )
            })?;
        } else if let Some(rest) = name
            .strip_prefix(ImmutableBlobKindV2::Operation.staging_prefix())
            .or_else(|| name.strip_prefix(ImmutableBlobKindV2::Checkpoint.staging_prefix()))
        {
            require_private_staging_entry(&path, &metadata, false)?;
            let Some(rest) = rest.strip_suffix(".stage") else {
                bail!("hosted session staging contains malformed entry `{name}`");
            };
            let Some((session_id, random)) = rest.split_once('-') else {
                bail!("hosted session staging contains malformed entry `{name}`");
            };
            validate_sha256_v2("staged blob session_id", session_id)?;
            if !is_lower_hex(random, 32) {
                bail!("hosted session staging contains malformed entry `{name}`");
            }
            fs::remove_file(&path).with_context(|| {
                format!(
                    "failed to remove unpublished immutable-blob staging file `{}`",
                    path.display()
                )
            })?;
        } else {
            bail!("hosted session staging contains unknown entry `{name}`");
        }
        removed_any = true;
    }
    if removed_any {
        sync_directory(staging)?;
    }
    Ok(())
}

fn require_private_staging_entry(
    path: &Path,
    metadata: &fs::Metadata,
    directory: bool,
) -> Result<()> {
    let expected_type = if directory {
        "directory"
    } else {
        "regular file"
    };
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        bail!(
            "hosted session staging entry `{}` is not a private {expected_type}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "hosted session staging entry `{}` is not private",
                path.display()
            );
        }
    }
    Ok(())
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn tree_bytes(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        bail!("hosted V2 state contains symlink `{}`", path.display());
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        bail!(
            "hosted V2 state contains unsupported object `{}`",
            path.display()
        );
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        total = total
            .checked_add(tree_bytes(&entry?.path())?)
            .context("hosted V2 durable-byte count overflow")?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::state::{
        BackendCheckpointV1, BackendStateTierV1, EvaluatorActorCheckpointV1,
    };
    use crate::hosted_remote::v2::{HostedPlacementIdentityV2, SessionStateTierV2};
    use crate::ir::BackendRegistry;
    use crate::placement::{
        CanonicalPlacementRecordV1, GenerationV1, PlacementReservationV1, SemanticDigestV1,
        StateQuotaLimitsV2, StateReservationV2, StateSessionIdV2, TaskAttemptIdV1,
    };

    #[test]
    fn persistent_state_lock_reopens_but_refuses_concurrent_owner() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        ensure_private_directory_v2(&root)?;

        // A regular lock marker can survive a crash. Its presence is not
        // ownership; only the advisory lock on its open inode is.
        let stale = root.join(STATE_LOCK_FILE);
        fs::write(&stale, b"pid=stale\n")?;

        let signer = HostedNodeSignerV2::from_secret_bytes([7; 32]);
        let first = DurableSessionStoreV2::open(&root, signer.clone())?;
        let concurrent = DurableSessionStoreV2::open(&root, signer.clone())
            .expect_err("a live state-root owner must exclude a concurrent runtime");
        assert!(
            format!("{concurrent:#}").contains("already locked"),
            "{concurrent:#}"
        );
        drop(first);

        let reopened = DurableSessionStoreV2::open(&root, signer.clone())?;
        assert!(
            stale.is_file(),
            "the advisory-lock inode must stay persistent"
        );
        drop(reopened);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn state_lock_refuses_symlink() -> Result<()> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        ensure_private_directory_v2(&root)?;
        let target = directory.path().join("outside");
        fs::write(&target, b"do not lock through me")?;
        symlink(&target, root.join(STATE_LOCK_FILE))?;

        let error =
            DurableSessionStoreV2::open(&root, HostedNodeSignerV2::from_secret_bytes([9; 32]))
                .expect_err("state locking must not follow a symlink");
        assert!(format!("{error:#}").contains("failed to open"), "{error:#}");
        Ok(())
    }

    #[test]
    fn session_paths_require_canonical_sha256_identity() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = DurableSessionStoreV2::open(
            directory.path().join("state"),
            HostedNodeSignerV2::from_secret_bytes([11; 32]),
        )?;
        for dangerous in [".", "..", "session-name"] {
            assert!(store.read_journal(dangerous).is_err());
            assert!(store.gc_closed_session(dangerous).is_err());
        }
        Ok(())
    }

    fn first_open_receipt(
        signer: &HostedNodeSignerV2,
        state_session: StateSessionIdV2,
    ) -> Result<SignedJournalEntryV2> {
        let session_id = state_session.semantic_digest()?.to_string();
        let digest = || SemanticDigestV1::hash_bytes("ostadix/store-test/v2", b"fixture");
        signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id,
            sequence: 1,
            previous_entry_sha256: None,
            recorded_unix_ms: 1,
            event: JournalEventV2::SessionOpened {
                request_sha256: "66".repeat(32),
                principal_sha256: "11".repeat(32),
                bearer_salt: "22".repeat(32),
                bearer_hash: "33".repeat(32),
                capability_commitment: digest(),
                state_tier: SessionStateTierV2::Stateless,
                state_session,
                state_quota_generation: GenerationV1::new(1)?,
                state_quota_limits: StateQuotaLimitsV2::new(1, 1, 0, 1024 * 1024, 1024 * 1024)?,
                state_reservation: StateReservationV2::new(1, 0, 1024 * 1024)?,
                placement_identity: HostedPlacementIdentityV2 {
                    target_descriptor: digest(),
                    requirement_footprint: digest(),
                    backend_implementation: digest(),
                    realization_pipeline: digest(),
                    trust_policy: digest(),
                    reservation: PlacementReservationV1::new(1, 1, 0)?,
                },
                placement_lease_sha256: "44".repeat(32),
                placement_lease_nonce: "55".repeat(32),
                client_request_id: "open-atomic".to_owned(),
            },
        })
    }

    fn install_test_session(
        store: &DurableSessionStoreV2,
        signer: &HostedNodeSignerV2,
        identity: &[u8],
    ) -> Result<(String, SignedJournalEntryV2)> {
        let state_session = StateSessionIdV2::new(
            "store-test-node",
            GenerationV1::new(1)?,
            SemanticDigestV1::hash_bytes("ostadix/store-test/session/v2", identity),
        )?;
        let session_id = state_session.semantic_digest()?.to_string();
        let receipt = first_open_receipt(signer, state_session)?;
        store.install_session(&session_id, &receipt)?;
        Ok((session_id, receipt))
    }

    fn install_closed_test_session(
        store: &DurableSessionStoreV2,
        signer: &HostedNodeSignerV2,
        identity: &[u8],
    ) -> Result<String> {
        let (session_id, opened) = install_test_session(store, signer, identity)?;
        let closed = next_test_receipt(signer, &session_id, 2, Some(opened.entry_sha256))?;
        store.append_entry(&session_id, &closed)?;
        Ok(session_id)
    }

    fn next_test_receipt(
        signer: &HostedNodeSignerV2,
        session_id: &str,
        sequence: u64,
        previous_entry_sha256: Option<String>,
    ) -> Result<SignedJournalEntryV2> {
        signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: session_id.to_owned(),
            sequence,
            previous_entry_sha256,
            recorded_unix_ms: sequence,
            event: JournalEventV2::SessionClosed {
                client_sequence: sequence.saturating_sub(1),
                client_request_id: format!("close-{sequence}"),
                request_sha256: format!("{:064x}", sequence),
                actor_generation: None,
            },
        })
    }

    fn accepted_test_receipt(
        signer: &HostedNodeSignerV2,
        session_id: &str,
        sequence: u64,
        previous_entry_sha256: String,
        placement_lease_nonce: &str,
    ) -> Result<SignedJournalEntryV2> {
        let operation = test_operation(
            &format!("accepted-{sequence}"),
            "bash^(printf 'accepted')_bash",
        )?;
        signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: session_id.to_owned(),
            sequence,
            previous_entry_sha256: Some(previous_entry_sha256),
            recorded_unix_ms: sequence,
            event: JournalEventV2::OperationAccepted {
                client_sequence: sequence - 1,
                client_request_id: format!("accepted-{sequence}"),
                request_sha256: "66".repeat(32),
                operation_id: operation.operation_id.clone(),
                task_attempt: operation.task_attempt.clone(),
                operation_sha256: operation.sha256()?,
                source_sha256: operation.source_sha256.clone(),
                actor_id: None,
                actor_generation: None,
                placement_lease_sha256: "77".repeat(32),
                placement_lease_nonce: placement_lease_nonce.to_owned(),
            },
        })
    }

    fn test_operation(operation_id: &str, source: &str) -> Result<PreparedOperationV2> {
        PreparedOperationV2::new(
            operation_id,
            TaskAttemptIdV1::new(
                SemanticDigestV1::hash_bytes("ostadix/store-test/task/v2", operation_id.as_bytes()),
                GenerationV1::new(1)?,
            ),
            source,
            BackendRegistry::global().catalog_sha256(),
            u64::MAX,
            4096,
        )
    }

    #[test]
    fn operation_write_is_exactly_idempotent_after_create_before_journal_crash() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([31; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"operation-idempotence")?;
        let operation = test_operation("durable-op", "bash^(printf '2')_bash")?;

        let expected_new_bytes = store.operation_new_bytes(&session_id, &operation)?;
        assert!(expected_new_bytes > 0);
        assert_eq!(
            store.write_operation(&session_id, &operation)?,
            expected_new_bytes
        );
        // Models a process dying after the immutable operation file was
        // fsynced but before OperationAccepted reached the session journal.
        assert_eq!(store.operation_new_bytes(&session_id, &operation)?, 0);
        assert_eq!(store.write_operation(&session_id, &operation)?, 0);

        let conflicting = test_operation("durable-op", "bash^(printf '3')_bash")?;
        let error = store
            .operation_new_bytes(&session_id, &conflicting)
            .expect_err("preflight must reject a conflicting content-addressed operation");
        assert!(
            format!("{error:#}").contains("different canonical bytes"),
            "{error:#}"
        );
        let error = store
            .write_operation(&session_id, &conflicting)
            .expect_err("one operation identity must never alias different canonical bytes");
        assert!(
            format!("{error:#}").contains("different canonical bytes"),
            "{error:#}"
        );
        assert_eq!(store.read_operation(&session_id, "durable-op")?, operation);
        Ok(())
    }

    #[test]
    fn exact_retry_finishes_published_blob_stage_and_returns_original_delta() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([75; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"blob-parent-sync")?;
        let operation = test_operation("published-stage-op", "bash^(printf 'stage')_bash")?;
        let frame = canonical_hosted_frame(&operation)?;
        let final_path = store.operation_path(&session_id, &operation.operation_id)?;
        let before = store.durable_bytes()?;
        let staged_path =
            store.stage_immutable_blob(&session_id, ImmutableBlobKindV2::Operation, &frame)?;
        fs::hard_link(&staged_path, &final_path)?;
        lock_mutex(
            &store.pending_blob_publications,
            "test pending blob publications",
        )?
        .insert(final_path.clone(), staged_path.clone());

        assert_eq!(
            store.write_operation(&session_id, &operation)?,
            frame.len() as u64
        );
        assert!(!staged_path.exists());
        assert!(lock_mutex(
            &store.pending_blob_publications,
            "test pending blob publications"
        )?
        .is_empty());
        assert_eq!(store.durable_bytes()? - before, frame.len() as u64);
        assert_eq!(store.write_operation(&session_id, &operation)?, 0);
        Ok(())
    }

    #[test]
    fn partial_operation_stage_is_reconciled_before_retry_publication() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([41; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"partial-operation")?;
        let operation = test_operation("partial-stage-op", "bash^(printf 'complete')_bash")?;
        let frame = canonical_hosted_frame(&operation)?;
        let staged = store.stage_immutable_blob(
            &session_id,
            ImmutableBlobKindV2::Operation,
            &frame[..frame.len() / 2],
        )?;

        assert!(staged.is_file());
        assert!(!store
            .operation_path(&session_id, &operation.operation_id)?
            .exists());
        drop(store); // injected crash after a durable partial stage write

        let reopened = DurableSessionStoreV2::open(&root, signer.clone())?;
        assert!(!staged.exists());
        assert!(reopened.write_operation(&session_id, &operation)? > 0);
        assert_eq!(reopened.write_operation(&session_id, &operation)?, 0);
        assert_eq!(
            fs::read(reopened.operation_path(&session_id, &operation.operation_id)?)?,
            frame
        );
        assert_eq!(
            reopened.read_operation(&session_id, &operation.operation_id)?,
            operation
        );
        Ok(())
    }

    #[test]
    fn complete_checkpoint_stage_before_publish_is_reconciled_and_retryable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([42; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"staged-checkpoint")?;
        let actor_generation_sha256 = "ab".repeat(32);
        let snapshot = EvaluatorStateSnapshotV1::new(Vec::new())?;
        let snapshot_payload_bytes = snapshot.encoded_len()? as u64;
        let frame = canonical_checkpoint_frame(&snapshot, snapshot_payload_bytes)?;
        let staged =
            store.stage_immutable_blob(&session_id, ImmutableBlobKindV2::Checkpoint, &frame)?;

        assert!(staged.is_file());
        assert!(!store
            .checkpoint_path(&session_id, &actor_generation_sha256)?
            .exists());
        drop(store); // injected crash after fsync, immediately before publication

        let reopened = DurableSessionStoreV2::open(&root, signer)?;
        assert!(!staged.exists());
        assert_eq!(
            reopened.checkpoint_new_bytes(
                &session_id,
                &actor_generation_sha256,
                &snapshot,
                snapshot_payload_bytes,
            )?,
            frame.len() as u64
        );
        assert_eq!(
            reopened.write_checkpoint(
                &session_id,
                &actor_generation_sha256,
                &snapshot,
                snapshot_payload_bytes,
            )?,
            frame.len() as u64
        );
        assert_eq!(
            reopened.write_checkpoint(
                &session_id,
                &actor_generation_sha256,
                &snapshot,
                snapshot_payload_bytes,
            )?,
            0
        );
        assert_eq!(
            fs::read(reopened.checkpoint_path(&session_id, &actor_generation_sha256)?)?,
            frame
        );
        assert_eq!(
            reopened.read_checkpoint(
                &session_id,
                &actor_generation_sha256,
                snapshot_payload_bytes,
            )?,
            snapshot
        );
        Ok(())
    }

    #[test]
    fn checkpoint_store_honors_authenticated_capacity_above_network_frame_limit() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([44; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"large-checkpoint")?;
        let checkpoint = BackendCheckpointV1::new(
            "python",
            BackendStateTierV1::SemanticSnapshot,
            "ostadix.store-test-large/v1",
            "11".repeat(32),
            serde_json::json!({
                "blob": "x".repeat(MAX_HOSTED_FRAME_BYTES + 128 * 1024),
            }),
            Vec::new(),
        )?;
        let actor =
            EvaluatorActorCheckpointV1::new("python", 7, Vec::new(), "22".repeat(32), checkpoint)?;
        let snapshot = EvaluatorStateSnapshotV1::new(vec![actor])?;
        let snapshot_payload_bytes = snapshot.encoded_len()? as u64;
        assert!(snapshot_payload_bytes > MAX_HOSTED_FRAME_BYTES as u64);
        let actor_generation_sha256 = "cd".repeat(32);

        let too_small = snapshot_payload_bytes - 1;
        let error = store
            .checkpoint_new_bytes(&session_id, &actor_generation_sha256, &snapshot, too_small)
            .expect_err("an authenticated bound one byte too small must reject the snapshot");
        assert!(
            format!("{error:#}").contains("exceeds authenticated limit"),
            "{error:#}"
        );

        let frame_bytes = snapshot_payload_bytes + 4;
        assert_eq!(
            store.checkpoint_new_bytes(
                &session_id,
                &actor_generation_sha256,
                &snapshot,
                snapshot_payload_bytes,
            )?,
            frame_bytes
        );
        assert_eq!(
            store.write_checkpoint(
                &session_id,
                &actor_generation_sha256,
                &snapshot,
                snapshot_payload_bytes,
            )?,
            frame_bytes
        );
        assert_eq!(
            store.write_checkpoint(
                &session_id,
                &actor_generation_sha256,
                &snapshot,
                snapshot_payload_bytes,
            )?,
            0
        );
        assert_eq!(
            store.read_checkpoint(
                &session_id,
                &actor_generation_sha256,
                snapshot_payload_bytes,
            )?,
            snapshot
        );
        let error = store
            .read_checkpoint(&session_id, &actor_generation_sha256, too_small)
            .expect_err("signed snapshot length mismatch must fail before payload allocation");
        assert!(
            format!("{error:#}").contains("does not match signed expectation"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn corrupt_published_immutable_blob_is_never_replaced_on_retry() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([43; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"corrupt-operation")?;
        let operation = test_operation("corrupt-final-op", "bash^(printf 'original')_bash")?;
        store.write_operation(&session_id, &operation)?;
        let path = store.operation_path(&session_id, &operation.operation_id)?;

        let mut file = OpenOptions::new().append(true).open(&path)?;
        file.write_all(b"corrupt-tail")?;
        file.sync_all()?;
        let corrupt = fs::read(&path)?;

        let error = store
            .write_operation(&session_id, &operation)
            .expect_err("a corrupt final blob must fail closed instead of being overwritten");
        assert!(
            format!("{error:#}").contains("different canonical bytes"),
            "{error:#}"
        );
        assert_eq!(fs::read(path)?, corrupt);
        assert_eq!(fs::read_dir(&store.session_staging)?.count(), 0);
        Ok(())
    }

    #[test]
    fn startup_repairs_only_incomplete_length_and_payload_tails_with_signed_evidence() -> Result<()>
    {
        for (ordinal, truncate_payload) in [false, true].into_iter().enumerate() {
            let directory = tempfile::tempdir()?;
            let root = directory.path().join("state");
            let signer = HostedNodeSignerV2::from_secret_bytes([32 + ordinal as u8; 32]);
            let store = DurableSessionStoreV2::open(&root, signer.clone())?;
            let (session_id, opened) =
                install_test_session(&store, &signer, format!("torn-{ordinal}").as_bytes())?;
            let journal_path = store.journal_path(&session_id);
            let validated_bytes = fs::metadata(&journal_path)?.len();
            let tail = if truncate_payload {
                let next =
                    next_test_receipt(&signer, &session_id, 2, Some(opened.entry_sha256.clone()))?;
                let frame = canonical_hosted_frame(&next)?;
                frame[..frame.len() - 1].to_vec()
            } else {
                vec![0_u8, 1_u8]
            };
            drop(store);

            let mut file = OpenOptions::new().append(true).open(&journal_path)?;
            file.write_all(&tail)?;
            file.sync_all()?;
            let damaged_bytes = fs::metadata(&journal_path)?.len();

            let reopened = DurableSessionStoreV2::open(&root, signer)?;
            assert_eq!(fs::metadata(&journal_path)?.len(), validated_bytes);
            let journal = reopened.read_journal(&session_id)?;
            assert!(journal.corruption.is_none(), "{:?}", journal.corruption);
            assert_eq!(journal.entries, vec![opened.clone()]);
            let authority = reopened.read_authority_journal()?;
            let repair = authority
                .entries
                .last()
                .context("repair must leave signed authority evidence")?;
            repair.verify()?;
            let repair_bytes = canonical_hosted_frame(repair)?.len() as u64;
            assert_eq!(
                reopened.remaining_authority_control_headroom_bytes(),
                CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 - repair_bytes
            );
            match &repair.entry.event {
                JournalEventV2::JournalTailRepaired {
                    journal_id,
                    old_bytes,
                    new_bytes,
                    recovered_head_sha256,
                } => {
                    assert_eq!(journal_id, &session_id);
                    assert_eq!(*old_bytes, damaged_bytes);
                    assert_eq!(*new_bytes, validated_bytes);
                    assert_eq!(recovered_head_sha256.as_ref(), Some(&opened.entry_sha256));
                }
                other => bail!("unexpected repair evidence: {other:?}"),
            }
        }
        Ok(())
    }

    #[test]
    fn journal_tail_repair_refuses_to_write_beyond_durable_control_headroom() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([33; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        store.authority_control_bytes.store(
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 - 1,
            Ordering::Release,
        );
        let authority_path = store.authority_journal_path();
        let before = fs::metadata(&authority_path)?.len();
        let error = store
            .record_journal_tail_repair(
                &"ab".repeat(32),
                &TornJournalTailV2 {
                    old_bytes: 2,
                    new_bytes: 0,
                    recovered_head_sha256: None,
                },
            )
            .expect_err("repair evidence must fit the remaining durable control budget");
        assert!(format!("{error:#}").contains("cannot fit"), "{error:#}");
        assert_eq!(fs::metadata(authority_path)?.len(), before);
        assert!(store.read_authority_journal()?.entries.is_empty());
        Ok(())
    }

    #[test]
    fn ordinary_authority_history_does_not_consume_emergency_control_headroom() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([36; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let mut previous = None;
        for sequence in 1..=20_u64 {
            let entry = signer.issue_journal_entry(JournalEntryV2 {
                schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
                session_id: AUTHORITY_JOURNAL_ID_V2.to_owned(),
                sequence,
                previous_entry_sha256: previous,
                recorded_unix_ms: sequence,
                event: JournalEventV2::PlacementLeaseRefused {
                    state_session_sha256: "11".repeat(32),
                    placement_lease_sha256: "22".repeat(32),
                    placement_lease_nonce: format!("{sequence:064x}"),
                    hosted_command_sha256: "33".repeat(32),
                    code: "ordinary_refusal".to_owned(),
                    message: "x".repeat(1024),
                },
            })?;
            store.append_authority_entry(&entry)?;
            previous = Some(entry.entry_sha256);
        }
        assert!(
            fs::metadata(store.authority_journal_path())?.len()
                > CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        assert_eq!(
            store.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        drop(store);

        let reopened = DurableSessionStoreV2::open(root, signer)?;
        assert_eq!(reopened.read_authority_journal()?.entries.len(), 20);
        assert_eq!(
            reopened.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        Ok(())
    }

    #[test]
    fn near_total_restart_consumes_repair_bytes_from_reserved_headroom() -> Result<()> {
        const SIGNED_HARD_TOTAL: u64 = 1024 * 1024;

        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([37; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"near-total-repair")?;
        let ordinary_limit = SIGNED_HARD_TOTAL - CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2;
        let available = ordinary_limit
            .checked_sub(store.durable_bytes()?)
            .context("test fixture already exceeds ordinary admission capacity")?;

        let operation_for_length =
            |source_len: usize| test_operation("near-total-op", &"x".repeat(source_len));
        let mut lower = 0_usize;
        let mut upper = crate::hosted_remote::protocol::MAX_HOSTED_SOURCE_BYTES + 1;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            let operation = operation_for_length(middle)?;
            let frame_bytes = canonical_hosted_frame(&operation)?.len() as u64;
            if frame_bytes <= available {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        let source_len = lower
            .checked_sub(1)
            .context("ordinary admission capacity cannot fit a test operation")?;
        let operation = operation_for_length(source_len)?;
        store.write_operation(&session_id, &operation)?;
        let admitted_bytes = store.durable_bytes()?;
        assert!(admitted_bytes <= ordinary_limit);
        assert!(ordinary_limit - admitted_bytes < 8);

        let journal_path = store.journal_path(&session_id);
        drop(store);
        let mut journal = OpenOptions::new().append(true).open(&journal_path)?;
        journal.write_all(&[0, 1])?;
        journal.sync_all()?;
        drop(journal);

        let reopened = DurableSessionStoreV2::open(&root, signer.clone())?;
        let authority = reopened.read_authority_journal()?;
        let repair = authority
            .entries
            .last()
            .context("near-total restart must sign its tail repair")?;
        let repair_bytes = canonical_hosted_frame(repair)?.len() as u64;
        assert_eq!(
            reopened.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 - repair_bytes
        );
        let reconstructed_reserved = reopened
            .durable_bytes()?
            .checked_add(reopened.remaining_authority_control_headroom_bytes())
            .context("near-total reconstructed capacity overflow")?;
        assert_eq!(
            reconstructed_reserved,
            admitted_bytes + CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        assert!(reconstructed_reserved <= SIGNED_HARD_TOTAL);
        let durable_after_repair = reopened.durable_bytes()?;
        let remaining_after_repair = reopened.remaining_authority_control_headroom_bytes();
        drop(reopened);

        let clean_reopen = DurableSessionStoreV2::open(&root, signer)?;
        assert_eq!(clean_reopen.durable_bytes()?, durable_after_repair);
        assert_eq!(
            clean_reopen.remaining_authority_control_headroom_bytes(),
            remaining_after_repair
        );
        assert_eq!(
            clean_reopen
                .durable_bytes()?
                .checked_add(clean_reopen.remaining_authority_control_headroom_bytes())
                .context("clean-restart reconstructed capacity overflow")?,
            reconstructed_reserved
        );
        Ok(())
    }

    #[test]
    fn authority_journal_can_audit_its_own_incomplete_tail_repair() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([35; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let authority_path = store.authority_journal_path();
        drop(store);

        let mut file = OpenOptions::new().append(true).open(&authority_path)?;
        file.write_all(&[0, 0, 0])?;
        file.sync_all()?;

        let reopened = DurableSessionStoreV2::open(&root, signer)?;
        let authority = reopened.read_authority_journal()?;
        assert_eq!(authority.entries.len(), 1);
        let repair = &authority.entries[0];
        repair.verify()?;
        match &repair.entry.event {
            JournalEventV2::JournalTailRepaired {
                journal_id,
                old_bytes,
                new_bytes,
                recovered_head_sha256,
            } => {
                assert_eq!(journal_id, AUTHORITY_JOURNAL_ID_V2);
                assert_eq!(*old_bytes, 3);
                assert_eq!(*new_bytes, 0);
                assert!(recovered_head_sha256.is_none());
            }
            other => bail!("unexpected authority repair evidence: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn repair_evidence_is_rejected_from_a_session_journal() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([34; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let (session_id, opened) = install_test_session(&store, &signer, b"repair-location")?;
        let invalid = signer.issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: session_id.clone(),
            sequence: 2,
            previous_entry_sha256: Some(opened.entry_sha256.clone()),
            recorded_unix_ms: 2,
            event: JournalEventV2::JournalTailRepaired {
                journal_id: session_id.clone(),
                old_bytes: 2,
                new_bytes: 1,
                recovered_head_sha256: Some("aa".repeat(32)),
            },
        })?;
        let error = store
            .append_entry(&session_id, &invalid)
            .expect_err("repair evidence is authority-journal-only");
        assert!(
            format!("{error:#}").contains("placement-authority journal"),
            "{error:#}"
        );
        assert_eq!(store.read_journal(&session_id)?.entries, vec![opened]);
        Ok(())
    }

    #[test]
    fn complete_invalid_signed_frame_is_never_truncated_or_repaired() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([36; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, opened) = install_test_session(&store, &signer, b"complete-corrupt")?;
        let journal_path = store.journal_path(&session_id);
        let mut invalid =
            next_test_receipt(&signer, &session_id, 2, Some(opened.entry_sha256.clone()))?;
        invalid.signature.replace_range(
            ..1,
            if invalid.signature.starts_with('0') {
                "1"
            } else {
                "0"
            },
        );
        let invalid_frame = canonical_hosted_frame(&invalid)?;
        drop(store);

        let mut file = OpenOptions::new().append(true).open(&journal_path)?;
        file.write_all(&invalid_frame)?;
        file.sync_all()?;
        let corrupt_bytes = fs::metadata(&journal_path)?.len();
        let error = DurableSessionStoreV2::open(&root, signer)
            .expect_err("a complete invalid signature must fail closed");
        assert!(
            format!("{error:#}").contains("complete invalid signed frame"),
            "{error:#}"
        );
        assert_eq!(fs::metadata(&journal_path)?.len(), corrupt_bytes);
        Ok(())
    }

    #[test]
    fn complete_invalid_cbor_frame_is_never_truncated_or_repaired() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([38; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"complete-invalid-cbor")?;
        let journal_path = store.journal_path(&session_id);
        drop(store);

        // A fully present one-byte CBOR break marker is invalid data, not a
        // torn transport frame, so startup must preserve it for diagnosis.
        let invalid_frame = [0_u8, 0, 0, 1, 0xff];
        let mut file = OpenOptions::new().append(true).open(&journal_path)?;
        file.write_all(&invalid_frame)?;
        file.sync_all()?;
        let corrupt_bytes = fs::metadata(&journal_path)?.len();
        let error = DurableSessionStoreV2::open(&root, signer)
            .expect_err("a complete invalid CBOR frame must fail closed");
        assert!(
            format!("{error:#}").contains("complete invalid frame"),
            "{error:#}"
        );
        assert_eq!(fs::metadata(&journal_path)?.len(), corrupt_bytes);
        Ok(())
    }

    #[test]
    fn complete_hash_chain_discontinuity_is_never_truncated_or_repaired() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([39; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, _) = install_test_session(&store, &signer, b"complete-invalid-chain")?;
        let journal_path = store.journal_path(&session_id);
        let discontinuous = next_test_receipt(&signer, &session_id, 2, None)?;
        let invalid_frame = canonical_hosted_frame(&discontinuous)?;
        drop(store);

        let mut file = OpenOptions::new().append(true).open(&journal_path)?;
        file.write_all(&invalid_frame)?;
        file.sync_all()?;
        let corrupt_bytes = fs::metadata(&journal_path)?.len();
        let error = DurableSessionStoreV2::open(&root, signer)
            .expect_err("a complete hash-chain discontinuity must fail closed");
        assert!(
            format!("{error:#}").contains("hash-chain discontinuity"),
            "{error:#}"
        );
        assert_eq!(fs::metadata(&journal_path)?.len(), corrupt_bytes);
        Ok(())
    }

    #[test]
    fn journal_append_uses_validated_cached_head_without_rescanning_prefix() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([37; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let (session_id, opened) = install_test_session(&store, &signer, b"append-cache")?;
        let scans_before = store.validated_journal_scan_count();
        let mut previous = opened.entry_sha256;
        for sequence in 2..=66 {
            let entry = next_test_receipt(&signer, &session_id, sequence, Some(previous.clone()))?;
            store.append_entry(&session_id, &entry)?;
            previous = entry.entry_sha256;
        }
        assert_eq!(store.validated_journal_scan_count(), scans_before);
        let journal = store.read_journal(&session_id)?;
        assert!(journal.corruption.is_none());
        assert_eq!(journal.entries.len(), 66);
        assert_eq!(store.validated_journal_scan_count(), scans_before + 1);
        Ok(())
    }

    #[test]
    fn journal_append_reconciles_exact_tail_across_post_write_sync_failures() -> Result<()> {
        for (ordinal, injected) in [
            JournalAppendFaultPointV2::AfterWrite,
            JournalAppendFaultPointV2::AfterFileSync,
            JournalAppendFaultPointV2::AfterParentSync,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir()?;
            let signer = HostedNodeSignerV2::from_secret_bytes([70 + ordinal as u8; 32]);
            let store =
                DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
            let (session_id, opened) =
                install_test_session(&store, &signer, format!("append-{ordinal}").as_bytes())?;
            let entry =
                next_test_receipt(&signer, &session_id, 2, Some(opened.entry_sha256.clone()))?;
            let path = store.journal_path(&session_id);
            let cached = store.cached_journal(&session_id, &path)?;
            let mut head = lock_mutex(&cached.head, "test journal head")?;
            let mut fired = false;
            let written =
                store.append_journal_file_locked_inner(&path, &entry, &mut head, |point, _| {
                    if point == injected && !fired {
                        fired = true;
                        bail!("injected {point:?} failure")
                    }
                    Ok(())
                })?;
            assert!(fired);
            assert_eq!(written, store.encoded_frame_bytes(&entry)?);
            assert_eq!(head.next_sequence, 3);
            assert_eq!(
                head.head_sha256.as_deref(),
                Some(entry.entry_sha256.as_str())
            );
            drop(head);

            let next =
                next_test_receipt(&signer, &session_id, 3, Some(entry.entry_sha256.clone()))?;
            store.append_entry(&session_id, &next)?;
            let journal = store.read_journal(&session_id)?;
            assert!(journal.corruption.is_none());
            assert_eq!(journal.entries, vec![opened, entry, next]);
        }
        Ok(())
    }

    #[test]
    fn debug_append_fault_fails_requested_attempt_before_writing_bytes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([73; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let (session_id, opened) = install_test_session(&store, &signer, b"append-countdown")?;
        let second = next_test_receipt(&signer, &session_id, 2, Some(opened.entry_sha256.clone()))?;
        let third = next_test_receipt(&signer, &session_id, 3, Some(second.entry_sha256.clone()))?;
        store.inject_append_failure_after_successes_for_test(1)?;
        store.append_entry(&session_id, &second)?;
        let before = fs::metadata(store.journal_path(&session_id))?.len();
        let error = store
            .append_entry(&session_id, &third)
            .expect_err("the armed second append must fail before writing bytes");
        assert!(format!("{error:#}").contains("injected zero-byte"));
        assert_eq!(fs::metadata(store.journal_path(&session_id))?.len(), before);
        assert!(!store.is_reopen_required());
        store.append_entry(&session_id, &third)?;
        assert_eq!(
            store.read_journal(&session_id)?.entries,
            vec![opened, second, third]
        );
        Ok(())
    }

    #[test]
    fn unprovable_append_tail_returns_typed_reopen_required_error() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([74; 32]);
        let root = directory.path().join("state");
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, opened) = install_test_session(&store, &signer, b"append-ambiguous")?;
        let entry = next_test_receipt(&signer, &session_id, 2, Some(opened.entry_sha256))?;
        let path = store.journal_path(&session_id);
        let cached = store.cached_journal(&session_id, &path)?;
        let mut head = lock_mutex(&cached.head, "test journal head")?;
        let error = store
            .append_journal_file_locked_inner(&path, &entry, &mut head, |point, _| {
                if matches!(
                    point,
                    JournalAppendFaultPointV2::AfterWrite
                        | JournalAppendFaultPointV2::BeforeReconcileFileSync
                ) {
                    bail!("injected durability ambiguity")
                }
                Ok(())
            })
            .expect_err("an unprovable written tail must require reopening the store");
        assert!(
            error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{error:#}"
        );
        assert_eq!(head.next_sequence, 2);
        drop(head);
        assert!(store.is_reopen_required());

        let retry = next_test_receipt(
            &signer,
            &session_id,
            2,
            Some(entry.entry.previous_entry_sha256.clone().unwrap()),
        )?;
        let append_error = store
            .append_entry(&session_id, &retry)
            .expect_err("poisoned store must refuse later journal mutations");
        assert!(
            append_error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{append_error:#}"
        );
        let operation = test_operation("poisoned-op", "bash^(printf 'x')_bash")?;
        let blob_error = store
            .write_operation(&session_id, &operation)
            .expect_err("poisoned store must refuse immutable-blob mutations");
        assert!(
            blob_error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{blob_error:#}"
        );
        let fresh_state_session = StateSessionIdV2::new(
            "atomic-node",
            GenerationV1::new(1)?,
            SemanticDigestV1::hash_bytes("ostadix/store-test/session/v2", b"poisoned-install"),
        )?;
        let fresh_session_id = fresh_state_session.semantic_digest()?.to_string();
        let fresh_receipt = first_open_receipt(&signer, fresh_state_session)?;
        let install_error = store
            .install_session(&fresh_session_id, &fresh_receipt)
            .expect_err("poisoned store must refuse session publication");
        assert!(
            install_error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{install_error:#}"
        );
        let gc_error = store
            .authorize_closed_session_gc(&session_id)
            .expect_err("poisoned store must refuse authority mutations");
        assert!(
            gc_error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{gc_error:#}"
        );
        drop(store);

        let reopened = DurableSessionStoreV2::open(root, signer)?;
        assert!(!reopened.is_reopen_required());
        let journal = reopened.read_journal(&session_id)?;
        assert!(journal.corruption.is_none());
        assert_eq!(journal.entries.last(), Some(&entry));
        Ok(())
    }

    #[test]
    fn first_session_entry_is_not_visible_before_atomic_publish() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([21; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let state_session = StateSessionIdV2::new(
            "atomic-node",
            GenerationV1::new(1)?,
            SemanticDigestV1::hash_bytes("ostadix/store-test/session/v2", b"atomic"),
        )?;
        let session_id = state_session.semantic_digest()?.to_string();
        let receipt = first_open_receipt(&signer, state_session)?;
        let error = store
            .install_session_inner(&session_id, &receipt, |point, staged| {
                if point != SessionInstallFaultPointV2::BeforePublish {
                    return Ok(());
                }
                assert!(staged.join(JOURNAL_FILE).is_file());
                assert!(!store.session_directory(&session_id).exists());
                bail!("injected failure immediately before atomic publish")
            })
            .expect_err("injected pre-publish failure must abort installation");
        assert!(format!("{error:#}").contains("injected failure"));
        assert!(!store.session_directory(&session_id).exists());
        assert_eq!(fs::read_dir(&store.session_staging)?.count(), 0);
        Ok(())
    }

    #[test]
    fn failed_unpublished_staging_cleanup_poisons_all_later_mutations() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([79; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let state_session = StateSessionIdV2::new(
            "atomic-node",
            GenerationV1::new(1)?,
            SemanticDigestV1::hash_bytes("ostadix/store-test/session/v2", b"cleanup-poison"),
        )?;
        let session_id = state_session.semantic_digest()?.to_string();
        let receipt = first_open_receipt(&signer, state_session)?;
        let error = store
            .install_session_inner(&session_id, &receipt, |point, _| match point {
                SessionInstallFaultPointV2::BeforePublish => {
                    bail!("injected failure before publication")
                }
                SessionInstallFaultPointV2::BeforeUnpublishedCleanup => {
                    bail!("injected staging-removal failure")
                }
                _ => Ok(()),
            })
            .expect_err("unprovable cleanup must poison the store");
        assert!(
            error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{error:#}"
        );
        assert!(store.is_reopen_required());
        assert_eq!(fs::read_dir(&store.session_staging)?.count(), 1);

        let operation = test_operation("cleanup-poison-op", "bash^(printf 'x')_bash")?;
        let blob_error = store
            .write_operation(&session_id, &operation)
            .expect_err("poisoned store must refuse later blob publication");
        assert!(
            blob_error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{blob_error:#}"
        );
        let install_error = store
            .install_session(&session_id, &receipt)
            .expect_err("poisoned store must refuse later session publication");
        assert!(
            install_error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{install_error:#}"
        );
        let gc_error = store
            .authorize_closed_session_gc(&session_id)
            .expect_err("poisoned store must refuse later authority mutation");
        assert!(
            gc_error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{gc_error:#}"
        );
        drop(store);

        let reopened = DurableSessionStoreV2::open(root, signer)?;
        assert!(!reopened.is_reopen_required());
        assert_eq!(fs::read_dir(&reopened.session_staging)?.count(), 0);
        Ok(())
    }

    #[test]
    fn published_session_reconciles_each_post_rename_failure() -> Result<()> {
        for (ordinal, injected) in [
            SessionInstallFaultPointV2::AfterRename,
            SessionInstallFaultPointV2::AfterSessionsParentSync,
            SessionInstallFaultPointV2::AfterStagingParentSync,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir()?;
            let signer = HostedNodeSignerV2::from_secret_bytes([80 + ordinal as u8; 32]);
            let store =
                DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
            let state_session = StateSessionIdV2::new(
                "atomic-node",
                GenerationV1::new(1)?,
                SemanticDigestV1::hash_bytes(
                    "ostadix/store-test/session/v2",
                    format!("published-{ordinal}").as_bytes(),
                ),
            )?;
            let session_id = state_session.semantic_digest()?.to_string();
            let receipt = first_open_receipt(&signer, state_session)?;
            let mut fired = false;
            let written = store.install_session_inner(&session_id, &receipt, |point, _| {
                if point == injected && !fired {
                    fired = true;
                    bail!("injected {point:?} failure")
                }
                Ok(())
            })?;
            assert!(fired);
            assert_eq!(written, store.encoded_frame_bytes(&receipt)?);
            assert_eq!(store.read_journal(&session_id)?.entries, vec![receipt]);
        }
        Ok(())
    }

    #[test]
    fn exact_published_session_retry_resumes_in_process_and_ambiguity_poisons() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([84; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let state_session = StateSessionIdV2::new(
            "atomic-node",
            GenerationV1::new(1)?,
            SemanticDigestV1::hash_bytes("ostadix/store-test/session/v2", b"resume-final"),
        )?;
        let session_id = state_session.semantic_digest()?.to_string();
        let receipt = first_open_receipt(&signer, state_session)?;
        let error = store
            .install_session_inner(&session_id, &receipt, |point, _| {
                if matches!(
                    point,
                    SessionInstallFaultPointV2::AfterRename
                        | SessionInstallFaultPointV2::BeforePublishedReconcile
                ) {
                    bail!("injected post-rename ambiguity")
                }
                Ok(())
            })
            .expect_err("blocked post-rename reconciliation must require reopen");
        assert!(
            error
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{error:#}"
        );
        assert!(store.session_directory(&session_id).is_dir());
        assert!(!store.journal_is_registered(&session_id)?);
        assert!(store.is_reopen_required());
        let retry = store
            .install_session(&session_id, &receipt)
            .expect_err("poisoned store must refuse exact publication retry until reopen");
        assert!(
            retry
                .downcast_ref::<DurableStoreReopenRequiredV2>()
                .is_some(),
            "{retry:#}"
        );
        drop(store);

        let reopened = DurableSessionStoreV2::open(directory.path().join("state"), signer)?;
        assert!(!reopened.is_reopen_required());
        assert_eq!(reopened.read_journal(&session_id)?.entries, vec![receipt]);
        Ok(())
    }

    #[test]
    fn exact_unregistered_final_session_is_resumed_in_process() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let signer = HostedNodeSignerV2::from_secret_bytes([85; 32]);
        let store = DurableSessionStoreV2::open(directory.path().join("state"), signer.clone())?;
        let state_session = StateSessionIdV2::new(
            "atomic-node",
            GenerationV1::new(1)?,
            SemanticDigestV1::hash_bytes("ostadix/store-test/session/v2", b"resume-exact"),
        )?;
        let session_id = state_session.semantic_digest()?.to_string();
        let receipt = first_open_receipt(&signer, state_session)?;
        let final_directory = store.session_directory(&session_id);
        create_private_directory_new(&final_directory)?;
        create_private_directory_new(&final_directory.join(OPERATIONS_DIRECTORY))?;
        create_private_directory_new(&final_directory.join(CHECKPOINTS_DIRECTORY))?;
        let journal_path = final_directory.join(JOURNAL_FILE);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut journal = options.open(&journal_path)?;
        write_hosted_frame(&mut journal, &receipt)?;
        journal.sync_all()?;

        let written = store.install_session(&session_id, &receipt)?;
        assert_eq!(written, fs::metadata(journal_path)?.len());
        assert!(store.journal_is_registered(&session_id)?);
        assert_eq!(store.read_journal(&session_id)?.entries, vec![receipt]);
        Ok(())
    }

    #[test]
    fn exclusive_open_reclaims_only_strict_unpublished_staging_directory() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        ensure_private_directory_v2(&root)?;
        let staging = root.join(SESSION_STAGING_DIRECTORY);
        ensure_private_directory_v2(&staging)?;
        let stale = staging.join(format!("install-{}-{}", "ab".repeat(32), "cd".repeat(16)));
        create_private_directory_new(&stale)?;
        fs::write(stale.join("partial"), b"crashed before publish")?;

        let store =
            DurableSessionStoreV2::open(&root, HostedNodeSignerV2::from_secret_bytes([22; 32]))?;
        assert!(!stale.exists());
        assert_eq!(fs::read_dir(&store.session_staging)?.count(), 0);
        drop(store);

        create_private_directory_new(&staging.join("unknown"))?;
        let error =
            DurableSessionStoreV2::open(&root, HostedNodeSignerV2::from_secret_bytes([22; 32]))
                .expect_err("unknown staging entries must not be deleted by reconciliation");
        assert!(format!("{error:#}").contains("unknown entry"));
        assert!(staging.join("unknown").is_dir());
        Ok(())
    }

    #[test]
    fn gc_completion_retries_archive_and_source_deletion_parent_barriers() -> Result<()> {
        for (ordinal, injected) in [
            ClosedSessionGcFaultPointV2::JournalRenamePublished,
            ClosedSessionGcFaultPointV2::DuplicateSourceUnlinked,
            ClosedSessionGcFaultPointV2::SessionDirectoryUnlinked,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir()?;
            let root = directory.path().join("state");
            let signer = HostedNodeSignerV2::from_secret_bytes([47 + ordinal as u8; 32]);
            let store = DurableSessionStoreV2::open(&root, signer.clone())?;
            let session_id = install_closed_test_session(
                &store,
                &signer,
                format!("gc-barrier-{ordinal}").as_bytes(),
            )?;
            store.authorize_closed_session_gc(&session_id)?;
            let pending = store
                .pending_closed_session_gc()?
                .context("test GC authorization must remain pending")?;
            let source = store.journal_path(&session_id);
            let archive = store.gc_archive_path(&session_id)?;

            if injected == ClosedSessionGcFaultPointV2::DuplicateSourceUnlinked {
                store.retain_closed_session_gc_journal(&pending)?;
                fs::hard_link(&archive, &source)?;
                sync_directory(source.parent().context("source has no parent")?)?;
                sync_directory(&store.gc_tombstones)?;
                assert!(source.is_file() && archive.is_file());
            }

            let mut fired = false;
            let error = store
                .complete_authorized_closed_session_gc_inner(&session_id, |point, _| {
                    if point == injected && !fired {
                        fired = true;
                        bail!("injected {point:?} barrier failure")
                    }
                    Ok(())
                })
                .expect_err("injected GC parent barrier must interrupt completion");
            assert!(fired);
            assert!(format!("{error:#}").contains("injected"), "{error:#}");
            assert!(archive.is_file());
            assert!(!path_exists(&source)?);
            if injected == ClosedSessionGcFaultPointV2::SessionDirectoryUnlinked {
                assert!(!path_exists(&store.session_directory(&session_id))?);
                assert!(store.journal_is_registered(&session_id)?);
            } else {
                assert!(store.session_directory(&session_id).is_dir());
            }
            assert!(matches!(
                store
                    .read_authority_journal()?
                    .entries
                    .last()
                    .map(|entry| &entry.entry.event),
                Some(JournalEventV2::ClosedSessionGcAuthorized { .. })
            ));

            let completed = store.complete_authorized_closed_session_gc(&session_id)?;
            assert!(matches!(
                completed.entry.event,
                JournalEventV2::ClosedSessionGcCompleted { .. }
            ));
            assert!(!path_exists(&store.session_directory(&session_id))?);
            assert!(!store.journal_is_registered(&session_id)?);
            assert!(archive.is_file());
        }
        Ok(())
    }

    #[test]
    fn reclaiming_gc_cycles_recycle_emergency_headroom_across_clean_restart() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([50; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;

        for cycle in 0..12_u8 {
            let session_id = install_closed_test_session(
                &store,
                &signer,
                format!("reclaiming-gc-{cycle}").as_bytes(),
            )?;
            let filler_path = store
                .session_directory(&session_id)
                .join(OPERATIONS_DIRECTORY)
                .join("reclaimed-filler");
            let mut filler = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&filler_path)?;
            filler.write_all(&vec![cycle; 4096])?;
            filler.sync_all()?;
            sync_directory(filler_path.parent().context("filler has no parent")?)?;
            store.gc_closed_session(&session_id)?;
            assert_eq!(
                store.remaining_authority_control_headroom_bytes(),
                CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
            );
        }
        assert!(
            fs::metadata(store.authority_journal_path())?.len()
                > CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        drop(store);

        let reopened = DurableSessionStoreV2::open(&root, signer.clone())?;
        assert_eq!(
            reopened.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        let next_session =
            install_closed_test_session(&reopened, &signer, b"reclaiming-gc-after-restart")?;
        let filler_path = reopened
            .session_directory(&next_session)
            .join(OPERATIONS_DIRECTORY)
            .join("reclaimed-filler");
        let mut filler = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&filler_path)?;
        filler.write_all(&vec![0_u8; 4096])?;
        filler.sync_all()?;
        sync_directory(filler_path.parent().context("filler has no parent")?)?;
        reopened.gc_closed_session(&next_session)?;
        assert_eq!(
            reopened.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        Ok(())
    }

    #[test]
    fn zero_reclaim_gc_cycles_exhaust_emergency_headroom_and_stay_exhausted() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([51; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let mut completed = 0_u8;
        let blocked_session = loop {
            let session_id = install_closed_test_session(
                &store,
                &signer,
                format!("zero-reclaim-gc-{completed}").as_bytes(),
            )?;
            match store.gc_closed_session(&session_id) {
                Ok(_) => completed = completed.checked_add(1).context("test cycle overflow")?,
                Err(error) => {
                    assert!(
                        format!("{error:#}").contains("authority-control headroom"),
                        "{error:#}"
                    );
                    break session_id;
                }
            }
            assert!(completed < 32, "zero-reclaim GC never exhausted headroom");
        };
        assert!(completed > 0);
        let remaining = store.remaining_authority_control_headroom_bytes();
        assert!(remaining < CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2);
        drop(store);

        let reopened = DurableSessionStoreV2::open(root, signer)?;
        assert_eq!(
            reopened.remaining_authority_control_headroom_bytes(),
            remaining
        );
        let error = reopened
            .gc_closed_session(&blocked_session)
            .expect_err("a clean restart must not replenish zero-reclaim GC headroom");
        assert!(
            format!("{error:#}").contains("authority-control headroom"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn gc_retains_signed_nonce_journal_across_delete_restart_and_quota_boundary() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let signer = HostedNodeSignerV2::from_secret_bytes([52; 32]);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        let (session_id, opened) = install_test_session(&store, &signer, b"gc-archive")?;
        let accepted = accepted_test_receipt(
            &signer,
            &session_id,
            2,
            opened.entry_sha256.clone(),
            &"11".repeat(32),
        )?;
        store.append_entry(&session_id, &accepted)?;
        let closed =
            next_test_receipt(&signer, &session_id, 3, Some(accepted.entry_sha256.clone()))?;
        store.append_entry(&session_id, &closed)?;

        // Fill ordinary state capacity right up to the GC control-headroom
        // boundary. Authorization and completion must remain reachable.
        let hard_total = 1024 * 1024_u64;
        let ordinary_limit = hard_total - CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2;
        let before_fill = store.durable_bytes()?;
        let filler_bytes = ordinary_limit
            .checked_sub(before_fill)
            .context("test fixture unexpectedly exceeds ordinary state capacity")?;
        let filler_path = store
            .session_directory(&session_id)
            .join(OPERATIONS_DIRECTORY)
            .join("quota-filler");
        let mut filler = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&filler_path)?;
        filler.write_all(&vec![0_u8; filler_bytes as usize])?;
        filler.sync_all()?;
        sync_directory(filler_path.parent().context("filler has no parent")?)?;
        assert_eq!(store.durable_bytes()?, ordinary_limit);

        let live_journal_path = store.journal_path(&session_id);
        let retained_bytes = fs::metadata(&live_journal_path)?.len();
        let authorization = store.authorize_closed_session_gc(&session_id)?;
        let authorization_bytes = canonical_hosted_frame(&authorization)?.len() as u64;
        assert_eq!(
            store.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 - authorization_bytes
        );
        let (terminal_journal_head_sha256, expected_reclaimed_bytes, retained_journal_sha256) =
            match &authorization.entry.event {
                JournalEventV2::ClosedSessionGcAuthorized {
                    session_id: retired,
                    terminal_journal_head_sha256,
                    expected_reclaimed_bytes,
                    retained_journal_sha256,
                    retained_journal_bytes,
                } => {
                    assert_eq!(retired, &session_id);
                    assert_eq!(terminal_journal_head_sha256, &closed.entry_sha256);
                    assert_eq!(*retained_journal_bytes, retained_bytes);
                    (
                        terminal_journal_head_sha256.clone(),
                        *expected_reclaimed_bytes,
                        retained_journal_sha256.clone(),
                    )
                }
                other => bail!("unexpected GC authorization: {other:?}"),
            };
        assert_eq!(expected_reclaimed_bytes, filler_bytes);
        assert!(store.durable_bytes()? <= hard_total);

        // Models a crash after Signed GC Authorized but before journal move.
        drop(store);
        let store = DurableSessionStoreV2::open(&root, signer.clone())?;
        assert_eq!(
            store.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2 - authorization_bytes
        );
        assert!(live_journal_path.is_file());
        let completion = store.complete_authorized_closed_session_gc(&session_id)?;
        assert!(!store.session_directory(&session_id).exists());
        let archive_path = store.gc_archive_path(&session_id)?;
        assert!(archive_path.is_file());
        assert_eq!(fs::metadata(&archive_path)?.len(), retained_bytes);
        assert_eq!(
            regular_file_sha256(&archive_path)?.1,
            retained_journal_sha256
        );
        assert!(store.durable_bytes()? <= hard_total);
        assert!(matches!(
            completion.entry.event,
            JournalEventV2::ClosedSessionGcCompleted {
                reclaimed_bytes,
                ..
            } if reclaimed_bytes == filler_bytes
        ));
        let durable_authority_bytes = fs::metadata(store.authority_journal_path())?.len();
        assert!(durable_authority_bytes > 0);
        assert_eq!(
            store.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );

        let archived = store.read_closed_session_gc_archive(&authorization.entry.event)?;
        assert_eq!(archived.entries, vec![opened.clone(), accepted, closed]);
        let consumed_nonces = archived
            .entries
            .iter()
            .filter_map(|entry| entry.entry.event.placement_lease_nonce().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            consumed_nonces,
            BTreeSet::from(["11".repeat(32), "55".repeat(32)])
        );
        drop(store);

        let reopened = DurableSessionStoreV2::open(&root, signer)?;
        assert_eq!(
            reopened.remaining_authority_control_headroom_bytes(),
            CLOSED_SESSION_GC_AUTHORITY_HEADROOM_BYTES_V2
        );
        assert!(reopened.list_session_ids()?.is_empty());
        let authority = reopened.read_authority_journal()?;
        let persisted_authorization = authority
            .entries
            .iter()
            .find(|entry| entry.entry.event.retired_session_id() == Some(session_id.as_str()))
            .context("retired session authorization disappeared after restart")?;
        let persisted =
            reopened.read_closed_session_gc_archive(&persisted_authorization.entry.event)?;
        assert_eq!(persisted.entries.len(), 3);
        let resurrection = reopened
            .install_session(&session_id, &opened)
            .expect_err("a retired session identity must never be installed again");
        assert!(format!("{resurrection:#}").contains("resurrect"));
        assert_eq!(
            terminal_journal_head_sha256,
            persisted.entries[2].entry_sha256
        );
        Ok(())
    }

    #[test]
    fn corrupt_or_missing_authorized_gc_archive_fails_restart_closed() -> Result<()> {
        for remove_archive in [false, true] {
            let directory = tempfile::tempdir()?;
            let root = directory.path().join("state");
            let signer =
                HostedNodeSignerV2::from_secret_bytes([if remove_archive { 54 } else { 53 }; 32]);
            let store = DurableSessionStoreV2::open(&root, signer.clone())?;
            let (session_id, opened) =
                install_test_session(&store, &signer, b"gc-corrupt-archive")?;
            let closed = next_test_receipt(&signer, &session_id, 2, Some(opened.entry_sha256))?;
            store.append_entry(&session_id, &closed)?;
            store.gc_closed_session(&session_id)?;
            let archive = store.gc_archive_path(&session_id)?;
            drop(store);

            if remove_archive {
                fs::remove_file(&archive)?;
            } else {
                let mut file = OpenOptions::new().append(true).open(&archive)?;
                file.write_all(b"corrupt")?;
                file.sync_all()?;
            }
            sync_directory(archive.parent().context("archive has no parent")?)?;
            let error = DurableSessionStoreV2::open(&root, signer)
                .expect_err("authorized missing/corrupt archive must fail startup closed");
            let message = format!("{error:#}");
            assert!(
                message.contains("no retained journal")
                    || message.contains("do not match signed authorization"),
                "{message}"
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn session_directory_symlink_is_never_a_gc_target() -> Result<()> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let store = DurableSessionStoreV2::open(
            directory.path().join("state"),
            HostedNodeSignerV2::from_secret_bytes([13; 32]),
        )?;
        let session_id = "ab".repeat(32);
        let outside = directory.path().join("outside");
        fs::create_dir(&outside)?;
        symlink(&outside, store.sessions.join(&session_id))?;
        let error = store
            .gc_closed_session(&session_id)
            .expect_err("GC must reject a symlink even when its name is a valid session digest");
        assert!(format!("{error:#}").contains("not a real session directory"));
        assert!(outside.is_dir());
        Ok(())
    }

    #[test]
    fn state_lock_reopens_after_abrupt_child_exit() -> Result<()> {
        const CHILD_FLAG: &str = "OSTADIX_V2_LOCK_CRASH_CHILD";
        const ROOT_FLAG: &str = "OSTADIX_V2_LOCK_CRASH_ROOT";
        if env::var_os(CHILD_FLAG).is_some() {
            let root = PathBuf::from(env::var_os(ROOT_FLAG).context("child lock root missing")?);
            let _store =
                DurableSessionStoreV2::open(root, HostedNodeSignerV2::from_secret_bytes([15; 32]))?;
            std::process::abort();
        }

        let directory = tempfile::tempdir()?;
        let root = directory.path().join("state");
        let status = std::process::Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("hosted_remote::v2::store::tests::state_lock_reopens_after_abrupt_child_exit")
            .arg("--nocapture")
            .env(CHILD_FLAG, "1")
            .env(ROOT_FLAG, &root)
            .status()?;
        assert!(
            !status.success(),
            "crash child unexpectedly returned normally"
        );

        let reopened =
            DurableSessionStoreV2::open(&root, HostedNodeSignerV2::from_secret_bytes([15; 32]))?;
        drop(reopened);
        Ok(())
    }
}
