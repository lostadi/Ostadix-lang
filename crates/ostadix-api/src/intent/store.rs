//! Crash-durable, private storage for [`RunRecordV1`].
//!
//! Global locking is limited to allocation, publication, reconciliation, and
//! retention.  The returned [`RunLeaseV1`] holds a per-run advisory lock while
//! computation proceeds, so unrelated invocations can begin and finish
//! concurrently.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::record::{
    validate_lower_hex_64, RunAttemptSeedV1, RunContentKindV1, RunContentRefV1, RunRecordV1,
    RunTraceAttachmentV1, RunTraceBindingV1,
};

pub const MAX_RUN_ATTEMPTS_V1: usize = 128;
pub const MAX_RUN_OBJECT_BYTES_V1: u64 = 256 * 1024 * 1024;

const ATTEMPT_INDEX_SCHEMA_V1: &str = "ostadix.run-attempt-index/v1";
const SEQUENCE_SCHEMA_V1: &str = "ostadix.run-sequence/v1";
const MAX_INDEX_BYTES: usize = 256 * 1024;
const MAX_SEQUENCE_BYTES: usize = 4 * 1024;
const MAX_DECODE_ITEMS: usize = 8_000_000;
const MAX_DECODE_DEPTH: usize = 128;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum RunStoreErrorV1 {
    #[error("run store I/O failure at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid run store observation: {0}")]
    Invalid(String),
    #[error("run `{0}` was not found")]
    NotFound(String),
    #[error("run `{run_id}` is still executing")]
    StillRunning { run_id: String },
    #[error(
        "run store has {active} active attempts; refusing to allocate attempt {}",
        MAX_RUN_ATTEMPTS_V1 + 1
    )]
    ActiveCapacity { active: usize },
    #[error("run-record retention cannot fit {required_bytes} bytes within the {maximum_bytes}-byte bound")]
    ByteCapacity {
        required_bytes: u64,
        maximum_bytes: u64,
    },
    #[error(
        "run `{run_id}` computation may already have occurred, but final recording failed: {detail}; fallback observation: {fallback}"
    )]
    FinalizationIncomplete {
        run_id: String,
        detail: String,
        fallback: String,
    },
    #[error("run store location is unavailable: set XDG_STATE_HOME or HOME")]
    DefaultLocationUnavailable,
}

impl RunStoreErrorV1 {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Default private history location.
pub fn default_run_store_root() -> Result<PathBuf, RunStoreErrorV1> {
    default_run_store_root_from(env::var_os("XDG_STATE_HOME"), env::var_os("HOME"))
}

fn default_run_store_root_from(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, RunStoreErrorV1> {
    if let Some(root) = xdg_state_home.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(root).join("ostadix").join("runs-v1"));
    }
    if let Some(home) = home.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("ostadix")
            .join("runs-v1"));
    }
    Err(RunStoreErrorV1::DefaultLocationUnavailable)
}

#[derive(Clone, Debug)]
pub struct RunStoreV1 {
    root: PathBuf,
}

impl RunStoreV1 {
    pub fn open_default() -> Result<Self, RunStoreErrorV1> {
        let root = default_run_store_root()?;
        let state_root = root.parent().ok_or_else(|| {
            RunStoreErrorV1::Invalid("default run store has no state parent".to_string())
        })?;
        ensure_private_directory(state_root)?;
        Self::open_at(root)
    }

    /// Open or create one writer-capable store rooted directly at `runs-v1`.
    /// A writer open reconciles released running leases; read-only inspection
    /// uses [`RunStoreReaderV1`] and never performs this repair.
    pub fn open_at(root: impl AsRef<Path>) -> Result<Self, RunStoreErrorV1> {
        let root = root.as_ref().to_path_buf();
        ensure_layout(&root)?;
        with_global_lock(&root, || {
            cleanup_stale_temporary_files_locked(&root)?;
            repair_sequence_floor_locked(&root)?;
            reconcile_orphans_locked(&root)?;
            prune_locked(&root, 0, &[], None)?;
            cleanup_unreferenced_objects_locked(&root)
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Allocate a random run id and monotonic sequence after the caller has
    /// completed exact preflight.  The running index is durable before this
    /// function returns.
    pub fn begin(&self, seed: RunAttemptSeedV1) -> Result<RunLeaseV1, RunStoreErrorV1> {
        seed.validate().map_err(RunStoreErrorV1::Invalid)?;
        with_global_lock(&self.root, || {
            let sequence = read_sequence(&self.root)?
                .checked_add(1)
                .ok_or_else(|| RunStoreErrorV1::Invalid("run sequence overflow".to_string()))?;
            // Every random run id has the same encoded width as this probe.
            // Reject an index the reader cannot later load before maintenance,
            // retention, sequence advancement, or lease creation mutates the
            // store.
            let prospective = AttemptIndexV1 {
                schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
                run_id: "0".repeat(64),
                sequence,
                state: AttemptStateV1::Running {
                    seed: Box::new(seed.clone()),
                },
            };
            validate_attempt_index_storage_size(&prospective)?;

            reconcile_orphans_locked(&self.root)?;
            prune_locked(&self.root, 1, &[], None)?;

            let attempts = load_attempts(&self.root)?;
            let active = attempts
                .iter()
                .filter(|attempt| matches!(attempt.index.state, AttemptStateV1::Running { .. }))
                .count();
            if active >= MAX_RUN_ATTEMPTS_V1 {
                return Err(RunStoreErrorV1::ActiveCapacity { active });
            }

            let (run_id, lease_path, lease) = create_unique_lease(&self.root)?;
            if let Err(error) = FileExt::lock_exclusive(&lease) {
                let _ = fs::remove_file(&lease_path);
                return Err(RunStoreErrorV1::io(&lease_path, error));
            }

            let index = AttemptIndexV1 {
                schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
                run_id: run_id.clone(),
                sequence,
                state: AttemptStateV1::Running {
                    seed: Box::new(seed.clone()),
                },
            };
            let attempt_path = attempt_path(&self.root, &index);
            let result = (|| {
                write_sequence(&self.root, sequence)?;
                write_canonical_atomic(&self.root, &attempt_path, &index)?;
                sync_directory(&self.root.join("attempts"))
            })();
            if let Err(error) = result {
                let _ = FileExt::unlock(&lease);
                let _ = fs::remove_file(&lease_path);
                return Err(error);
            }
            Ok(RunLeaseV1 {
                root: self.root.clone(),
                attempt: RunAttemptV1 {
                    run_id,
                    sequence,
                    seed,
                },
                attempt_path,
                lease_path,
                lease: Some(lease),
                finalized: false,
            })
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunAttemptV1 {
    pub run_id: String,
    pub sequence: u64,
    pub seed: RunAttemptSeedV1,
}

/// Per-run lease held across execution.  Dropping it without finalization is
/// intentionally non-mutating; a later writer will observe the released lock
/// and publish an interrupted terminal observation.
pub struct RunLeaseV1 {
    root: PathBuf,
    attempt: RunAttemptV1,
    attempt_path: PathBuf,
    lease_path: PathBuf,
    lease: Option<File>,
    finalized: bool,
}

impl RunLeaseV1 {
    pub fn attempt(&self) -> &RunAttemptV1 {
        &self.attempt
    }

    /// Publish a terminal record and optional trace attachment.  If exact
    /// publication fails after computation, the store attempts to replace the
    /// running index with a small `recording_incomplete` observation and still
    /// returns an error so `--require-record` can fail closed without replay.
    pub fn finalize(
        mut self,
        mut record: RunRecordV1,
        trace: Option<RunTraceAttachmentV1>,
    ) -> Result<FinalizedRunV1, RunStoreErrorV1> {
        let exact = self.finalize_exact(&mut record, trace);
        match exact {
            Ok(finalized) => {
                self.release_after_finalization();
                Ok(finalized)
            }
            Err(error) => {
                let detail = error.to_string();
                let fallback = self.finalize_incomplete(&detail);
                let fallback_detail = match fallback {
                    Ok(reference) => format!("recorded as {}", reference.sha256),
                    Err(fallback_error) => format!("also failed: {fallback_error}"),
                };
                // Release even if the fallback failed.  A later writer may
                // then reconcile the still-running index as interrupted.
                self.release_after_finalization();
                Err(RunStoreErrorV1::FinalizationIncomplete {
                    run_id: self.attempt.run_id.clone(),
                    detail,
                    fallback: fallback_detail,
                })
            }
        }
    }

    fn finalize_exact(
        &mut self,
        record: &mut RunRecordV1,
        trace: Option<RunTraceAttachmentV1>,
    ) -> Result<FinalizedRunV1, RunStoreErrorV1> {
        validate_record_matches_attempt(record, &self.attempt)?;

        let mut staged = Vec::new();
        if let Some(trace) = trace {
            trace.validate().map_err(RunStoreErrorV1::Invalid)?;
            let bytes = canonical_bytes(&trace)?;
            let object = stage_object(
                &self.root,
                &self.attempt.run_id,
                RunContentKindV1::Trace,
                bytes,
            )?;
            record.trace = RunTraceBindingV1::Attached {
                object: object.reference.clone(),
            };
            trace
                .validate_for_record(record)
                .map_err(RunStoreErrorV1::Invalid)?;
            staged.push(object);
        } else if matches!(record.trace, RunTraceBindingV1::Attached { .. }) {
            return Err(RunStoreErrorV1::Invalid(
                "record names a trace attachment but finalization supplied no trace".to_string(),
            ));
        }
        record.validate().map_err(RunStoreErrorV1::Invalid)?;
        validate_record_matches_attempt(record, &self.attempt)?;

        let record_bytes = canonical_bytes(record)?;
        let record_object = stage_object(
            &self.root,
            &self.attempt.run_id,
            RunContentKindV1::Record,
            record_bytes,
        )?;
        let record_reference = record_object.reference.clone();
        staged.push(record_object);

        let references = staged
            .iter()
            .map(|object| object.reference.clone())
            .collect::<Vec<_>>();
        let result = with_global_lock(&self.root, || {
            require_running_attempt(&self.attempt_path, &self.attempt)?;
            prune_locked(
                &self.root,
                0,
                &references,
                Some(self.attempt.run_id.as_str()),
            )?;
            for object in &mut staged {
                publish_staged_object(&self.root, object)?;
            }
            let index = AttemptIndexV1 {
                schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
                run_id: self.attempt.run_id.clone(),
                sequence: self.attempt.sequence,
                state: AttemptStateV1::Terminal {
                    record: record_reference.clone(),
                    referenced_objects: references.clone(),
                },
            };
            write_canonical_atomic(&self.root, &self.attempt_path, &index)?;
            sync_directory(&self.root.join("attempts"))?;
            cleanup_unreferenced_objects_locked(&self.root)?;
            Ok(FinalizedRunV1 {
                run_id: self.attempt.run_id.clone(),
                sequence: self.attempt.sequence,
                record: record_reference.clone(),
            })
        });
        if result.is_err() {
            for object in &staged {
                let _ = fs::remove_file(&object.temporary);
            }
        }
        result
    }

    fn finalize_incomplete(&mut self, detail: &str) -> Result<RunContentRefV1, RunStoreErrorV1> {
        let record = RunRecordV1::recording_incomplete(
            self.attempt.run_id.clone(),
            self.attempt.sequence,
            &self.attempt.seed,
            unix_nanos_now()?,
            detail,
        );
        record.validate().map_err(RunStoreErrorV1::Invalid)?;
        let object = stage_object(
            &self.root,
            &self.attempt.run_id,
            RunContentKindV1::Record,
            canonical_bytes(&record)?,
        )?;
        let reference = object.reference.clone();
        let mut staged = [object];
        let result = with_global_lock(&self.root, || {
            require_running_attempt(&self.attempt_path, &self.attempt)?;
            prune_locked(
                &self.root,
                0,
                std::slice::from_ref(&reference),
                Some(self.attempt.run_id.as_str()),
            )?;
            publish_staged_object(&self.root, &mut staged[0])?;
            let index = AttemptIndexV1 {
                schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
                run_id: self.attempt.run_id.clone(),
                sequence: self.attempt.sequence,
                state: AttemptStateV1::Terminal {
                    record: reference.clone(),
                    referenced_objects: vec![reference.clone()],
                },
            };
            write_canonical_atomic(&self.root, &self.attempt_path, &index)?;
            sync_directory(&self.root.join("attempts"))?;
            cleanup_unreferenced_objects_locked(&self.root)?;
            Ok(reference.clone())
        });
        if result.is_err() {
            let _ = fs::remove_file(&staged[0].temporary);
        }
        result
    }

    fn release_after_finalization(&mut self) {
        if let Some(lease) = self.lease.take() {
            // Dropping the descriptor also releases the advisory lock, so a
            // platform-specific explicit-unlock failure cannot retroactively
            // turn an already durable terminal record into a failed finalize.
            let _ = FileExt::unlock(&lease);
        }
        if self.lease_path.exists()
            && reject_symlink(&self.lease_path).is_ok()
            && fs::remove_file(&self.lease_path).is_ok()
        {
            let _ = sync_directory(&self.root.join("leases"));
        }
        self.finalized = true;
    }
}

impl Drop for RunLeaseV1 {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            let _ = FileExt::unlock(&lease);
        }
        // Do not remove an unfinished lease file.  Its released advisory lock
        // is the durable orphan signal consumed by the next writer.
        if self.finalized && reject_symlink(&self.lease_path).is_ok() {
            let _ = fs::remove_file(&self.lease_path);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FinalizedRunV1 {
    pub run_id: String,
    pub sequence: u64,
    pub record: RunContentRefV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunSelectorV1 {
    LastRun,
    RunId(String),
}

#[derive(Clone, Debug)]
pub struct RunStoreReaderV1 {
    root: PathBuf,
}

impl RunStoreReaderV1 {
    pub fn open_default_existing() -> Result<Self, RunStoreErrorV1> {
        let root = default_run_store_root()?;
        let state_root = root.parent().ok_or_else(|| {
            RunStoreErrorV1::Invalid("default run store has no state parent".to_string())
        })?;
        validate_existing_private_directory(state_root)?;
        Self::open_existing(root)
    }

    /// Open a strictly read-only view.  This does not create directories, open
    /// a lock file, reconcile leases, repair permissions, prune, or update an
    /// access timestamp through an application-level write.
    pub fn open_existing(root: impl AsRef<Path>) -> Result<Self, RunStoreErrorV1> {
        let root = root.as_ref().to_path_buf();
        validate_existing_layout(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn inspect(
        &self,
        selector: RunSelectorV1,
        include_trace: bool,
    ) -> Result<RunInspectionV1, RunStoreErrorV1> {
        let attempts = load_attempts(&self.root)?;
        let stored = select_attempt(attempts, selector)?;
        match stored.index.state {
            AttemptStateV1::Running { seed } => Ok(RunInspectionV1::Running {
                attempt: Box::new(RunAttemptV1 {
                    run_id: stored.index.run_id,
                    sequence: stored.index.sequence,
                    seed: *seed,
                }),
            }),
            AttemptStateV1::Terminal {
                record,
                referenced_objects,
            } => {
                let record_bytes = read_object(&self.root, &record)?;
                let decoded: RunRecordV1 = decode_canonical(&record_bytes)?;
                decoded.validate().map_err(RunStoreErrorV1::Invalid)?;
                if decoded.run_id != stored.index.run_id
                    || decoded.sequence != stored.index.sequence
                {
                    return Err(RunStoreErrorV1::Invalid(
                        "terminal run record disagrees with its attempt index".to_string(),
                    ));
                }
                if !referenced_objects.contains(&record) {
                    return Err(RunStoreErrorV1::Invalid(
                        "attempt index omits its terminal record reference".to_string(),
                    ));
                }
                let expected_reference_count = match &decoded.trace {
                    RunTraceBindingV1::Attached { object } => {
                        if !referenced_objects.contains(object) {
                            return Err(RunStoreErrorV1::Invalid(
                                "attempt index omits the record's trace reference".to_string(),
                            ));
                        }
                        2
                    }
                    RunTraceBindingV1::Unavailable { .. } => 1,
                };
                if referenced_objects.len() != expected_reference_count {
                    return Err(RunStoreErrorV1::Invalid(
                        "attempt index contains objects not bound by its terminal record"
                            .to_string(),
                    ));
                }
                let trace = if include_trace {
                    match &decoded.trace {
                        RunTraceBindingV1::Attached { object } => {
                            let bytes = read_object(&self.root, object)?;
                            let trace: RunTraceAttachmentV1 = decode_canonical(&bytes)?;
                            trace
                                .validate_for_record(&decoded)
                                .map_err(RunStoreErrorV1::Invalid)?;
                            Some(trace)
                        }
                        RunTraceBindingV1::Unavailable { .. } => None,
                    }
                } else {
                    None
                };
                Ok(RunInspectionV1::Terminal {
                    record: Box::new(decoded),
                    trace: trace.map(Box::new),
                })
            }
        }
    }

    pub fn read_terminal(
        &self,
        selector: RunSelectorV1,
        include_trace: bool,
    ) -> Result<(RunRecordV1, Option<RunTraceAttachmentV1>), RunStoreErrorV1> {
        match self.inspect(selector, include_trace)? {
            RunInspectionV1::Running { attempt } => Err(RunStoreErrorV1::StillRunning {
                run_id: attempt.run_id,
            }),
            RunInspectionV1::Terminal { record, trace } => {
                Ok((*record, trace.map(|attachment| *attachment)))
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunInspectionV1 {
    Running {
        attempt: Box<RunAttemptV1>,
    },
    Terminal {
        record: Box<RunRecordV1>,
        #[serde(skip_serializing_if = "Option::is_none")]
        trace: Option<Box<RunTraceAttachmentV1>>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SequenceV1 {
    schema: String,
    greatest_allocated: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct AttemptIndexV1 {
    schema: String,
    run_id: String,
    sequence: u64,
    state: AttemptStateV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AttemptStateV1 {
    Running {
        seed: Box<RunAttemptSeedV1>,
    },
    Terminal {
        record: RunContentRefV1,
        referenced_objects: Vec<RunContentRefV1>,
    },
}

impl AttemptIndexV1 {
    fn validate(&self) -> Result<(), RunStoreErrorV1> {
        if self.schema != ATTEMPT_INDEX_SCHEMA_V1 {
            return Err(RunStoreErrorV1::Invalid(format!(
                "unsupported attempt-index schema `{}`",
                self.schema
            )));
        }
        validate_lower_hex_64(&self.run_id, "attempt run id").map_err(RunStoreErrorV1::Invalid)?;
        if self.sequence == 0 {
            return Err(RunStoreErrorV1::Invalid(
                "attempt sequence must be positive".to_string(),
            ));
        }
        match &self.state {
            AttemptStateV1::Running { seed } => {
                seed.validate().map_err(RunStoreErrorV1::Invalid)?;
            }
            AttemptStateV1::Terminal {
                record,
                referenced_objects,
            } => {
                record.validate().map_err(RunStoreErrorV1::Invalid)?;
                if record.kind != RunContentKindV1::Record {
                    return Err(RunStoreErrorV1::Invalid(
                        "attempt terminal pointer is not a record object".to_string(),
                    ));
                }
                if referenced_objects.is_empty() || !referenced_objects.contains(record) {
                    return Err(RunStoreErrorV1::Invalid(
                        "terminal attempt has an incomplete object inventory".to_string(),
                    ));
                }
                let mut unique = BTreeSet::new();
                for reference in referenced_objects {
                    reference.validate().map_err(RunStoreErrorV1::Invalid)?;
                    if !unique.insert((reference.kind, reference.sha256.as_str())) {
                        return Err(RunStoreErrorV1::Invalid(
                            "terminal attempt repeats an object reference".to_string(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_attempt_index_storage_size(index: &AttemptIndexV1) -> Result<(), RunStoreErrorV1> {
    let encoded_len = canonical_bytes(index)?.len();
    if encoded_len > MAX_INDEX_BYTES {
        return Err(RunStoreErrorV1::Invalid(format!(
            "running attempt index requires {encoded_len} bytes; maximum is {MAX_INDEX_BYTES}"
        )));
    }
    Ok(())
}

struct StoredAttempt {
    path: PathBuf,
    index: AttemptIndexV1,
}

struct StagedObject {
    temporary: PathBuf,
    reference: RunContentRefV1,
}

impl Drop for StagedObject {
    fn drop(&mut self) {
        // Covers every early-return boundary between staging the optional
        // trace and publishing the terminal attempt index.
        let _ = fs::remove_file(&self.temporary);
    }
}

fn ensure_layout(root: &Path) -> Result<(), RunStoreErrorV1> {
    ensure_private_directory(root)?;
    for relative in [
        "attempts",
        "objects",
        "objects/records",
        "objects/traces",
        "leases",
        "tmp",
    ] {
        ensure_private_directory(&root.join(relative))?;
    }
    Ok(())
}

/// Remove only temporary files whose owning transaction is provably absent.
///
/// Global temporaries are created while `store.lock` is held, so the caller's
/// global lock proves they are stale. Run-scoped temporaries may be staged
/// before finalization acquires the global lock; retain those while the
/// corresponding per-run lease is still locked by an executor.
fn cleanup_stale_temporary_files_locked(root: &Path) -> Result<(), RunStoreErrorV1> {
    let directory = root.join("tmp");
    let entries =
        fs::read_dir(&directory).map_err(|error| RunStoreErrorV1::io(&directory, error))?;
    let mut changed = false;

    for entry in entries {
        let entry = entry.map_err(|error| RunStoreErrorV1::io(&directory, error))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RunStoreErrorV1::Invalid("non-UTF-8 temporary filename".to_string()))?;
        reject_symlink(&path)?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| RunStoreErrorV1::io(&path, error))?;
        if !metadata.is_file() {
            return Err(RunStoreErrorV1::Invalid(format!(
                "{} is not a regular temporary file",
                path.display()
            )));
        }
        validate_private_permissions(&path, &metadata)?;

        let stale = if name.starts_with(".tmp-global-") {
            true
        } else if let Some(rest) = name.strip_prefix(".tmp-run-") {
            let (run_id, suffix) = rest.split_at_checked(64).ok_or_else(|| {
                RunStoreErrorV1::Invalid(format!("invalid run-scoped temporary name `{name}`"))
            })?;
            validate_lower_hex_64(run_id, "temporary owner run id")
                .map_err(RunStoreErrorV1::Invalid)?;
            if !suffix.starts_with('-') {
                return Err(RunStoreErrorV1::Invalid(format!(
                    "invalid run-scoped temporary name `{name}`"
                )));
            }

            let lease_path = root.join("leases").join(format!("{run_id}.lock"));
            match open_private_file(&lease_path, false, false) {
                Ok(lease) => match FileExt::try_lock_exclusive(&lease) {
                    Ok(()) => {
                        let _ = FileExt::unlock(&lease);
                        true
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => false,
                    Err(error) => return Err(RunStoreErrorV1::io(&lease_path, error)),
                },
                Err(RunStoreErrorV1::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
                    true
                }
                Err(error) => return Err(error),
            }
        } else {
            return Err(RunStoreErrorV1::Invalid(format!(
                "unrecognized run-store temporary `{name}`"
            )));
        };

        if stale {
            fs::remove_file(&path).map_err(|error| RunStoreErrorV1::io(&path, error))?;
            changed = true;
        }
    }

    if changed {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn validate_existing_layout(root: &Path) -> Result<(), RunStoreErrorV1> {
    validate_existing_private_directory(root)?;
    for relative in [
        "attempts",
        "objects",
        "objects/records",
        "objects/traces",
        "leases",
        "tmp",
    ] {
        validate_existing_private_directory(&root.join(relative))?;
    }
    Ok(())
}

fn with_global_lock<T>(
    root: &Path,
    operation: impl FnOnce() -> Result<T, RunStoreErrorV1>,
) -> Result<T, RunStoreErrorV1> {
    let path = root.join("store.lock");
    let lock = open_private_file(&path, true, false)?;
    FileExt::lock_exclusive(&lock).map_err(|error| RunStoreErrorV1::io(&path, error))?;
    let result = operation();
    // The descriptor's Drop is the authoritative unlock boundary. An
    // explicit unlock failure must not hide a transaction that already made
    // its durable mutation visible.
    let _ = FileExt::unlock(&lock);
    result
}

fn read_sequence(root: &Path) -> Result<u64, RunStoreErrorV1> {
    let path = root.join("sequence.cbor");
    let Some(bytes) = read_regular_file_if_exists(&path, MAX_SEQUENCE_BYTES)? else {
        return Ok(0);
    };
    let sequence: SequenceV1 = decode_canonical(&bytes)?;
    if sequence.schema != SEQUENCE_SCHEMA_V1 {
        return Err(RunStoreErrorV1::Invalid(format!(
            "unsupported run-sequence schema `{}`",
            sequence.schema
        )));
    }
    Ok(sequence.greatest_allocated)
}

fn write_sequence(root: &Path, greatest_allocated: u64) -> Result<(), RunStoreErrorV1> {
    write_canonical_atomic(
        root,
        &root.join("sequence.cbor"),
        &SequenceV1 {
            schema: SEQUENCE_SCHEMA_V1.to_string(),
            greatest_allocated,
        },
    )
}

/// Recover a missing or rolled-back sequence marker from durable attempts.
/// Attempt indices are the allocation evidence; the small sequence file is a
/// cache that must never move below their greatest observed sequence.
fn repair_sequence_floor_locked(root: &Path) -> Result<(), RunStoreErrorV1> {
    let greatest_attempt = load_attempts(root)?
        .iter()
        .map(|attempt| attempt.index.sequence)
        .max()
        .unwrap_or(0);
    if read_sequence(root)? < greatest_attempt {
        write_sequence(root, greatest_attempt)?;
    }
    Ok(())
}

fn create_unique_lease(root: &Path) -> Result<(String, PathBuf, File), RunStoreErrorV1> {
    for _ in 0..32 {
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|error| {
            RunStoreErrorV1::Invalid(format!("failed to allocate random run id: {error}"))
        })?;
        let run_id = hex::encode(random);
        let path = root.join("leases").join(format!("{run_id}.lock"));
        match open_private_file(&path, true, true) {
            Ok(file) => return Ok((run_id, path, file)),
            Err(RunStoreErrorV1::Io { source, .. })
                if source.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(RunStoreErrorV1::Invalid(
        "failed to allocate a collision-free random run id".to_string(),
    ))
}

fn attempt_path(root: &Path, index: &AttemptIndexV1) -> PathBuf {
    root.join("attempts")
        .join(format!("{:020}-{}.cbor", index.sequence, index.run_id))
}

fn load_attempts(root: &Path) -> Result<Vec<StoredAttempt>, RunStoreErrorV1> {
    let directory = root.join("attempts");
    let entries =
        fs::read_dir(&directory).map_err(|error| RunStoreErrorV1::io(&directory, error))?;
    let mut attempts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| RunStoreErrorV1::io(&directory, error))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RunStoreErrorV1::Invalid("non-UTF-8 attempt filename".to_string()))?;
        if name.starts_with(".tmp-") {
            continue;
        }
        let bytes = read_regular_file(&path, MAX_INDEX_BYTES)?;
        let index: AttemptIndexV1 = decode_canonical(&bytes)?;
        index.validate()?;
        let expected = format!("{:020}-{}.cbor", index.sequence, index.run_id);
        if name != expected {
            return Err(RunStoreErrorV1::Invalid(format!(
                "attempt filename `{name}` disagrees with indexed identity `{expected}`"
            )));
        }
        attempts.push(StoredAttempt { path, index });
    }
    attempts.sort_by_key(|attempt| attempt.index.sequence);
    for pair in attempts.windows(2) {
        if pair[0].index.sequence == pair[1].index.sequence {
            return Err(RunStoreErrorV1::Invalid(format!(
                "run sequence {} appears more than once",
                pair[0].index.sequence
            )));
        }
    }
    Ok(attempts)
}

fn select_attempt(
    attempts: Vec<StoredAttempt>,
    selector: RunSelectorV1,
) -> Result<StoredAttempt, RunStoreErrorV1> {
    match selector {
        RunSelectorV1::LastRun => attempts
            .into_iter()
            .max_by_key(|attempt| attempt.index.sequence)
            .ok_or_else(|| RunStoreErrorV1::NotFound("last-run".to_string())),
        RunSelectorV1::RunId(run_id) => {
            validate_lower_hex_64(&run_id, "requested run id").map_err(RunStoreErrorV1::Invalid)?;
            attempts
                .into_iter()
                .find(|attempt| attempt.index.run_id == run_id)
                .ok_or(RunStoreErrorV1::NotFound(run_id))
        }
    }
}

fn require_running_attempt(path: &Path, attempt: &RunAttemptV1) -> Result<(), RunStoreErrorV1> {
    let bytes = read_regular_file(path, MAX_INDEX_BYTES)?;
    let index: AttemptIndexV1 = decode_canonical(&bytes)?;
    index.validate()?;
    if index.run_id != attempt.run_id || index.sequence != attempt.sequence {
        return Err(RunStoreErrorV1::Invalid(
            "run lease disagrees with its durable attempt index".to_string(),
        ));
    }
    match index.state {
        AttemptStateV1::Running { seed } if seed.as_ref() == &attempt.seed => Ok(()),
        AttemptStateV1::Running { .. } => Err(RunStoreErrorV1::Invalid(
            "run lease seed disagrees with its durable attempt index".to_string(),
        )),
        AttemptStateV1::Terminal { .. } => Err(RunStoreErrorV1::Invalid(
            "run attempt was already finalized".to_string(),
        )),
    }
}

fn validate_record_matches_attempt(
    record: &RunRecordV1,
    attempt: &RunAttemptV1,
) -> Result<(), RunStoreErrorV1> {
    if record.run_id != attempt.run_id
        || record.sequence != attempt.sequence
        || record.input != attempt.seed.input
        || record.intent != attempt.seed.intent
        || record.plan != attempt.seed.plan
        || record.started_unix_nanos != attempt.seed.started_unix_nanos
    {
        return Err(RunStoreErrorV1::Invalid(
            "terminal record disagrees with its pre-execution attempt seed".to_string(),
        ));
    }
    Ok(())
}

fn reconcile_orphans_locked(root: &Path) -> Result<(), RunStoreErrorV1> {
    let attempts = load_attempts(root)?;
    let running_run_ids = attempts
        .iter()
        .filter_map(|stored| match &stored.index.state {
            AttemptStateV1::Running { .. } => Some(stored.index.run_id.clone()),
            AttemptStateV1::Terminal { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    sweep_released_nonrunning_leases_locked(root, &running_run_ids)?;

    for stored in attempts {
        let AttemptStateV1::Running { seed } = &stored.index.state else {
            continue;
        };
        let lease_path = root
            .join("leases")
            .join(format!("{}.lock", stored.index.run_id));
        let lease = match open_private_file(&lease_path, false, false) {
            Ok(file) => Some(file),
            Err(RunStoreErrorV1::Io { source, .. }) if source.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let released = if let Some(file) = &lease {
            match FileExt::try_lock_exclusive(file) {
                Ok(()) => true,
                Err(error) if error.kind() == ErrorKind::WouldBlock => false,
                Err(error) => return Err(RunStoreErrorV1::io(&lease_path, error)),
            }
        } else {
            true
        };
        if !released {
            continue;
        }

        let record = RunRecordV1::interrupted(
            stored.index.run_id.clone(),
            stored.index.sequence,
            seed,
            unix_nanos_now()?,
        );
        let bytes = canonical_bytes(&record)?;
        let reference = RunContentRefV1 {
            kind: RunContentKindV1::Record,
            sha256: domain_digest(RunContentKindV1::Record.domain(), &bytes),
            bytes_len: bytes.len() as u64,
        };
        prune_locked(
            root,
            0,
            std::slice::from_ref(&reference),
            Some(stored.index.run_id.as_str()),
        )?;
        publish_object_bytes(root, &reference, &bytes)?;
        let terminal = AttemptIndexV1 {
            schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
            run_id: stored.index.run_id.clone(),
            sequence: stored.index.sequence,
            state: AttemptStateV1::Terminal {
                record: reference.clone(),
                referenced_objects: vec![reference],
            },
        };
        write_canonical_atomic(root, &stored.path, &terminal)?;
        sync_directory(&root.join("attempts"))?;
        if let Some(file) = lease {
            let _ = FileExt::unlock(&file);
            fs::remove_file(&lease_path)
                .map_err(|error| RunStoreErrorV1::io(&lease_path, error))?;
            sync_directory(&root.join("leases"))?;
        }
    }
    Ok(())
}

/// Remove released lease files which cannot belong to a running attempt.
///
/// These are durable crash remnants from either side of attempt-index
/// publication: a process may die after creating its lease but before writing
/// a running index, or after publishing a terminal index but before unlinking
/// its lease. An advisory lock always wins over index state so a live writer in
/// either narrow transition window is never disturbed.
fn sweep_released_nonrunning_leases_locked(
    root: &Path,
    running_run_ids: &BTreeSet<String>,
) -> Result<(), RunStoreErrorV1> {
    let directory = root.join("leases");
    let entries =
        fs::read_dir(&directory).map_err(|error| RunStoreErrorV1::io(&directory, error))?;
    let mut changed = false;

    for entry in entries {
        let entry = entry.map_err(|error| RunStoreErrorV1::io(&directory, error))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RunStoreErrorV1::Invalid("non-UTF-8 lease filename".to_string()))?;
        let run_id = name.strip_suffix(".lock").ok_or_else(|| {
            RunStoreErrorV1::Invalid(format!("invalid run-lease filename `{name}`"))
        })?;
        validate_lower_hex_64(run_id, "run-lease filename").map_err(RunStoreErrorV1::Invalid)?;

        reject_symlink(&path)?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| RunStoreErrorV1::io(&path, error))?;
        if !metadata.is_file() {
            return Err(RunStoreErrorV1::Invalid(format!(
                "{} is not a regular run lease",
                path.display()
            )));
        }
        validate_private_permissions(&path, &metadata)?;

        let lease = open_private_file(&path, false, false)?;
        let released = match FileExt::try_lock_exclusive(&lease) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::WouldBlock => false,
            Err(error) => return Err(RunStoreErrorV1::io(&path, error)),
        };
        if !released || running_run_ids.contains(run_id) {
            if released {
                let _ = FileExt::unlock(&lease);
            }
            continue;
        }

        let _ = FileExt::unlock(&lease);
        drop(lease);
        reject_symlink(&path)?;
        fs::remove_file(&path).map_err(|error| RunStoreErrorV1::io(&path, error))?;
        changed = true;
    }

    if changed {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn prune_locked(
    root: &Path,
    reserve_attempts: usize,
    reserved_objects: &[RunContentRefV1],
    protected_run_id: Option<&str>,
) -> Result<(), RunStoreErrorV1> {
    let mut reserved = BTreeMap::new();
    for reference in reserved_objects {
        reference.validate().map_err(RunStoreErrorV1::Invalid)?;
        let key = (reference.kind, reference.sha256.clone());
        if let Some(previous) = reserved.insert(key, reference.clone()) {
            if previous.bytes_len != reference.bytes_len {
                return Err(RunStoreErrorV1::Invalid(format!(
                    "reserved object {} has conflicting recorded lengths",
                    reference.sha256
                )));
            }
        }
    }
    let reserved_only_bytes = reserved.values().try_fold(0_u64, |total, reference| {
        total.checked_add(reference.bytes_len).ok_or_else(|| {
            RunStoreErrorV1::Invalid("reserved object byte counter overflow".to_string())
        })
    })?;
    if reserved_only_bytes > MAX_RUN_OBJECT_BYTES_V1 {
        return Err(RunStoreErrorV1::ByteCapacity {
            required_bytes: reserved_only_bytes,
            maximum_bytes: MAX_RUN_OBJECT_BYTES_V1,
        });
    }

    loop {
        let attempts = load_attempts(root)?;
        let active = attempts
            .iter()
            .filter(|attempt| matches!(attempt.index.state, AttemptStateV1::Running { .. }))
            .count();
        if active.saturating_add(reserve_attempts) > MAX_RUN_ATTEMPTS_V1 {
            return Err(RunStoreErrorV1::ActiveCapacity { active });
        }
        let referenced = referenced_object_inventory(root, &attempts)?;
        let mut required = referenced.values().try_fold(0_u64, |total, reference| {
            total
                .checked_add(reference.bytes_len)
                .ok_or_else(|| RunStoreErrorV1::Invalid("object byte counter overflow".to_string()))
        })?;
        for (key, reference) in &reserved {
            if let Some(previous) = referenced.get(key) {
                if previous.bytes_len != reference.bytes_len {
                    return Err(RunStoreErrorV1::Invalid(format!(
                        "object {} has conflicting recorded lengths",
                        reference.sha256
                    )));
                }
            } else {
                required = required.checked_add(reference.bytes_len).ok_or_else(|| {
                    RunStoreErrorV1::Invalid("reserved object byte counter overflow".to_string())
                })?;
            }
        }
        let count_ok = attempts.len().saturating_add(reserve_attempts) <= MAX_RUN_ATTEMPTS_V1;
        let bytes_ok = required <= MAX_RUN_OBJECT_BYTES_V1;
        if count_ok && bytes_ok {
            return Ok(());
        }

        let evict = attempts.iter().find(|attempt| {
            attempt.index.run_id != protected_run_id.unwrap_or("")
                && matches!(attempt.index.state, AttemptStateV1::Terminal { .. })
        });
        let Some(evict) = evict else {
            if !bytes_ok {
                return Err(RunStoreErrorV1::ByteCapacity {
                    required_bytes: required,
                    maximum_bytes: MAX_RUN_OBJECT_BYTES_V1,
                });
            }
            return Err(RunStoreErrorV1::ActiveCapacity { active });
        };
        fs::remove_file(&evict.path).map_err(|error| RunStoreErrorV1::io(&evict.path, error))?;
        sync_directory(&root.join("attempts"))?;
        cleanup_unreferenced_objects_locked(root)?;
    }
}

fn referenced_object_inventory(
    root: &Path,
    attempts: &[StoredAttempt],
) -> Result<BTreeMap<(RunContentKindV1, String), RunContentRefV1>, RunStoreErrorV1> {
    let mut objects = BTreeMap::new();
    for attempt in attempts {
        let AttemptStateV1::Terminal {
            referenced_objects, ..
        } = &attempt.index.state
        else {
            continue;
        };
        for reference in referenced_objects {
            let path = object_path(root, reference);
            validate_object_metadata(&path, reference.bytes_len)?;
            let key = (reference.kind, reference.sha256.clone());
            if let Some(previous) = objects.insert(key, reference.clone()) {
                if previous.bytes_len != reference.bytes_len {
                    return Err(RunStoreErrorV1::Invalid(format!(
                        "object {} has conflicting recorded lengths",
                        reference.sha256
                    )));
                }
            }
        }
    }
    Ok(objects)
}

fn cleanup_unreferenced_objects_locked(root: &Path) -> Result<(), RunStoreErrorV1> {
    let attempts = load_attempts(root)?;
    let referenced = referenced_object_inventory(root, &attempts)?;
    for kind in [RunContentKindV1::Record, RunContentKindV1::Trace] {
        let directory = root.join("objects").join(kind.directory());
        let entries =
            fs::read_dir(&directory).map_err(|error| RunStoreErrorV1::io(&directory, error))?;
        let mut changed = false;
        for entry in entries {
            let entry = entry.map_err(|error| RunStoreErrorV1::io(&directory, error))?;
            let path = entry.path();
            let name = entry.file_name().into_string().map_err(|_| {
                RunStoreErrorV1::Invalid("non-UTF-8 run-object filename".to_string())
            })?;
            validate_lower_hex_64(&name, "run-object filename")
                .map_err(RunStoreErrorV1::Invalid)?;
            reject_symlink(&path)?;
            if !referenced.contains_key(&(kind, name)) {
                fs::remove_file(&path).map_err(|error| RunStoreErrorV1::io(&path, error))?;
                changed = true;
            }
        }
        if changed {
            sync_directory(&directory)?;
        }
    }
    Ok(())
}

fn stage_object(
    root: &Path,
    run_id: &str,
    kind: RunContentKindV1,
    bytes: Vec<u8>,
) -> Result<StagedObject, RunStoreErrorV1> {
    let bytes_len = u64::try_from(bytes.len())
        .map_err(|_| RunStoreErrorV1::Invalid("run object length does not fit u64".to_string()))?;
    if bytes_len > MAX_RUN_OBJECT_BYTES_V1 {
        return Err(RunStoreErrorV1::ByteCapacity {
            required_bytes: bytes_len,
            maximum_bytes: MAX_RUN_OBJECT_BYTES_V1,
        });
    }
    let reference = RunContentRefV1 {
        kind,
        sha256: domain_digest(kind.domain(), &bytes),
        bytes_len,
    };
    validate_lower_hex_64(run_id, "staged-object run id").map_err(RunStoreErrorV1::Invalid)?;
    let temporary = unique_temporary_path(root, "object", Some(run_id))?;
    write_new_private_synced(&temporary, &bytes)?;
    Ok(StagedObject {
        temporary,
        reference,
    })
}

fn publish_staged_object(root: &Path, object: &mut StagedObject) -> Result<(), RunStoreErrorV1> {
    let destination = object_path(root, &object.reference);
    if let Some(existing) = read_regular_file_if_exists(
        &destination,
        usize::try_from(MAX_RUN_OBJECT_BYTES_V1).unwrap_or(usize::MAX),
    )? {
        let actual = domain_digest(object.reference.kind.domain(), &existing);
        if actual != object.reference.sha256 || existing.len() as u64 != object.reference.bytes_len
        {
            return Err(RunStoreErrorV1::Invalid(format!(
                "existing immutable run object {} disagrees with its identity",
                destination.display()
            )));
        }
        fs::remove_file(&object.temporary)
            .map_err(|error| RunStoreErrorV1::io(&object.temporary, error))?;
        sync_directory(&root.join("tmp"))?;
        return Ok(());
    }
    reject_symlink(&destination)?;
    fs::rename(&object.temporary, &destination)
        .map_err(|error| RunStoreErrorV1::io(&destination, error))?;
    sync_directory(destination.parent().expect("object path has parent"))?;
    sync_directory(&root.join("tmp"))
}

fn publish_object_bytes(
    root: &Path,
    reference: &RunContentRefV1,
    bytes: &[u8],
) -> Result<(), RunStoreErrorV1> {
    if bytes.len() as u64 != reference.bytes_len
        || domain_digest(reference.kind.domain(), bytes) != reference.sha256
    {
        return Err(RunStoreErrorV1::Invalid(
            "run object bytes disagree with their proposed identity".to_string(),
        ));
    }
    let mut staged = StagedObject {
        temporary: unique_temporary_path(root, "object", None)?,
        reference: reference.clone(),
    };
    write_new_private_synced(&staged.temporary, bytes)?;
    let result = publish_staged_object(root, &mut staged);
    if result.is_err() {
        let _ = fs::remove_file(&staged.temporary);
    }
    result
}

fn read_object(root: &Path, reference: &RunContentRefV1) -> Result<Vec<u8>, RunStoreErrorV1> {
    reference.validate().map_err(RunStoreErrorV1::Invalid)?;
    let maximum = usize::try_from(MAX_RUN_OBJECT_BYTES_V1).unwrap_or(usize::MAX);
    let path = object_path(root, reference);
    let bytes = read_regular_file(&path, maximum)?;
    if bytes.len() as u64 != reference.bytes_len {
        return Err(RunStoreErrorV1::Invalid(format!(
            "run object {} length disagrees with its reference",
            reference.sha256
        )));
    }
    let actual = domain_digest(reference.kind.domain(), &bytes);
    if actual != reference.sha256 {
        return Err(RunStoreErrorV1::Invalid(format!(
            "run object digest mismatch: expected {}, observed {actual}",
            reference.sha256
        )));
    }
    Ok(bytes)
}

fn object_path(root: &Path, reference: &RunContentRefV1) -> PathBuf {
    root.join("objects")
        .join(reference.kind.directory())
        .join(&reference.sha256)
}

fn validate_object_metadata(path: &Path, expected_len: u64) -> Result<(), RunStoreErrorV1> {
    reject_symlink(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| RunStoreErrorV1::io(path, error))?;
    if !metadata.is_file() {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} is not a regular run object",
            path.display()
        )));
    }
    if metadata.len() != expected_len {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} has {} bytes; indexed length is {expected_len}",
            path.display(),
            metadata.len()
        )));
    }
    validate_private_permissions(path, &metadata)
}

fn domain_digest(domain: &'static [u8], bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ostadix.run-object-domain/v1\0");
    hash.update((domain.len() as u64).to_be_bytes());
    hash.update(domain);
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    hex::encode(hash.finalize())
}

fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, RunStoreErrorV1> {
    crate::canonical_cbor::encode(value).map_err(|error| {
        RunStoreErrorV1::Invalid(format!("canonical CBOR encoding failed: {error}"))
    })
}

fn decode_canonical<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T, RunStoreErrorV1> {
    let decoded: T = crate::canonical_cbor::decode_bounded(
        bytes,
        crate::canonical_cbor::DecodeLimits {
            max_bytes: usize::try_from(MAX_RUN_OBJECT_BYTES_V1).unwrap_or(usize::MAX),
            max_items: MAX_DECODE_ITEMS,
            max_depth: MAX_DECODE_DEPTH,
        },
    )
    .map_err(|error| {
        RunStoreErrorV1::Invalid(format!("canonical CBOR decoding failed: {error}"))
    })?;
    let reencoded = canonical_bytes(&decoded)?;
    if reencoded != bytes {
        return Err(RunStoreErrorV1::Invalid(
            "run-store object is not canonical CBOR".to_string(),
        ));
    }
    Ok(decoded)
}

fn write_canonical_atomic<T: Serialize>(
    root: &Path,
    destination: &Path,
    value: &T,
) -> Result<(), RunStoreErrorV1> {
    let bytes = canonical_bytes(value)?;
    let temporary = unique_temporary_path(root, "metadata", None)?;
    write_new_private_synced(&temporary, &bytes)?;
    reject_symlink(destination)?;
    let result = fs::rename(&temporary, destination)
        .map_err(|error| RunStoreErrorV1::io(destination, error))
        .and_then(|()| {
            sync_directory(destination.parent().ok_or_else(|| {
                RunStoreErrorV1::Invalid("metadata destination has no parent".to_string())
            })?)?;
            sync_directory(&root.join("tmp"))
        });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn unique_temporary_path(
    root: &Path,
    label: &str,
    run_id: Option<&str>,
) -> Result<PathBuf, RunStoreErrorV1> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| {
        RunStoreErrorV1::Invalid(format!("failed to allocate random temporary name: {error}"))
    })?;
    let owner = run_id
        .map(|value| format!("run-{value}"))
        .unwrap_or_else(|| "global".to_string());
    Ok(root.join("tmp").join(format!(
        ".tmp-{owner}-{label}-{}-{sequence}-{}",
        std::process::id(),
        hex::encode(random)
    )))
}

fn write_new_private_synced(path: &Path, bytes: &[u8]) -> Result<(), RunStoreErrorV1> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|error| RunStoreErrorV1::io(path, error))?;
    file.write_all(bytes)
        .map_err(|error| RunStoreErrorV1::io(path, error))?;
    file.sync_all()
        .map_err(|error| RunStoreErrorV1::io(path, error))
}

fn open_private_file(path: &Path, create: bool, create_new: bool) -> Result<File, RunStoreErrorV1> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .create_new(create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| RunStoreErrorV1::io(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| RunStoreErrorV1::io(path, error))?;
    if !metadata.is_file() {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| RunStoreErrorV1::io(path, error))?;
    }
    Ok(file)
}

fn read_regular_file(path: &Path, maximum: usize) -> Result<Vec<u8>, RunStoreErrorV1> {
    read_regular_file_if_exists(path, maximum)?.ok_or_else(|| {
        RunStoreErrorV1::io(path, std::io::Error::new(ErrorKind::NotFound, "not found"))
    })
}

fn read_regular_file_if_exists(
    path: &Path,
    maximum: usize,
) -> Result<Option<Vec<u8>>, RunStoreErrorV1> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RunStoreErrorV1::io(path, error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| RunStoreErrorV1::io(path, error))?;
    if !metadata.is_file() {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    validate_private_permissions(path, &metadata)?;
    let maximum_u64 = u64::try_from(maximum)
        .map_err(|_| RunStoreErrorV1::Invalid("file-size limit does not fit u64".to_string()))?;
    if metadata.len() > maximum_u64 {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} has {} bytes; maximum is {maximum}",
            path.display(),
            metadata.len()
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(maximum));
    file.take(maximum_u64.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| RunStoreErrorV1::io(path, error))?;
    if bytes.len() > maximum {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} exceeds the {maximum}-byte read limit",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

fn reject_symlink(path: &Path) -> Result<(), RunStoreErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(RunStoreErrorV1::Invalid(
            format!("refusing symlink {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RunStoreErrorV1::io(path, error)),
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), RunStoreErrorV1> {
    reject_symlink(path)?;
    fs::create_dir_all(path).map_err(|error| RunStoreErrorV1::io(path, error))?;
    let metadata = fs::symlink_metadata(path).map_err(|error| RunStoreErrorV1::io(path, error))?;
    if !metadata.is_dir() {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| RunStoreErrorV1::io(path, error))?;
    }
    Ok(())
}

fn validate_existing_private_directory(path: &Path) -> Result<(), RunStoreErrorV1> {
    reject_symlink(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| RunStoreErrorV1::io(path, error))?;
    if !metadata.is_dir() {
        return Err(RunStoreErrorV1::Invalid(format!(
            "{} is not an existing directory",
            path.display()
        )));
    }
    validate_private_permissions(path, &metadata)
}

fn validate_private_permissions(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), RunStoreErrorV1> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RunStoreErrorV1::Invalid(format!(
                "{} is not private to its owner",
                path.display()
            )));
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), RunStoreErrorV1> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| RunStoreErrorV1::io(path, error))
}

fn unix_nanos_now() -> Result<u64, RunStoreErrorV1> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            RunStoreErrorV1::Invalid(format!("system clock precedes Unix epoch: {error}"))
        })?
        .as_nanos();
    u64::try_from(nanos)
        .map_err(|_| RunStoreErrorV1::Invalid("Unix timestamp does not fit u64".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::record::{
        CapturedStreamV1, ExecutionIntentObservationV1, PlanIdentitiesV1, RunDispositionV1,
        RunInputIdentityV1, RunInputKindV1, RunTraceBindingV1,
    };

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn seed(name: &str) -> RunAttemptSeedV1 {
        RunAttemptSeedV1 {
            input: RunInputIdentityV1 {
                kind: RunInputKindV1::OrdinaryO,
                path: PathBuf::from(name),
                digest_sha256: sha(name.as_bytes()),
            },
            intent: ExecutionIntentObservationV1 {
                engine: "local_hgraph".to_string(),
                target: Some("local".to_string()),
                selected_route: None,
                route_policy: None,
                route_declarations: Vec::new(),
                parallel_policy: "local".to_string(),
                local_worker_limit: None,
                mesh_mode: None,
                mesh_max_retries: None,
                mesh_fallback: None,
                mesh_discovery_timeout_ms: None,
                mesh_closed_registry: None,
                mesh_peer_root: None,
            },
            plan: PlanIdentitiesV1 {
                oir_sha256: Some(sha(b"oir")),
                execution_plan_sha256: Some(sha(b"plan")),
                hgraph_sha256: Some(sha(b"hgraph")),
                execution_intent_sha256: Some(sha(b"intent")),
                ..PlanIdentitiesV1::default()
            },
            started_unix_nanos: 1,
        }
    }

    fn success_record(attempt: &RunAttemptV1, bytes: Vec<u8>) -> RunRecordV1 {
        let decoded = serde_json::json!({"ok": true});
        RunRecordV1::terminal(
            attempt.run_id.clone(),
            attempt.sequence,
            &attempt.seed,
            2,
            1,
            RunDispositionV1::Succeeded,
            CapturedStreamV1::complete(bytes),
            CapturedStreamV1::default(),
            Some(decoded.clone()),
            Vec::new(),
            crate::intent::record::decoded_value_result_references(Some(&decoded), "ordinary_o"),
            RunTraceBindingV1::unavailable("test compatibility engine"),
            None,
        )
    }

    #[derive(Debug, PartialEq, Eq)]
    enum SnapshotEntry {
        Directory(PathBuf),
        File(PathBuf, Vec<u8>),
    }

    fn snapshot_tree(root: &Path) -> Vec<SnapshotEntry> {
        fn visit(root: &Path, path: &Path, snapshot: &mut Vec<SnapshotEntry>) {
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                snapshot.push(SnapshotEntry::Directory(relative));
                let mut entries = fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect::<Vec<_>>();
                entries.sort();
                for entry in entries {
                    visit(root, &entry, snapshot);
                }
            } else {
                snapshot.push(SnapshotEntry::File(relative, fs::read(path).unwrap()));
            }
        }

        let mut snapshot = Vec::new();
        visit(root, root, &mut snapshot);
        snapshot
    }

    fn install_sparse_terminal(root: &Path, sequence: u64, bytes_len: u64) -> AttemptIndexV1 {
        let run_id = format!("{sequence:064x}");
        let reference = RunContentRefV1 {
            kind: RunContentKindV1::Record,
            sha256: sha(format!("sparse-{sequence}").as_bytes()),
            bytes_len,
        };
        let path = object_path(root, &reference);
        let file = open_private_file(&path, true, true).unwrap();
        file.set_len(bytes_len).unwrap();
        file.sync_all().unwrap();
        let index = AttemptIndexV1 {
            schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
            run_id,
            sequence,
            state: AttemptStateV1::Terminal {
                record: reference.clone(),
                referenced_objects: vec![reference],
            },
        };
        write_canonical_atomic(root, &attempt_path(root, &index), &index).unwrap();
        index
    }

    #[test]
    fn finalize_and_last_run_round_trip_exact_retained_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let store = RunStoreV1::open_at(temp.path().join("runs-v1")).unwrap();
        let lease = store.begin(seed("first.O")).unwrap();
        let attempt = lease.attempt().clone();
        let expected = vec![0, 1, 2, 0xff];
        let record = success_record(&attempt, expected.clone());
        let finalized = lease.finalize(record, None).unwrap();

        let reader = RunStoreReaderV1::open_existing(store.root()).unwrap();
        let (record, trace) = reader.read_terminal(RunSelectorV1::LastRun, true).unwrap();
        assert_eq!(record.run_id, finalized.run_id);
        assert_eq!(record.stdout.retained, expected);
        assert!(trace.is_none());
    }

    #[test]
    fn default_location_requires_xdg_or_home_without_cwd_fallback() {
        assert_eq!(
            default_run_store_root_from(Some(OsString::from("/state")), None).unwrap(),
            PathBuf::from("/state/ostadix/runs-v1")
        );
        assert_eq!(
            default_run_store_root_from(None, Some(OsString::from("/home/lee"))).unwrap(),
            PathBuf::from("/home/lee/.local/state/ostadix/runs-v1")
        );
        assert!(matches!(
            default_run_store_root_from(None, None),
            Err(RunStoreErrorV1::DefaultLocationUnavailable)
        ));
        assert!(matches!(
            default_run_store_root_from(Some(OsString::new()), Some(OsString::new())),
            Err(RunStoreErrorV1::DefaultLocationUnavailable)
        ));
    }

    #[test]
    fn oversized_running_index_is_rejected_without_mutating_history_or_sequence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let first = store.begin(seed("retained-before-oversize.O")).unwrap();
        let first_attempt = first.attempt().clone();
        let finalized = first
            .finalize(
                success_record(&first_attempt, b"retained history".to_vec()),
                None,
            )
            .unwrap();
        let before = snapshot_tree(&root);

        let mut oversized = seed("oversized.O");
        oversized
            .intent
            .route_declarations
            .push("x".repeat(MAX_INDEX_BYTES));
        let error = match store.begin(oversized) {
            Err(error) => error,
            Ok(_) => panic!("oversized running attempt index was accepted"),
        };

        assert!(matches!(
            error,
            RunStoreErrorV1::Invalid(detail)
                if detail.contains("running attempt index requires")
                    && detail.contains("maximum is 262144")
        ));
        assert_eq!(snapshot_tree(&root), before);
        assert_eq!(read_sequence(&root).unwrap(), 1);
        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        let (retained, _) = reader
            .read_terminal(RunSelectorV1::RunId(finalized.run_id), false)
            .unwrap();
        assert_eq!(retained.stdout.retained, b"retained history");

        let next = store.begin(seed("usable-after-oversize.O")).unwrap();
        assert_eq!(next.attempt().sequence, 2);
        let next_attempt = next.attempt().clone();
        next.finalize(success_record(&next_attempt, Vec::new()), None)
            .unwrap();
    }

    #[test]
    fn independent_leases_finalize_concurrently() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let first = store.begin(seed("concurrent-a.O")).unwrap();
        let second = store.begin(seed("concurrent-b.O")).unwrap();
        let first_attempt = first.attempt().clone();
        let second_attempt = second.attempt().clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let first_barrier = barrier.clone();
        let first_thread = std::thread::spawn(move || {
            first_barrier.wait();
            first
                .finalize(success_record(&first_attempt, b"first".to_vec()), None)
                .unwrap()
        });
        let second_thread = std::thread::spawn(move || {
            barrier.wait();
            second
                .finalize(success_record(&second_attempt, b"second".to_vec()), None)
                .unwrap()
        });
        let first = first_thread.join().unwrap();
        let second = second_thread.join().unwrap();

        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        let (first_record, _) = reader
            .read_terminal(RunSelectorV1::RunId(first.run_id), false)
            .unwrap();
        let (second_record, _) = reader
            .read_terminal(RunSelectorV1::RunId(second.run_id), false)
            .unwrap();
        assert_eq!(first_record.stdout.retained, b"first");
        assert_eq!(second_record.stdout.retained, b"second");
        assert_eq!(load_attempts(&root).unwrap().len(), 2);
    }

    #[test]
    fn one_hundred_twenty_ninth_terminal_evicts_the_oldest_attempt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let mut first_run_id = None;
        let mut newest_run_id = None;
        for index in 0..=MAX_RUN_ATTEMPTS_V1 {
            let lease = store.begin(seed(&format!("terminal-{index}.O"))).unwrap();
            let run_id = lease.attempt().run_id.clone();
            first_run_id.get_or_insert_with(|| run_id.clone());
            newest_run_id = Some(run_id);
            let attempt = lease.attempt().clone();
            lease
                .finalize(success_record(&attempt, Vec::new()), None)
                .unwrap();
        }

        let attempts = load_attempts(&root).unwrap();
        assert_eq!(attempts.len(), MAX_RUN_ATTEMPTS_V1);
        assert_eq!(attempts.first().unwrap().index.sequence, 2);
        assert_eq!(attempts.last().unwrap().index.sequence, 129);
        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        assert!(matches!(
            reader.inspect(RunSelectorV1::RunId(first_run_id.unwrap()), false),
            Err(RunStoreErrorV1::NotFound(_))
        ));
        let (last, _) = reader.read_terminal(RunSelectorV1::LastRun, false).unwrap();
        assert_eq!(last.run_id, newest_run_id.unwrap());
    }

    #[test]
    fn referenced_object_byte_pressure_evicts_oldest_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        RunStoreV1::open_at(&root).unwrap();
        let sparse_bytes = MAX_RUN_OBJECT_BYTES_V1 / 2 + 1;
        let oldest = install_sparse_terminal(&root, 1, sparse_bytes);
        let newest = install_sparse_terminal(&root, 2, sparse_bytes);

        with_global_lock(&root, || prune_locked(&root, 0, &[], None)).unwrap();

        let attempts = load_attempts(&root).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].index.run_id, newest.run_id);
        assert!(!attempt_path(&root, &oldest).exists());
        assert!(attempt_path(&root, &newest).exists());
        let AttemptStateV1::Terminal { record, .. } = &newest.state else {
            unreachable!()
        };
        assert!(object_path(&root, record).exists());
    }

    #[test]
    fn aggregate_reserved_oversize_is_rejected_before_history_eviction() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        RunStoreV1::open_at(&root).unwrap();
        let history = install_sparse_terminal(&root, 1, 1);
        let half_plus_one = MAX_RUN_OBJECT_BYTES_V1 / 2 + 1;
        let reserved_record = RunContentRefV1 {
            kind: RunContentKindV1::Record,
            sha256: sha(b"oversized-reserved-record"),
            bytes_len: half_plus_one,
        };
        let reserved_trace = RunContentRefV1 {
            kind: RunContentKindV1::Trace,
            sha256: sha(b"oversized-reserved-trace"),
            bytes_len: half_plus_one,
        };
        let before = snapshot_tree(&root);

        let error = with_global_lock(&root, || {
            prune_locked(
                &root,
                0,
                &[
                    reserved_record.clone(),
                    reserved_record.clone(),
                    reserved_trace,
                ],
                None,
            )
        })
        .unwrap_err();

        assert!(matches!(
            error,
            RunStoreErrorV1::ByteCapacity {
                required_bytes,
                maximum_bytes: MAX_RUN_OBJECT_BYTES_V1,
            } if required_bytes == half_plus_one * 2
        ));
        assert_eq!(snapshot_tree(&root), before);
        assert!(attempt_path(&root, &history).exists());
        let AttemptStateV1::Terminal { record, .. } = &history.state else {
            unreachable!()
        };
        assert!(object_path(&root, record).exists());
    }

    #[test]
    fn exact_finalization_schema_failure_publishes_recording_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let lease = store.begin(seed("invalid-terminal.O")).unwrap();
        let run_id = lease.attempt().run_id.clone();
        let mut record = success_record(lease.attempt(), Vec::new());
        record.schema = "ostadix.run-record/v999".to_string();

        let error = lease.finalize(record, None).unwrap_err();
        assert!(matches!(
            error,
            RunStoreErrorV1::FinalizationIncomplete { .. }
        ));
        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        let (fallback, _) = reader
            .read_terminal(RunSelectorV1::RunId(run_id), false)
            .unwrap();
        assert_eq!(fallback.disposition, RunDispositionV1::RecordingIncomplete);
        let failure = fallback.failure.unwrap();
        assert_eq!(failure.stage, "recording");
        assert!(failure.message.contains("unsupported run-record schema"));
    }

    #[test]
    fn canonical_record_with_wrong_schema_is_rejected_during_inspection() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let lease = store.begin(seed("schema-mismatch.O")).unwrap();
        let attempt = lease.attempt().clone();
        let finalized = lease
            .finalize(success_record(&attempt, Vec::new()), None)
            .unwrap();
        let original = read_object(&root, &finalized.record).unwrap();
        let mut record: RunRecordV1 = decode_canonical(&original).unwrap();
        record.schema = "ostadix.run-record/v999".to_string();
        let bytes = canonical_bytes(&record).unwrap();
        let replacement = RunContentRefV1 {
            kind: RunContentKindV1::Record,
            sha256: domain_digest(RunContentKindV1::Record.domain(), &bytes),
            bytes_len: bytes.len() as u64,
        };

        with_global_lock(&root, || {
            publish_object_bytes(&root, &replacement, &bytes)?;
            let index = AttemptIndexV1 {
                schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
                run_id: finalized.run_id.clone(),
                sequence: finalized.sequence,
                state: AttemptStateV1::Terminal {
                    record: replacement.clone(),
                    referenced_objects: vec![replacement.clone()],
                },
            };
            write_canonical_atomic(&root, &attempt_path(&root, &index), &index)?;
            cleanup_unreferenced_objects_locked(&root)
        })
        .unwrap();

        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        let error = reader
            .read_terminal(RunSelectorV1::LastRun, false)
            .unwrap_err();
        assert!(matches!(
            error,
            RunStoreErrorV1::Invalid(detail)
                if detail.contains("unsupported run-record schema")
        ));
    }

    #[test]
    fn read_only_inspection_leaves_store_tree_byte_identical() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let lease = store.begin(seed("read-only.O")).unwrap();
        let attempt = lease.attempt().clone();
        lease
            .finalize(success_record(&attempt, b"observed".to_vec()), None)
            .unwrap();
        let before = snapshot_tree(&root);

        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        let inspected = reader.inspect(RunSelectorV1::LastRun, true).unwrap();
        assert!(matches!(inspected, RunInspectionV1::Terminal { .. }));

        assert_eq!(snapshot_tree(&root), before);
    }

    #[test]
    fn reader_does_not_reconcile_but_later_writer_does() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let lease = store.begin(seed("orphan.O")).unwrap();
        let run_id = lease.attempt().run_id.clone();
        drop(lease);

        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        assert!(matches!(
            reader
                .inspect(RunSelectorV1::RunId(run_id.clone()), false)
                .unwrap(),
            RunInspectionV1::Running { .. }
        ));

        RunStoreV1::open_at(&root).unwrap();
        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        let (record, _) = reader
            .read_terminal(RunSelectorV1::RunId(run_id), false)
            .unwrap();
        assert_eq!(record.disposition, RunDispositionV1::Interrupted);
    }

    #[test]
    fn writer_sweeps_released_leases_on_both_index_crash_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();

        // Crash boundary two: the terminal index is durable, but its executor
        // died before releasing and unlinking the lease.
        let terminal_lease = store.begin(seed("terminal-before-release.O")).unwrap();
        let terminal_attempt = terminal_lease.attempt().clone();
        let terminal_record = success_record(&terminal_attempt, b"durable".to_vec());
        let terminal_bytes = canonical_bytes(&terminal_record).unwrap();
        let terminal_reference = RunContentRefV1 {
            kind: RunContentKindV1::Record,
            sha256: domain_digest(RunContentKindV1::Record.domain(), &terminal_bytes),
            bytes_len: terminal_bytes.len() as u64,
        };
        with_global_lock(&root, || {
            publish_object_bytes(&root, &terminal_reference, &terminal_bytes)?;
            let terminal_index = AttemptIndexV1 {
                schema: ATTEMPT_INDEX_SCHEMA_V1.to_string(),
                run_id: terminal_attempt.run_id.clone(),
                sequence: terminal_attempt.sequence,
                state: AttemptStateV1::Terminal {
                    record: terminal_reference.clone(),
                    referenced_objects: vec![terminal_reference.clone()],
                },
            };
            write_canonical_atomic(
                &root,
                &attempt_path(&root, &terminal_index),
                &terminal_index,
            )?;
            sync_directory(&root.join("attempts"))
        })
        .unwrap();
        let terminal_lease_path = terminal_lease.lease_path.clone();
        drop(terminal_lease);

        // Crash boundary one: the lease was locked, but no running attempt
        // index reached durable storage. Install this remnant last so no
        // intervening writer performs the sweep under test.
        let (_, pre_index_lease_path, pre_index_lease) = create_unique_lease(&root).unwrap();
        FileExt::lock_exclusive(&pre_index_lease).unwrap();
        drop(pre_index_lease);

        assert!(pre_index_lease_path.exists());
        assert!(terminal_lease_path.exists());
        RunStoreV1::open_at(&root).unwrap();
        assert!(!pre_index_lease_path.exists());
        assert!(!terminal_lease_path.exists());

        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        let (record, _) = reader
            .read_terminal(RunSelectorV1::RunId(terminal_attempt.run_id.clone()), false)
            .unwrap();
        assert_eq!(record.stdout.retained, b"durable");
    }

    #[test]
    fn writer_preserves_actively_locked_lease_without_running_index() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        RunStoreV1::open_at(&root).unwrap();
        let (_, lease_path, lease) = create_unique_lease(&root).unwrap();
        FileExt::lock_exclusive(&lease).unwrap();

        RunStoreV1::open_at(&root).unwrap();
        assert!(lease_path.exists());

        FileExt::unlock(&lease).unwrap();
        drop(lease);
        RunStoreV1::open_at(&root).unwrap();
        assert!(!lease_path.exists());
    }

    #[test]
    fn writer_rejects_malformed_and_nonregular_lease_entries() {
        let malformed_temp = tempfile::tempdir().unwrap();
        let malformed_root = malformed_temp.path().join("runs-v1");
        RunStoreV1::open_at(&malformed_root).unwrap();
        write_new_private_synced(&malformed_root.join("leases/not-a-run-id.lock"), b"invalid")
            .unwrap();
        assert!(matches!(
            RunStoreV1::open_at(&malformed_root),
            Err(RunStoreErrorV1::Invalid(detail))
                if detail.contains("run-lease filename")
        ));

        let nonregular_temp = tempfile::tempdir().unwrap();
        let nonregular_root = nonregular_temp.path().join("runs-v1");
        RunStoreV1::open_at(&nonregular_root).unwrap();
        let nonregular_path = nonregular_root
            .join("leases")
            .join(format!("{}.lock", "a1".repeat(32)));
        fs::create_dir(&nonregular_path).unwrap();
        assert!(matches!(
            RunStoreV1::open_at(&nonregular_root),
            Err(RunStoreErrorV1::Invalid(detail)) if detail.contains("not a regular run lease")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn writer_rejects_nonprivate_lease_entries() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        RunStoreV1::open_at(&root).unwrap();
        let lease_path = root
            .join("leases")
            .join(format!("{}.lock", "b2".repeat(32)));
        write_new_private_synced(&lease_path, b"invalid").unwrap();
        fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(matches!(
            RunStoreV1::open_at(&root),
            Err(RunStoreErrorV1::Invalid(detail)) if detail.contains("not private to its owner")
        ));
    }

    #[test]
    fn writer_repairs_missing_or_rolled_back_sequence_from_attempts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let first = store.begin(seed("first.O")).unwrap();
        let attempt = first.attempt().clone();
        first
            .finalize(success_record(&attempt, Vec::new()), None)
            .unwrap();

        fs::remove_file(root.join("sequence.cbor")).unwrap();
        let store = RunStoreV1::open_at(&root).unwrap();
        assert_eq!(read_sequence(&root).unwrap(), 1);
        let second = store.begin(seed("second.O")).unwrap();
        assert_eq!(second.attempt().sequence, 2);
        let attempt = second.attempt().clone();
        second
            .finalize(success_record(&attempt, Vec::new()), None)
            .unwrap();

        write_sequence(&root, 1).unwrap();
        let store = RunStoreV1::open_at(&root).unwrap();
        assert_eq!(read_sequence(&root).unwrap(), 2);
        let third = store.begin(seed("third.O")).unwrap();
        assert_eq!(third.attempt().sequence, 3);
    }

    #[test]
    fn writer_cleans_only_temporaries_without_an_active_owner() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let global = unique_temporary_path(&root, "test", None).unwrap();
        write_new_private_synced(&global, b"stale-global").unwrap();

        let lease = store.begin(seed("active.O")).unwrap();
        let run_temporary =
            unique_temporary_path(&root, "test", Some(&lease.attempt().run_id)).unwrap();
        write_new_private_synced(&run_temporary, b"active-run").unwrap();
        RunStoreV1::open_at(&root).unwrap();
        assert!(!global.exists());
        assert!(run_temporary.exists());

        drop(lease);
        RunStoreV1::open_at(&root).unwrap();
        assert!(!run_temporary.exists());
    }

    #[test]
    fn corrupted_immutable_record_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let lease = store.begin(seed("corrupt.O")).unwrap();
        let record = success_record(lease.attempt(), Vec::new());
        let finalized = lease.finalize(record, None).unwrap();
        let path = root.join("objects/records").join(finalized.record.sha256);
        fs::write(&path, b"corrupt").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let reader = RunStoreReaderV1::open_existing(&root).unwrap();
        assert!(reader.read_terminal(RunSelectorV1::LastRun, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn reader_rejects_symlinked_attempt() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let target = temp.path().join("outside");
        fs::write(&target, b"not an attempt").unwrap();
        symlink(
            &target,
            root.join("attempts")
                .join(format!("{:020}-{}.cbor", 1, "11".repeat(32))),
        )
        .unwrap();
        let reader = RunStoreReaderV1::open_existing(store.root()).unwrap();
        assert!(reader.inspect(RunSelectorV1::LastRun, false).is_err());
    }

    #[test]
    fn one_hundred_twenty_ninth_active_attempt_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let store = RunStoreV1::open_at(temp.path().join("runs-v1")).unwrap();
        let mut leases = Vec::new();
        for index in 0..MAX_RUN_ATTEMPTS_V1 {
            leases.push(store.begin(seed(&format!("active-{index}.O"))).unwrap());
        }
        assert!(matches!(
            store.begin(seed("too-many.O")),
            Err(RunStoreErrorV1::ActiveCapacity { active: 128 })
        ));
        drop(leases);
    }

    #[cfg(unix)]
    #[test]
    fn managed_paths_are_owner_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runs-v1");
        let store = RunStoreV1::open_at(&root).unwrap();
        let lease = store.begin(seed("private.O")).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&lease.lease_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
