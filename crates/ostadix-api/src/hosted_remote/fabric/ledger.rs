//! Durable replay fencing for the deliberately narrow Fabric V1 provider.
//!
//! This is node-local execution bookkeeping, not the coordinator journal.  A
//! provider may execute only after [`FabricAttemptLedgerV1::consume_and_accept`]
//! returns a sealed [`FabricLedgerAcceptanceV1`] whose `may_execute` bit is
//! true.  Only this module can construct that grant, and it does so only after
//! the exact issuer/attempt/nonce binding and `Accepted` state have been
//! atomically published and fsynced.

use std::cmp::Ordering as CompareOrdering;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical_cbor::{decode_bounded, encode, DecodeLimits};
use crate::execution_fabric::{AttemptIdV1, Sha256DigestV1, MAX_EXECUTION_CANDIDATE_BYTES};
use crate::execution_fabric_authority::{
    decode_fabric_response_v1, encode_fabric_response_v1, ExecutionCellIncarnationV1,
    FabricAttemptQueryV1, FabricResponseV1, FabricSubmissionV1, FabricTerminalCandidateV1,
    MAX_FABRIC_HEADER_BYTES,
};
use crate::placement_protocol::{GenerationV1, SemanticDigestV1};

const FABRIC_LEDGER_DIRECTORY_V1: &str = "fabric-v1";
const FABRIC_LEDGER_LOCK_FILE_V1: &str = ".exclusive-runtime.lock";
const FABRIC_LEDGER_SNAPSHOT_FILE_V1: &str = "attempt-ledger.cbor";
const FABRIC_LEDGER_TEMP_PREFIX_V1: &str = ".attempt-ledger-";
const FABRIC_LEDGER_TEMP_SUFFIX_V1: &str = ".tmp";
const FABRIC_LEDGER_SNAPSHOT_SCHEMA_V1: &str = "ostadix.fabric-attempt-ledger/v1";
const FABRIC_LEDGER_ENTRY_SCHEMA_V1: &str = "ostadix.fabric-attempt-ledger-entry/v1";
const FABRIC_LEDGER_BODY_DIGEST_DOMAIN_V1: &[u8] = b"ostadix/fabric-attempt-ledger/body/v1";
const FABRIC_LEDGER_TERMINAL_DIGEST_DOMAIN_V1: &[u8] = b"ostadix/fabric-attempt-ledger/terminal/v1";

/// Fabric V1 is intentionally bounded.  M3 has no general object plane and no
/// unbounded provider history; a later retention policy must version this
/// durable schema rather than silently relaxing it.
const MAX_FABRIC_LEDGER_ENTRIES_V1: usize = 128;
const MAX_FABRIC_LEDGER_SNAPSHOT_BYTES_V1: usize = 20 * 1024 * 1024;
const MAX_FABRIC_LEDGER_DECODE_ITEMS_V1: usize = 64 * 1024;
const MAX_FABRIC_LEDGER_DECODE_DEPTH_V1: usize = 64;
const MAX_FABRIC_LEDGER_REASON_CODE_BYTES_V1: usize = 64;
const MAX_FABRIC_LEDGER_REASON_MESSAGE_BYTES_V1: usize = 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FabricLedgerStateV1 {
    Received,
    Validated,
    Accepted,
    Running,
    TerminalCandidate,
    Rejected,
    Abandoned,
}

impl FabricLedgerStateV1 {
    fn is_incomplete(self) -> bool {
        matches!(
            self,
            Self::Received | Self::Validated | Self::Accepted | Self::Running
        )
    }
}

/// Every durable replay key and execution binding required before M3 work may
/// begin.  The binding is immutable for the lifetime of an attempt record.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FabricAttemptBindingV1 {
    issuer_key_id: SemanticDigestV1,
    attempt: AttemptIdV1,
    lease_nonce: SemanticDigestV1,
    tls_client_principal_sha256: SemanticDigestV1,
    submission_binding_sha256: Sha256DigestV1,
    capsule_sha256: Sha256DigestV1,
    source_closure_sha256: Sha256DigestV1,
    node_id: String,
    node_generation: GenerationV1,
    execution_cell_incarnation: ExecutionCellIncarnationV1,
}

impl FabricAttemptBindingV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer_key_id: SemanticDigestV1,
        attempt: AttemptIdV1,
        lease_nonce: SemanticDigestV1,
        tls_client_principal_sha256: SemanticDigestV1,
        submission_binding_sha256: Sha256DigestV1,
        capsule_sha256: Sha256DigestV1,
        source_closure_sha256: Sha256DigestV1,
        node_id: impl Into<String>,
        node_generation: GenerationV1,
        execution_cell_incarnation: ExecutionCellIncarnationV1,
    ) -> Result<Self> {
        let value = Self {
            issuer_key_id,
            attempt,
            lease_nonce,
            tls_client_principal_sha256,
            submission_binding_sha256,
            capsule_sha256,
            source_closure_sha256,
            node_id: node_id.into(),
            node_generation,
            execution_cell_incarnation,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn from_submission(submission: &FabricSubmissionV1) -> Result<Self> {
        submission
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid Fabric submission for ledger: {error}"))?;
        let lease = submission.header().lease().lease();
        Self::new(
            lease.issuer_key_id().clone(),
            lease.attempt().clone(),
            lease.lease_nonce().clone(),
            lease.target().tls_client_principal_sha256().clone(),
            *submission.header().submission_binding_sha256(),
            *lease.capsule_sha256(),
            *lease.source_closure_sha256(),
            lease.target().node_id(),
            lease.target().node_generation(),
            lease.target().execution_cell_incarnation(),
        )
    }

    pub fn issuer_key_id(&self) -> &SemanticDigestV1 {
        &self.issuer_key_id
    }

    pub fn attempt(&self) -> &AttemptIdV1 {
        &self.attempt
    }

    pub fn lease_nonce(&self) -> &SemanticDigestV1 {
        &self.lease_nonce
    }

    pub fn tls_client_principal_sha256(&self) -> &SemanticDigestV1 {
        &self.tls_client_principal_sha256
    }

    pub fn submission_binding_sha256(&self) -> &Sha256DigestV1 {
        &self.submission_binding_sha256
    }

    pub fn capsule_sha256(&self) -> &Sha256DigestV1 {
        &self.capsule_sha256
    }

    pub fn source_closure_sha256(&self) -> &Sha256DigestV1 {
        &self.source_closure_sha256
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn execution_cell_incarnation(&self) -> ExecutionCellIncarnationV1 {
        self.execution_cell_incarnation
    }

    fn validate(&self) -> Result<()> {
        AttemptIdV1::new(self.attempt.task().clone(), self.attempt.generation())
            .map_err(|error| anyhow::anyhow!("invalid Fabric ledger attempt: {error}"))?;
        for (field, digest) in [
            ("submission binding", &self.submission_binding_sha256),
            ("capsule", &self.capsule_sha256),
            ("source closure", &self.source_closure_sha256),
        ] {
            if digest.iter().all(|byte| *byte == 0) {
                bail!("Fabric ledger {field} digest must not be all-zero");
            }
        }
        if self.node_id.is_empty()
            || self.node_id.len() > 128
            || !self.node_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
            })
        {
            bail!("Fabric ledger node identity is not a bounded ASCII token");
        }
        Ok(())
    }

    fn compare_key(&self, other: &Self) -> CompareOrdering {
        self.issuer_key_id
            .as_sha256()
            .cmp(other.issuer_key_id.as_sha256())
            .then_with(|| {
                self.attempt
                    .task()
                    .execution()
                    .as_bytes()
                    .cmp(other.attempt.task().execution().as_bytes())
            })
            .then_with(|| {
                self.attempt
                    .task()
                    .semantic_sha256()
                    .cmp(other.attempt.task().semantic_sha256())
            })
            .then_with(|| self.attempt.generation().cmp(&other.attempt.generation()))
            .then_with(|| {
                self.lease_nonce
                    .as_sha256()
                    .cmp(other.lease_nonce.as_sha256())
            })
    }
}

/// The shared canonical-CBOR layer intentionally models byte strings through
/// JSON-compatible arrays.  Durable terminal payloads use lowercase hex text
/// inside the private ledger schema so a bounded snapshot cannot amplify each
/// byte into a heap-allocated `serde_json::Value` during validation.
mod canonical_hex_bytes_v1 {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = <String as serde::Deserialize>::deserialize(deserializer)?;
        if encoded.len() % 2 != 0
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(serde::de::Error::custom(
                "durable Fabric bytes are not canonical lowercase hex",
            ));
        }
        hex::decode(encoded).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FabricStoredTerminalV1 {
    #[serde(with = "canonical_hex_bytes_v1")]
    header_bytes: Vec<u8>,
    #[serde(with = "canonical_hex_bytes_v1")]
    candidate_bytes: Vec<u8>,
    terminal_sha256: Sha256DigestV1,
}

impl FabricStoredTerminalV1 {
    fn from_candidate(candidate: &FabricTerminalCandidateV1) -> Result<Self> {
        candidate
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid Fabric terminal candidate: {error}"))?;
        let encoded =
            encode_fabric_response_v1(&FabricResponseV1::TerminalCandidate(candidate.clone()))
                .map_err(|error| {
                    anyhow::anyhow!("cannot encode Fabric terminal candidate: {error}")
                })?;
        let (header_bytes, payload_bytes) = encoded.into_parts();
        let candidate_bytes =
            payload_bytes.context("terminal candidate encoding omitted payload")?;
        Self::new_exact(header_bytes, candidate_bytes)
    }

    fn new_exact(header_bytes: Vec<u8>, candidate_bytes: Vec<u8>) -> Result<Self> {
        if header_bytes.is_empty() || header_bytes.len() > MAX_FABRIC_HEADER_BYTES {
            bail!("Fabric ledger terminal header is outside protocol bounds");
        }
        if candidate_bytes.is_empty() || candidate_bytes.len() > MAX_EXECUTION_CANDIDATE_BYTES {
            bail!("Fabric ledger candidate payload is outside protocol bounds");
        }
        let terminal_sha256 = terminal_parts_sha256(&header_bytes, &candidate_bytes)?;
        Ok(Self {
            header_bytes,
            candidate_bytes,
            terminal_sha256,
        })
    }

    pub fn header_bytes(&self) -> &[u8] {
        &self.header_bytes
    }

    pub fn candidate_bytes(&self) -> &[u8] {
        &self.candidate_bytes
    }

    fn validate(&self) -> Result<()> {
        let expected = Self::new_exact(self.header_bytes.clone(), self.candidate_bytes.clone())?;
        if expected.terminal_sha256 != self.terminal_sha256 {
            bail!("Fabric ledger terminal byte digest mismatch");
        }
        self.decode_terminal_candidate().map(|_| ())
    }

    fn decode_terminal_candidate(&self) -> Result<FabricTerminalCandidateV1> {
        match decode_fabric_response_v1(&self.header_bytes, Some(&self.candidate_bytes))
            .map_err(|error| anyhow::anyhow!("stored Fabric terminal is invalid: {error}"))?
        {
            FabricResponseV1::TerminalCandidate(candidate) => Ok(candidate),
            _ => bail!("stored Fabric terminal bytes do not encode a terminal candidate"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FabricLedgerEntryV1 {
    schema: String,
    binding: FabricAttemptBindingV1,
    state: FabricLedgerStateV1,
    transition_sequence: u64,
    terminal: Option<FabricStoredTerminalV1>,
    reason_code: Option<String>,
    reason_message: Option<String>,
}

impl FabricLedgerEntryV1 {
    fn new(binding: FabricAttemptBindingV1, state: FabricLedgerStateV1) -> Self {
        Self {
            schema: FABRIC_LEDGER_ENTRY_SCHEMA_V1.to_owned(),
            binding,
            state,
            transition_sequence: 1,
            terminal: None,
            reason_code: None,
            reason_message: None,
        }
    }

    pub fn binding(&self) -> &FabricAttemptBindingV1 {
        &self.binding
    }

    #[cfg(test)]
    fn state(&self) -> FabricLedgerStateV1 {
        self.state
    }

    #[cfg(test)]
    fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }

    pub fn current_response(&self) -> FabricLedgerCurrentResponseV1<'_> {
        match self.state {
            FabricLedgerStateV1::Received => FabricLedgerCurrentResponseV1::Received,
            FabricLedgerStateV1::Validated => FabricLedgerCurrentResponseV1::Validated,
            FabricLedgerStateV1::Accepted => FabricLedgerCurrentResponseV1::Accepted,
            FabricLedgerStateV1::Running => FabricLedgerCurrentResponseV1::Running,
            FabricLedgerStateV1::TerminalCandidate => {
                FabricLedgerCurrentResponseV1::TerminalCandidate(
                    self.terminal
                        .as_ref()
                        .expect("validated terminal state carries exact terminal bytes"),
                )
            }
            FabricLedgerStateV1::Rejected => FabricLedgerCurrentResponseV1::Rejected {
                reason_code: self
                    .reason_code
                    .as_deref()
                    .expect("validated rejection carries a reason code"),
                message: self
                    .reason_message
                    .as_deref()
                    .expect("validated rejection carries a message"),
            },
            FabricLedgerStateV1::Abandoned => FabricLedgerCurrentResponseV1::Abandoned {
                reason_code: self
                    .reason_code
                    .as_deref()
                    .expect("validated abandonment carries a reason code"),
                message: self
                    .reason_message
                    .as_deref()
                    .expect("validated abandonment carries a message"),
            },
        }
    }

    fn set_state(&mut self, state: FabricLedgerStateV1) -> Result<()> {
        self.transition_sequence = self
            .transition_sequence
            .checked_add(1)
            .context("Fabric ledger transition sequence overflow")?;
        self.state = state;
        self.terminal = None;
        self.reason_code = None;
        self.reason_message = None;
        Ok(())
    }

    fn set_reason(
        &mut self,
        state: FabricLedgerStateV1,
        reason_code: String,
        reason_message: String,
    ) -> Result<()> {
        validate_reason(&reason_code, &reason_message)?;
        self.set_state(state)?;
        self.reason_code = Some(reason_code);
        self.reason_message = Some(reason_message);
        Ok(())
    }

    fn set_terminal(&mut self, terminal: FabricStoredTerminalV1) -> Result<()> {
        self.set_state(FabricLedgerStateV1::TerminalCandidate)?;
        self.terminal = Some(terminal);
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.schema != FABRIC_LEDGER_ENTRY_SCHEMA_V1 {
            bail!("unsupported Fabric ledger entry schema");
        }
        self.binding.validate()?;
        if self.transition_sequence == 0 {
            bail!("Fabric ledger transition sequence must be nonzero");
        }
        match self.state {
            FabricLedgerStateV1::TerminalCandidate => {
                let terminal = self
                    .terminal
                    .as_ref()
                    .context("terminal Fabric ledger entry omitted exact response bytes")?;
                terminal.validate()?;
                let candidate = terminal.decode_terminal_candidate()?;
                validate_terminal_binding(&self.binding, &candidate)?;
                if self.reason_code.is_some() || self.reason_message.is_some() {
                    bail!("terminal candidate ledger entry unexpectedly carries a reason");
                }
            }
            FabricLedgerStateV1::Rejected | FabricLedgerStateV1::Abandoned => {
                if self.terminal.is_some() {
                    bail!("reason terminal Fabric ledger entry carries candidate bytes");
                }
                validate_reason(
                    self.reason_code
                        .as_deref()
                        .context("terminal ledger reason code is absent")?,
                    self.reason_message
                        .as_deref()
                        .context("terminal ledger reason message is absent")?,
                )?;
            }
            _ => {
                if self.terminal.is_some()
                    || self.reason_code.is_some()
                    || self.reason_message.is_some()
                {
                    bail!("nonterminal Fabric ledger entry carries terminal material");
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FabricLedgerCurrentResponseV1<'a> {
    Received,
    Validated,
    Accepted,
    Running,
    TerminalCandidate(&'a FabricStoredTerminalV1),
    Rejected {
        reason_code: &'a str,
        message: &'a str,
    },
    Abandoned {
        reason_code: &'a str,
        message: &'a str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FabricLedgerWriteOutcomeV1 {
    Applied(FabricLedgerEntryV1),
    Duplicate(FabricLedgerEntryV1),
}

impl FabricLedgerWriteOutcomeV1 {
    pub fn entry(&self) -> &FabricLedgerEntryV1 {
        match self {
            Self::Applied(entry) | Self::Duplicate(entry) => entry,
        }
    }

    pub fn was_applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }
}

/// A sealed result from the durable acceptance transaction.
///
/// Its fields and constructors are private so downstream callers cannot mint
/// an execution grant without crossing the ledger's fsynced `Accepted`
/// transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricLedgerAcceptanceV1 {
    entry: FabricLedgerEntryV1,
    may_execute: bool,
}

impl FabricLedgerAcceptanceV1 {
    fn execute(entry: FabricLedgerEntryV1) -> Self {
        Self {
            entry,
            may_execute: true,
        }
    }

    fn existing(entry: FabricLedgerEntryV1) -> Self {
        Self {
            entry,
            may_execute: false,
        }
    }

    pub fn entry(&self) -> &FabricLedgerEntryV1 {
        &self.entry
    }

    pub fn may_execute(&self) -> bool {
        self.may_execute
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FabricLedgerConflictKindV1 {
    NonceReused,
    AttemptRebound,
    QueryBindingMismatch,
    TlsPrincipalMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricLedgerConflictV1 {
    kind: FabricLedgerConflictKindV1,
    existing: FabricAttemptBindingV1,
}

impl FabricLedgerConflictV1 {
    pub fn kind(&self) -> FabricLedgerConflictKindV1 {
        self.kind
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FabricLedgerQueryOutcomeV1 {
    Unknown,
    Found(FabricLedgerEntryV1),
    Conflict(FabricLedgerConflictV1),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct FabricLedgerBodyV1 {
    execution_cell_incarnation: ExecutionCellIncarnationV1,
    entries: Vec<FabricLedgerEntryV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct FabricLedgerSnapshotV1 {
    schema: String,
    body: FabricLedgerBodyV1,
    body_sha256: Sha256DigestV1,
}

impl FabricLedgerSnapshotV1 {
    fn new(body: FabricLedgerBodyV1) -> Result<Self> {
        validate_body(&body)?;
        let body_sha256 = ledger_body_sha256(&body)?;
        Ok(Self {
            schema: FABRIC_LEDGER_SNAPSHOT_SCHEMA_V1.to_owned(),
            body,
            body_sha256,
        })
    }

    fn validate(&self) -> Result<()> {
        if self.schema != FABRIC_LEDGER_SNAPSHOT_SCHEMA_V1 {
            bail!("unsupported Fabric attempt-ledger snapshot schema");
        }
        validate_body(&self.body)?;
        if self.body_sha256 != ledger_body_sha256(&self.body)? {
            bail!("Fabric attempt-ledger snapshot body digest mismatch");
        }
        Ok(())
    }
}

#[derive(Debug)]
struct LedgerMemoryV1 {
    body: FabricLedgerBodyV1,
    snapshot_file_sha256: Sha256DigestV1,
}

#[derive(Debug)]
struct FabricAttemptLedgerInnerV1 {
    state_base: FabricLedgerDirectoryAnchorV1,
    root: FabricLedgerDirectoryAnchorV1,
    state: Mutex<LedgerMemoryV1>,
    poisoned: AtomicBool,
    _lock: FabricLedgerRootLockV1,
}

#[derive(Debug)]
struct FabricLedgerDirectoryAnchorV1 {
    path: PathBuf,
    file: File,
    #[cfg(unix)]
    identity: FabricLedgerUnixIdentityV1,
}

#[derive(Debug)]
struct FabricLedgerRootLockV1 {
    // The persistent inode must never be unlinked while a process owns it.
    // An advisory lock belongs to this open file description for the entire
    // lifetime of the ledger (and all its clones).
    path: PathBuf,
    file: File,
    #[cfg(unix)]
    identity: FabricLedgerUnixIdentityV1,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FabricLedgerUnixIdentityV1 {
    device: u64,
    inode: u64,
}

impl FabricLedgerDirectoryAnchorV1 {
    fn open(path: PathBuf, label: &'static str) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let file = options
            .open(&path)
            .with_context(|| format!("failed to pin {label} `{}`", path.display()))?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            bail!("{label} `{}` is not a directory", path.display());
        }
        #[cfg(unix)]
        let identity = unix_identity(&metadata);
        let anchor = Self {
            path,
            file,
            #[cfg(unix)]
            identity,
        };
        anchor.verify(label)?;
        Ok(anchor)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn verify(&self, label: &'static str) -> Result<()> {
        let path_metadata = fs::symlink_metadata(&self.path)
            .with_context(|| format!("{label} pathname is no longer reachable"))?;
        let file_metadata = self.file.metadata()?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_dir()
            || !file_metadata.is_dir()
        {
            bail!("{label} pathname no longer names the pinned directory");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            if unix_identity(&path_metadata) != self.identity
                || unix_identity(&file_metadata) != self.identity
                || path_metadata.nlink() == 0
                || file_metadata.nlink() == 0
            {
                bail!("{label} device/inode identity changed or was unlinked");
            }
            if path_metadata.uid() != unsafe { libc::geteuid() }
                || path_metadata.permissions().mode() & 0o777 != 0o700
            {
                bail!("{label} must remain owned by this user with mode 0700");
            }
        }
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        self.verify("Fabric ledger directory")?;
        self.file
            .sync_all()
            .with_context(|| format!("failed to sync directory `{}`", self.path.display()))?;
        self.verify("Fabric ledger directory")
    }
}

impl FabricLedgerRootLockV1 {
    fn verify(&self) -> Result<()> {
        let path_metadata = fs::symlink_metadata(&self.path)
            .context("Fabric attempt-ledger lock pathname is no longer reachable")?;
        let file_metadata = self.file.metadata()?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_file()
            || !file_metadata.is_file()
        {
            bail!("Fabric attempt-ledger lock pathname no longer names the pinned file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            if unix_identity(&path_metadata) != self.identity
                || unix_identity(&file_metadata) != self.identity
                || path_metadata.nlink() != 1
                || file_metadata.nlink() != 1
            {
                bail!(
                    "Fabric attempt-ledger lock identity changed, was unlinked, or was hard-linked"
                );
            }
            if path_metadata.uid() != unsafe { libc::geteuid() }
                || path_metadata.permissions().mode() & 0o777 != 0o600
            {
                bail!("Fabric attempt-ledger lock must remain owned by this user with mode 0600");
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn unix_identity(metadata: &fs::Metadata) -> FabricLedgerUnixIdentityV1 {
    use std::os::unix::fs::MetadataExt;

    FabricLedgerUnixIdentityV1 {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

impl Drop for FabricLedgerRootLockV1 {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Clone, Debug)]
pub struct FabricAttemptLedgerV1 {
    inner: Arc<FabricAttemptLedgerInnerV1>,
}

impl FabricAttemptLedgerV1 {
    /// Open `state_base/fabric-v1`.  Every successful open advances the durable
    /// execution-cell incarnation and abandons incomplete records from the
    /// preceding incarnation before returning.
    pub fn open(state_base: impl AsRef<Path>) -> Result<Self> {
        let requested_state_base = state_base.as_ref();
        ensure_directory_exists(requested_state_base)?;
        let state_base = fs::canonicalize(requested_state_base).with_context(|| {
            format!(
                "failed to resolve Fabric state base `{}`",
                requested_state_base.display()
            )
        })?;
        validate_secure_ancestor_chain(&state_base)?;
        validate_private_directory(&state_base, "Fabric state base")?;
        let state_base = FabricLedgerDirectoryAnchorV1::open(state_base, "Fabric state base")?;
        let root_path = state_base.path().join(FABRIC_LEDGER_DIRECTORY_V1);
        let root = &root_path;
        ensure_private_fabric_root(&root)?;
        state_base.sync()?;
        let root = FabricLedgerDirectoryAnchorV1::open(root_path, "Fabric attempt-ledger root")?;
        let lock = acquire_root_lock(&root)?;
        reconcile_stale_temp_files(&root)?;

        let snapshot_path = root.path().join(FABRIC_LEDGER_SNAPSHOT_FILE_V1);
        let (body, snapshot_file_sha256) = match path_kind(&snapshot_path)? {
            PathKindV1::Absent => {
                let body = FabricLedgerBodyV1 {
                    execution_cell_incarnation: ExecutionCellIncarnationV1::new(1)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?,
                    entries: Vec::new(),
                };
                let digest = persist_snapshot(&root, &body)?;
                (body, digest)
            }
            PathKindV1::RegularFile => {
                let (mut body, _) = read_snapshot(&snapshot_path)?;
                let next = body
                    .execution_cell_incarnation
                    .get()
                    .checked_add(1)
                    .context("Fabric execution-cell incarnation overflow")?;
                body.execution_cell_incarnation = ExecutionCellIncarnationV1::new(next)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                for entry in &mut body.entries {
                    if entry.state.is_incomplete() {
                        entry.set_reason(
                            FabricLedgerStateV1::Abandoned,
                            "provider-restarted".to_owned(),
                            "incomplete attempt belongs to an older execution-cell incarnation"
                                .to_owned(),
                        )?;
                    }
                }
                let digest = persist_snapshot(&root, &body)?;
                (body, digest)
            }
            PathKindV1::Other => {
                bail!(
                    "Fabric attempt-ledger snapshot `{}` is not a regular file",
                    snapshot_path.display()
                )
            }
        };

        Ok(Self {
            inner: Arc::new(FabricAttemptLedgerInnerV1 {
                state_base,
                root,
                state: Mutex::new(LedgerMemoryV1 {
                    body,
                    snapshot_file_sha256,
                }),
                poisoned: AtomicBool::new(false),
                _lock: lock,
            }),
        })
    }

    pub fn root(&self) -> &Path {
        self.inner.root.path()
    }

    pub fn is_poisoned(&self) -> bool {
        self.inner.poisoned.load(Ordering::Acquire)
    }

    pub fn execution_cell_incarnation(&self) -> Result<ExecutionCellIncarnationV1> {
        let guard = self.lock_verified()?;
        Ok(guard.body.execution_cell_incarnation)
    }

    pub fn record_received(
        &self,
        binding: FabricAttemptBindingV1,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        self.write_binding(binding, FabricLedgerStateV1::Received)
    }

    pub fn record_validated(
        &self,
        binding: &FabricAttemptBindingV1,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        self.transition_exact(binding, |entry| match entry.state {
            FabricLedgerStateV1::Received => {
                entry.set_state(FabricLedgerStateV1::Validated)?;
                Ok(true)
            }
            FabricLedgerStateV1::Validated
            | FabricLedgerStateV1::Accepted
            | FabricLedgerStateV1::Running
            | FabricLedgerStateV1::TerminalCandidate
            | FabricLedgerStateV1::Rejected
            | FabricLedgerStateV1::Abandoned => Ok(false),
        })
    }

    /// Atomically consume the issuer-scoped nonce and attempt coordinate and
    /// publish the exact `Accepted` binding.  `Execute` is returned only after
    /// the new snapshot and its parent directory are durably synced.
    pub fn consume_and_accept(
        &self,
        binding: FabricAttemptBindingV1,
    ) -> Result<FabricLedgerAcceptanceV1> {
        self.with_mutation(|body| match locate_binding(&body.entries, &binding) {
            BindingLookupV1::Exact(index) => {
                let entry = &mut body.entries[index];
                match entry.state {
                    FabricLedgerStateV1::Validated => {
                        require_current_incarnation(body.execution_cell_incarnation, &binding)?;
                        entry.set_state(FabricLedgerStateV1::Accepted)?;
                        Ok(MutationV1::Changed(FabricLedgerAcceptanceV1::execute(
                            entry.clone(),
                        )))
                    }
                    _ => Ok(MutationV1::Unchanged(FabricLedgerAcceptanceV1::existing(
                        entry.clone(),
                    ))),
                }
            }
            BindingLookupV1::Conflict(conflict) => Err(conflict_error(&conflict)),
            BindingLookupV1::Unknown => {
                bail!("Fabric attempt must be durably Validated before acceptance")
            }
        })
    }

    pub fn mark_running(
        &self,
        binding: &FabricAttemptBindingV1,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        self.transition_exact(binding, |entry| match entry.state {
            FabricLedgerStateV1::Accepted => {
                entry.set_state(FabricLedgerStateV1::Running)?;
                Ok(true)
            }
            FabricLedgerStateV1::Running | FabricLedgerStateV1::TerminalCandidate => Ok(false),
            other => bail!("cannot mark Fabric attempt Running from {other:?}"),
        })
    }

    pub fn record_terminal_candidate(
        &self,
        binding: &FabricAttemptBindingV1,
        candidate: &FabricTerminalCandidateV1,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        let terminal = FabricStoredTerminalV1::from_candidate(candidate)?;
        validate_terminal_binding(binding, candidate)?;
        self.transition_exact(binding, |entry| match entry.state {
            FabricLedgerStateV1::Running => {
                entry.set_terminal(terminal.clone())?;
                Ok(true)
            }
            FabricLedgerStateV1::TerminalCandidate => {
                if entry.terminal.as_ref() != Some(&terminal) {
                    bail!("Fabric terminal candidate conflicts with durable terminal bytes");
                }
                Ok(false)
            }
            other => bail!("cannot record Fabric terminal candidate from {other:?}"),
        })
    }

    pub fn record_rejected(
        &self,
        binding: FabricAttemptBindingV1,
        reason_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        self.record_reason_terminal(
            binding,
            FabricLedgerStateV1::Rejected,
            reason_code.into(),
            message.into(),
        )
    }

    /// Persist a semantic/authority rejection only while the attempt has not
    /// crossed the durable Accepted barrier. A concurrent identical submitter
    /// that already acquired execution authority wins; this method then
    /// returns its current entry without replacing Running or terminal state.
    pub fn record_preaccept_rejected(
        &self,
        binding: FabricAttemptBindingV1,
        reason_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        let reason_code = reason_code.into();
        let message = message.into();
        validate_reason(&reason_code, &message)?;
        self.with_mutation(|body| match locate_binding(&body.entries, &binding) {
            BindingLookupV1::Exact(index) => {
                let entry = &mut body.entries[index];
                match entry.state {
                    FabricLedgerStateV1::Received | FabricLedgerStateV1::Validated => {
                        entry.set_reason(FabricLedgerStateV1::Rejected, reason_code, message)?;
                        Ok(MutationV1::Changed(FabricLedgerWriteOutcomeV1::Applied(
                            entry.clone(),
                        )))
                    }
                    _ => Ok(MutationV1::Unchanged(
                        FabricLedgerWriteOutcomeV1::Duplicate(entry.clone()),
                    )),
                }
            }
            BindingLookupV1::Conflict(conflict) => Err(conflict_error(&conflict)),
            BindingLookupV1::Unknown => {
                require_current_incarnation(body.execution_cell_incarnation, &binding)?;
                if body.entries.len() >= MAX_FABRIC_LEDGER_ENTRIES_V1 {
                    bail!("Fabric attempt ledger reached its V1 entry bound");
                }
                let mut entry = FabricLedgerEntryV1::new(binding, FabricLedgerStateV1::Rejected);
                entry.reason_code = Some(reason_code);
                entry.reason_message = Some(message);
                body.entries.push(entry.clone());
                body.entries
                    .sort_by(|left, right| left.binding.compare_key(&right.binding));
                Ok(MutationV1::Changed(FabricLedgerWriteOutcomeV1::Applied(
                    entry,
                )))
            }
        })
    }

    #[cfg(test)]
    fn record_abandoned(
        &self,
        binding: FabricAttemptBindingV1,
        reason_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        self.record_reason_terminal(
            binding,
            FabricLedgerStateV1::Abandoned,
            reason_code.into(),
            message.into(),
        )
    }

    /// Query an attempt only for the authenticated TLS principal that created
    /// its immutable binding.  There is deliberately no principal-agnostic
    /// public lookup surface.
    pub fn query(
        &self,
        query: &FabricAttemptQueryV1,
        tls_client_principal_sha256: &SemanticDigestV1,
    ) -> Result<FabricLedgerQueryOutcomeV1> {
        let guard = self.lock_verified()?;
        for entry in &guard.body.entries {
            let binding = &entry.binding;
            let same_issuer = binding.issuer_key_id() == query.issuer_key_id();
            let same_attempt = binding.attempt() == query.attempt();
            let same_nonce = binding.lease_nonce() == query.lease_nonce();
            if same_issuer && same_attempt && same_nonce {
                if binding.submission_binding_sha256() == query.submission_binding_sha256() {
                    if binding.tls_client_principal_sha256() != tls_client_principal_sha256 {
                        return Ok(FabricLedgerQueryOutcomeV1::Conflict(
                            FabricLedgerConflictV1 {
                                kind: FabricLedgerConflictKindV1::TlsPrincipalMismatch,
                                existing: binding.clone(),
                            },
                        ));
                    }
                    return Ok(FabricLedgerQueryOutcomeV1::Found(entry.clone()));
                }
                return Ok(FabricLedgerQueryOutcomeV1::Conflict(
                    FabricLedgerConflictV1 {
                        kind: FabricLedgerConflictKindV1::QueryBindingMismatch,
                        existing: binding.clone(),
                    },
                ));
            }
            if same_issuer && same_nonce {
                return Ok(FabricLedgerQueryOutcomeV1::Conflict(
                    FabricLedgerConflictV1 {
                        kind: FabricLedgerConflictKindV1::NonceReused,
                        existing: binding.clone(),
                    },
                ));
            }
            if same_issuer && same_attempt {
                return Ok(FabricLedgerQueryOutcomeV1::Conflict(
                    FabricLedgerConflictV1 {
                        kind: FabricLedgerConflictKindV1::AttemptRebound,
                        existing: binding.clone(),
                    },
                ));
            }
        }
        Ok(FabricLedgerQueryOutcomeV1::Unknown)
    }

    fn write_binding(
        &self,
        binding: FabricAttemptBindingV1,
        state: FabricLedgerStateV1,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        self.with_mutation(|body| match locate_binding(&body.entries, &binding) {
            BindingLookupV1::Exact(index) => Ok(MutationV1::Unchanged(
                FabricLedgerWriteOutcomeV1::Duplicate(body.entries[index].clone()),
            )),
            BindingLookupV1::Conflict(conflict) => Err(conflict_error(&conflict)),
            BindingLookupV1::Unknown => {
                require_current_incarnation(body.execution_cell_incarnation, &binding)?;
                if body.entries.len() >= MAX_FABRIC_LEDGER_ENTRIES_V1 {
                    bail!("Fabric attempt ledger reached its V1 entry bound");
                }
                let entry = FabricLedgerEntryV1::new(binding, state);
                body.entries.push(entry.clone());
                body.entries
                    .sort_by(|left, right| left.binding.compare_key(&right.binding));
                Ok(MutationV1::Changed(FabricLedgerWriteOutcomeV1::Applied(
                    entry,
                )))
            }
        })
    }

    fn transition_exact<F>(
        &self,
        binding: &FabricAttemptBindingV1,
        transition: F,
    ) -> Result<FabricLedgerWriteOutcomeV1>
    where
        F: FnOnce(&mut FabricLedgerEntryV1) -> Result<bool>,
    {
        self.with_mutation(|body| match locate_binding(&body.entries, binding) {
            BindingLookupV1::Exact(index) => {
                let entry = &mut body.entries[index];
                if transition(entry)? {
                    Ok(MutationV1::Changed(FabricLedgerWriteOutcomeV1::Applied(
                        entry.clone(),
                    )))
                } else {
                    Ok(MutationV1::Unchanged(
                        FabricLedgerWriteOutcomeV1::Duplicate(entry.clone()),
                    ))
                }
            }
            BindingLookupV1::Conflict(conflict) => Err(conflict_error(&conflict)),
            BindingLookupV1::Unknown => bail!("Fabric attempt is unknown to the durable ledger"),
        })
    }

    fn record_reason_terminal(
        &self,
        binding: FabricAttemptBindingV1,
        state: FabricLedgerStateV1,
        reason_code: String,
        message: String,
    ) -> Result<FabricLedgerWriteOutcomeV1> {
        validate_reason(&reason_code, &message)?;
        self.with_mutation(|body| match locate_binding(&body.entries, &binding) {
            BindingLookupV1::Exact(index) => {
                let entry = &mut body.entries[index];
                if entry.state == state {
                    if entry.reason_code.as_deref() != Some(reason_code.as_str())
                        || entry.reason_message.as_deref() != Some(message.as_str())
                    {
                        bail!("Fabric terminal reason conflicts with durable terminal state");
                    }
                    return Ok(MutationV1::Unchanged(
                        FabricLedgerWriteOutcomeV1::Duplicate(entry.clone()),
                    ));
                }
                if !entry.state.is_incomplete() {
                    bail!("cannot replace an existing Fabric terminal state");
                }
                entry.set_reason(state, reason_code, message)?;
                Ok(MutationV1::Changed(FabricLedgerWriteOutcomeV1::Applied(
                    entry.clone(),
                )))
            }
            BindingLookupV1::Conflict(conflict) => Err(conflict_error(&conflict)),
            BindingLookupV1::Unknown => {
                require_current_incarnation(body.execution_cell_incarnation, &binding)?;
                if body.entries.len() >= MAX_FABRIC_LEDGER_ENTRIES_V1 {
                    bail!("Fabric attempt ledger reached its V1 entry bound");
                }
                let mut entry = FabricLedgerEntryV1::new(binding, state);
                entry.reason_code = Some(reason_code);
                entry.reason_message = Some(message);
                body.entries.push(entry.clone());
                body.entries
                    .sort_by(|left, right| left.binding.compare_key(&right.binding));
                Ok(MutationV1::Changed(FabricLedgerWriteOutcomeV1::Applied(
                    entry,
                )))
            }
        })
    }

    fn lock_verified(&self) -> Result<MutexGuard<'_, LedgerMemoryV1>> {
        self.require_healthy()?;
        let guard = self
            .inner
            .state
            .lock()
            .map_err(|_| self.poison("Fabric attempt-ledger mutex was poisoned"))?;
        self.require_healthy()?;
        let verified = self
            .inner
            .state_base
            .verify("Fabric state base")
            .and_then(|_| self.inner.root.verify("Fabric attempt-ledger root"))
            .and_then(|_| self.inner._lock.verify())
            .and_then(|_| verify_cached_snapshot(&self.inner.root, &guard));
        if let Err(error) = verified {
            return Err(self.poison(format!(
                "Fabric attempt ledger changed or became corrupt: {error:#}"
            )));
        }
        Ok(guard)
    }

    fn with_mutation<T>(
        &self,
        mutation: impl FnOnce(&mut FabricLedgerBodyV1) -> Result<MutationV1<T>>,
    ) -> Result<T> {
        let mut guard = self.lock_verified()?;
        let mut next = guard.body.clone();
        let mutation = mutation(&mut next)?;
        match mutation {
            MutationV1::Unchanged(value) => Ok(value),
            MutationV1::Changed(value) => {
                validate_body(&next)?;
                let digest = persist_snapshot(&self.inner.root, &next).map_err(|error| {
                    self.poison(format!(
                        "Fabric attempt-ledger durability became ambiguous: {error:#}"
                    ))
                })?;
                self.inner
                    .state_base
                    .verify("Fabric state base")
                    .and_then(|_| self.inner.root.verify("Fabric attempt-ledger root"))
                    .and_then(|_| self.inner._lock.verify())
                    .map_err(|error| {
                        self.poison(format!(
                            "Fabric attempt-ledger path identity became ambiguous: {error:#}"
                        ))
                    })?;
                guard.body = next;
                guard.snapshot_file_sha256 = digest;
                Ok(value)
            }
        }
    }

    fn require_healthy(&self) -> Result<()> {
        if self.is_poisoned() {
            bail!("Fabric attempt ledger is poisoned and must be reopened");
        }
        Ok(())
    }

    fn poison(&self, detail: impl Into<String>) -> anyhow::Error {
        self.inner.poisoned.store(true, Ordering::Release);
        anyhow::anyhow!(detail.into())
    }
}

enum MutationV1<T> {
    Changed(T),
    Unchanged(T),
}

enum BindingLookupV1 {
    Exact(usize),
    Conflict(FabricLedgerConflictV1),
    Unknown,
}

fn locate_binding(
    entries: &[FabricLedgerEntryV1],
    requested: &FabricAttemptBindingV1,
) -> BindingLookupV1 {
    for (index, entry) in entries.iter().enumerate() {
        let existing = &entry.binding;
        if existing == requested {
            return BindingLookupV1::Exact(index);
        }
        if existing.issuer_key_id == requested.issuer_key_id
            && existing.lease_nonce == requested.lease_nonce
        {
            return BindingLookupV1::Conflict(FabricLedgerConflictV1 {
                kind: FabricLedgerConflictKindV1::NonceReused,
                existing: existing.clone(),
            });
        }
        if existing.issuer_key_id == requested.issuer_key_id
            && existing.attempt == requested.attempt
        {
            return BindingLookupV1::Conflict(FabricLedgerConflictV1 {
                kind: FabricLedgerConflictKindV1::AttemptRebound,
                existing: existing.clone(),
            });
        }
    }
    BindingLookupV1::Unknown
}

fn conflict_error(conflict: &FabricLedgerConflictV1) -> anyhow::Error {
    match conflict.kind {
        FabricLedgerConflictKindV1::NonceReused => anyhow::anyhow!(
            "Fabric lease nonce is already consumed by a different attempt or binding"
        ),
        FabricLedgerConflictKindV1::AttemptRebound => anyhow::anyhow!(
            "Fabric attempt coordinate is already bound to a different nonce or command"
        ),
        FabricLedgerConflictKindV1::QueryBindingMismatch => {
            anyhow::anyhow!("Fabric query binding conflicts with the durable attempt")
        }
        FabricLedgerConflictKindV1::TlsPrincipalMismatch => {
            anyhow::anyhow!("Fabric query TLS principal conflicts with the durable attempt")
        }
    }
}

fn require_current_incarnation(
    current: ExecutionCellIncarnationV1,
    binding: &FabricAttemptBindingV1,
) -> Result<()> {
    if binding.execution_cell_incarnation != current {
        bail!(
            "Fabric attempt targets execution-cell incarnation {}, but the durable provider is incarnation {}",
            binding.execution_cell_incarnation.get(),
            current.get()
        );
    }
    Ok(())
}

fn validate_terminal_binding(
    binding: &FabricAttemptBindingV1,
    terminal: &FabricTerminalCandidateV1,
) -> Result<()> {
    let receipt = terminal.signed_receipt().receipt();
    if receipt.issuer_key_id() != binding.issuer_key_id()
        || receipt.attempt() != binding.attempt()
        || receipt.lease_nonce() != binding.lease_nonce()
        || receipt.submission_binding_sha256() != binding.submission_binding_sha256()
        || receipt.capsule_sha256() != binding.capsule_sha256()
        || receipt.source_closure_sha256() != binding.source_closure_sha256()
        || receipt.node_id() != binding.node_id()
        || receipt.node_generation() != binding.node_generation()
        || receipt.execution_cell_incarnation() != binding.execution_cell_incarnation()
    {
        bail!("Fabric terminal candidate does not match its durable attempt binding");
    }
    Ok(())
}

fn validate_body(body: &FabricLedgerBodyV1) -> Result<()> {
    if body.entries.len() > MAX_FABRIC_LEDGER_ENTRIES_V1 {
        bail!("Fabric attempt-ledger snapshot exceeds its V1 entry bound");
    }
    for entry in &body.entries {
        entry.validate()?;
        if entry.binding.execution_cell_incarnation.get() > body.execution_cell_incarnation.get() {
            bail!("Fabric ledger entry belongs to a future execution-cell incarnation");
        }
    }
    for pair in body.entries.windows(2) {
        if pair[0].binding.compare_key(&pair[1].binding) != CompareOrdering::Less {
            bail!("Fabric ledger entries are duplicated or not in canonical key order");
        }
    }
    // Re-run both replay indexes independently.  Sorting is by attempt first,
    // so nonce collisions are not necessarily adjacent.
    for (left_index, left) in body.entries.iter().enumerate() {
        for right in body.entries.iter().skip(left_index + 1) {
            if left.binding.issuer_key_id == right.binding.issuer_key_id
                && left.binding.lease_nonce == right.binding.lease_nonce
            {
                bail!("Fabric ledger contains a reused issuer-scoped nonce");
            }
            if left.binding.issuer_key_id == right.binding.issuer_key_id
                && left.binding.attempt == right.binding.attempt
            {
                bail!("Fabric ledger contains a rebound issuer-scoped attempt");
            }
        }
    }
    Ok(())
}

fn validate_reason(code: &str, message: &str) -> Result<()> {
    if code.is_empty()
        || code.len() > MAX_FABRIC_LEDGER_REASON_CODE_BYTES_V1
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("Fabric ledger reason code is invalid");
    }
    if message.is_empty()
        || message.len() > MAX_FABRIC_LEDGER_REASON_MESSAGE_BYTES_V1
        || message.chars().any(|character| character.is_control())
    {
        bail!("Fabric ledger reason message is invalid");
    }
    Ok(())
}

fn ledger_body_sha256(body: &FabricLedgerBodyV1) -> Result<Sha256DigestV1> {
    let bytes = encode(body).context("failed to encode Fabric ledger body")?;
    domain_sha256(FABRIC_LEDGER_BODY_DIGEST_DOMAIN_V1, &bytes)
}

fn terminal_parts_sha256(header_bytes: &[u8], candidate_bytes: &[u8]) -> Result<Sha256DigestV1> {
    let header_length =
        u64::try_from(header_bytes.len()).context("Fabric terminal header length exceeds u64")?;
    let candidate_length = u64::try_from(candidate_bytes.len())
        .context("Fabric terminal candidate length exceeds u64")?;
    let mut hash = Sha256::new();
    hash.update(FABRIC_LEDGER_TERMINAL_DIGEST_DOMAIN_V1);
    hash.update(header_length.to_be_bytes());
    hash.update(header_bytes);
    hash.update(candidate_length.to_be_bytes());
    hash.update(candidate_bytes);
    Ok(hash.finalize().into())
}

fn domain_sha256(domain: &[u8], bytes: &[u8]) -> Result<Sha256DigestV1> {
    let length = u64::try_from(bytes.len()).context("Fabric ledger digest input exceeds u64")?;
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(length.to_be_bytes());
    hash.update(bytes);
    Ok(hash.finalize().into())
}

fn verify_cached_snapshot(
    root: &FabricLedgerDirectoryAnchorV1,
    cached: &LedgerMemoryV1,
) -> Result<()> {
    root.verify("Fabric attempt-ledger root")?;
    let path = root.path().join(FABRIC_LEDGER_SNAPSHOT_FILE_V1);
    let (body, digest) = read_snapshot(&path)?;
    if body != cached.body || digest != cached.snapshot_file_sha256 {
        bail!("Fabric attempt-ledger snapshot differs from its validated in-memory image");
    }
    Ok(())
}

fn read_snapshot(path: &Path) -> Result<(FabricLedgerBodyV1, Sha256DigestV1)> {
    require_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open Fabric ledger `{}` safely", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("Fabric ledger snapshot is not a regular file");
    }
    let byte_length: usize = metadata
        .len()
        .try_into()
        .context("Fabric ledger snapshot length exceeds this process")?;
    if byte_length == 0 || byte_length > MAX_FABRIC_LEDGER_SNAPSHOT_BYTES_V1 {
        bail!("Fabric ledger snapshot is outside its V1 byte bounds");
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_length)
        .context("failed to reserve bounded Fabric ledger snapshot")?;
    file.take((MAX_FABRIC_LEDGER_SNAPSHOT_BYTES_V1 + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() != byte_length {
        bail!("Fabric ledger snapshot changed while it was read");
    }
    let snapshot: FabricLedgerSnapshotV1 = decode_bounded(
        &bytes,
        DecodeLimits {
            max_bytes: MAX_FABRIC_LEDGER_SNAPSHOT_BYTES_V1,
            max_items: MAX_FABRIC_LEDGER_DECODE_ITEMS_V1,
            max_depth: MAX_FABRIC_LEDGER_DECODE_DEPTH_V1,
        },
    )
    .context("Fabric ledger snapshot is not bounded canonical CBOR")?;
    snapshot.validate()?;
    let canonical = encode(&snapshot).context("failed to re-encode Fabric ledger snapshot")?;
    if canonical != bytes {
        bail!("Fabric ledger snapshot is not canonical CBOR");
    }
    Ok((snapshot.body, Sha256::digest(&bytes).into()))
}

fn persist_snapshot(
    root: &FabricLedgerDirectoryAnchorV1,
    body: &FabricLedgerBodyV1,
) -> Result<Sha256DigestV1> {
    root.verify("Fabric attempt-ledger root")?;
    let snapshot = FabricLedgerSnapshotV1::new(body.clone())?;
    let bytes = encode(&snapshot).context("failed to encode Fabric attempt-ledger snapshot")?;
    if bytes.is_empty() || bytes.len() > MAX_FABRIC_LEDGER_SNAPSHOT_BYTES_V1 {
        bail!("Fabric attempt-ledger snapshot exceeds its V1 byte bound");
    }

    let final_path = root.path().join(FABRIC_LEDGER_SNAPSHOT_FILE_V1);
    match path_kind(&final_path)? {
        PathKindV1::Absent | PathKindV1::RegularFile => {}
        PathKindV1::Other => bail!("refusing to replace non-regular Fabric ledger snapshot"),
    }

    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).context("failed to create Fabric ledger staging identity")?;
    let temp_path = root.path().join(format!(
        "{FABRIC_LEDGER_TEMP_PREFIX_V1}{}{FABRIC_LEDGER_TEMP_SUFFIX_V1}",
        hex::encode(random)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(&temp_path).with_context(|| {
        format!(
            "failed to create Fabric ledger staging file `{}`",
            temp_path.display()
        )
    })?;

    let staged = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        if !file.metadata()?.is_file() {
            bail!("Fabric ledger staging path is not a regular file");
        }
        fs::rename(&temp_path, &final_path)
            .context("failed to atomically publish Fabric attempt-ledger snapshot")?;
        root.sync()?;
        let actual = read_exact_regular_file(&final_path, bytes.len())?;
        if actual != bytes {
            bail!("published Fabric ledger snapshot differs from exact staged bytes");
        }
        Ok(())
    })();

    if let Err(error) = staged {
        match fs::symlink_metadata(&temp_path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(error)
                    .context("Fabric ledger publication failed and staging path changed type")
            }
            Ok(_) => {
                fs::remove_file(&temp_path).with_context(|| {
                    format!("Fabric ledger publication failed ({error:#}); staging cleanup failed")
                })?;
                root.sync()?;
            }
            Err(inspect) if inspect.kind() == std::io::ErrorKind::NotFound => {}
            Err(inspect) => {
                return Err(error).context(format!(
                    "Fabric ledger publication failed and staging cleanup is unprovable: {inspect}"
                ))
            }
        }
        return Err(error);
    }

    root.verify("Fabric attempt-ledger root")?;
    Ok(Sha256::digest(&bytes).into())
}

fn ensure_directory_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "Fabric state base `{}` is not a real directory",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty() && *parent != path)
                .unwrap_or_else(|| Path::new("."));
            ensure_directory_exists(parent)?;
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).with_context(|| {
                format!("failed to create Fabric state base `{}`", path.display())
            })?;
            sync_directory(parent)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn ensure_private_fabric_root(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) => validate_private_root(root, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(root).with_context(|| {
                format!("failed to create Fabric ledger root `{}`", root.display())
            })?;
            let metadata = fs::symlink_metadata(root)?;
            validate_private_root(root, &metadata)?;
            let parent = root
                .parent()
                .context("Fabric ledger root has no parent directory")?;
            sync_directory(parent)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_private_root(root: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "Fabric ledger root `{}` must be a real directory",
            root.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            bail!(
                "Fabric ledger root `{}` must be owned by this user with mode 0700",
                root.display()
            );
        }
    }
    Ok(())
}

fn validate_private_directory(path: &Path, label: &'static str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{label} `{}` must be a real directory", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            bail!(
                "{label} `{}` must be owned by this user with mode 0700",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_secure_ancestor_chain(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).with_context(|| {
            format!(
                "failed to inspect Fabric state ancestor `{}`",
                ancestor.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "Fabric state ancestor `{}` is not a real directory",
                ancestor.display()
            );
        }
        let mode = metadata.permissions().mode();
        if mode & 0o022 != 0 && mode & 0o1000 == 0 {
            bail!(
                "Fabric state ancestor `{}` is writable by another principal without the sticky bit",
                ancestor.display()
            );
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_secure_ancestor_chain(_path: &Path) -> Result<()> {
    Ok(())
}

fn acquire_root_lock(root: &FabricLedgerDirectoryAnchorV1) -> Result<FabricLedgerRootLockV1> {
    root.verify("Fabric attempt-ledger root")?;
    let path = root.path().join(FABRIC_LEDGER_LOCK_FILE_V1);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&path).with_context(|| {
        format!(
            "failed to open Fabric attempt-ledger root lock `{}`",
            path.display()
        )
    })?;
    #[cfg(unix)]
    let identity = unix_identity(&file.metadata()?);
    let mut lock = FabricLedgerRootLockV1 {
        path,
        file,
        #[cfg(unix)]
        identity,
    };
    // Verify the existing inode before chmod, truncation, or any write.  A
    // hard-linked attacker-controlled file must remain byte-for-byte untouched.
    lock.verify()?;
    lock.file.try_lock_exclusive().with_context(|| {
        format!(
            "Fabric attempt-ledger root `{}` is already locked",
            root.path().display()
        )
    })?;
    lock.file.set_len(0)?;
    writeln!(lock.file, "pid={}", std::process::id())?;
    lock.file.sync_all()?;
    root.sync()?;
    lock.verify()?;
    Ok(lock)
}

fn reconcile_stale_temp_files(root: &FabricLedgerDirectoryAnchorV1) -> Result<()> {
    root.verify("Fabric attempt-ledger root")?;
    let mut removed = false;
    for entry in fs::read_dir(root.path())? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Fabric ledger root contains a non-UTF-8 entry"))?;
        if matches!(
            name.as_str(),
            FABRIC_LEDGER_LOCK_FILE_V1 | FABRIC_LEDGER_SNAPSHOT_FILE_V1
        ) {
            continue;
        }
        let random = name
            .strip_prefix(FABRIC_LEDGER_TEMP_PREFIX_V1)
            .and_then(|value| value.strip_suffix(FABRIC_LEDGER_TEMP_SUFFIX_V1));
        let Some(random) = random else {
            bail!("Fabric ledger root contains unexpected entry `{name}`");
        };
        if random.len() != 32
            || !random
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("Fabric ledger root contains malformed staging entry `{name}`");
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("Fabric ledger staging entry `{name}` is not a regular file");
        }
        fs::remove_file(&path).with_context(|| {
            format!("failed to remove stale Fabric ledger staging file `{name}`")
        })?;
        removed = true;
    }
    if removed {
        root.sync()?;
    }
    root.verify("Fabric attempt-ledger root")?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathKindV1 {
    Absent,
    RegularFile,
    Other,
}

fn path_kind(path: &Path) -> Result<PathKindV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(PathKindV1::Other),
        Ok(metadata) if metadata.is_file() => Ok(PathKindV1::RegularFile),
        Ok(_) => Ok(PathKindV1::Other),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(PathKindV1::Absent),
        Err(error) => Err(error.into()),
    }
}

fn require_regular_file(path: &Path) -> Result<()> {
    if path_kind(path)? != PathKindV1::RegularFile {
        bail!(
            "Fabric ledger path `{}` is not a regular file",
            path.display()
        );
    }
    Ok(())
}

fn read_exact_regular_file(path: &Path, expected_bytes: usize) -> Result<Vec<u8>> {
    require_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() != expected_bytes as u64 {
        bail!("Fabric ledger file length differs from exact expected bytes");
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(expected_bytes)?;
    file.read_to_end(&mut bytes)?;
    if bytes.len() != expected_bytes {
        bail!("Fabric ledger file changed while it was verified");
    }
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory `{}` for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_fabric::{
        encode_execution_candidate_v1, encode_execution_capsule_v1, CandidateOutcomeV1,
        CandidateOutputV1, ExecutionCandidateV1, ExecutionCapsuleV1, ExecutionIdV1,
        ExecutionLimitsV1, InputBindingV1, InputManifestV1, LogicalTaskIdV1, OutputContractV1,
        OutputFidelityV1, OutputValueKindV1, RendererPartV1, SourceClosedRendererV1,
        TrustedInlineRendererV1,
    };
    use crate::execution_fabric_authority::{
        encode_fabric_response_v1, FabricSigningKeyV1, FabricSourceClosureV1,
        FabricTargetBindingV1, PlacementLeaseV3, FABRIC_SOURCE_CLOSURE_DIALECT_V1,
        FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
    };
    use crate::placement_protocol::UnixMillisV1;
    use crate::value::OText;
    use crate::world::{PortableOValue, PortableValueRecord, MAX_OVALUE_RECORD_BYTES};

    fn digest(byte: u8) -> Sha256DigestV1 {
        [byte; 32]
    }

    fn semantic(byte: u8) -> SemanticDigestV1 {
        SemanticDigestV1::from_sha256(hex::encode([byte; 32])).unwrap()
    }

    fn private_tempdir() -> Result<tempfile::TempDir> {
        let directory = tempfile::tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        }
        Ok(directory)
    }

    fn binding(
        incarnation: ExecutionCellIncarnationV1,
        attempt_generation: u64,
        nonce_byte: u8,
        binding_byte: u8,
    ) -> FabricAttemptBindingV1 {
        let execution = ExecutionIdV1::new(digest(1)).unwrap();
        let task = LogicalTaskIdV1::new(execution, digest(2)).unwrap();
        FabricAttemptBindingV1::new(
            semantic(3),
            AttemptIdV1::new(task, attempt_generation).unwrap(),
            semantic(nonce_byte),
            semantic(4),
            digest(binding_byte),
            digest(5),
            digest(6),
            "node-a",
            GenerationV1::new(7).unwrap(),
            incarnation,
        )
        .unwrap()
    }

    fn validate_for_accept(
        ledger: &FabricAttemptLedgerV1,
        binding: &FabricAttemptBindingV1,
    ) -> Result<()> {
        ledger.record_received(binding.clone())?;
        ledger.record_validated(binding)?;
        Ok(())
    }

    fn terminal_fixture(
        incarnation: ExecutionCellIncarnationV1,
    ) -> Result<(
        FabricSubmissionV1,
        FabricTerminalCandidateV1,
        SemanticDigestV1,
    )> {
        const DEADLINE_UNIX_MS: u64 = 2_000_000_000_000;

        let execution = ExecutionIdV1::new(digest(31))?;
        let task = LogicalTaskIdV1::new(execution, digest(32))?;
        let attempt = AttemptIdV1::new(task, 1)?;
        let input = PortableValueRecord::Core(PortableOValue::text(OText {
            utf8: "world".to_owned(),
            encoding: Some("utf-8".to_owned()),
        })?);
        let inputs = InputManifestV1::new(vec![InputBindingV1::new("name", &input)?])?;
        let region = SourceClosedRendererV1::new(
            TrustedInlineRendererV1::Text,
            vec![
                RendererPartV1::literal("hello "),
                RendererPartV1::input("name"),
            ],
            digest(33),
            digest(34),
            digest(35),
            digest(36),
        )?;
        let output = OutputContractV1::for_renderer(
            "result",
            TrustedInlineRendererV1::Text,
            MAX_OVALUE_RECORD_BYTES,
        )?;
        let capsule = ExecutionCapsuleV1::new(
            attempt,
            region,
            digest(37),
            inputs,
            output,
            DEADLINE_UNIX_MS,
            ExecutionLimitsV1::new(30_000, 16 * 1024, MAX_OVALUE_RECORD_BYTES)?,
        )?;
        let capsule_bytes = encode_execution_capsule_v1(&capsule)?;
        let source_closure = FabricSourceClosureV1::new(
            FABRIC_SOURCE_CLOSURE_DIALECT_V1,
            "main = render(name)",
            FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
            "eager",
            digest(38),
            digest(33),
            digest(34),
        )?;
        let peer = semantic(39);
        let target = FabricTargetBindingV1::new(
            peer.clone(),
            "node-a",
            GenerationV1::new(7)?,
            incarnation,
            semantic(40),
            GenerationV1::new(8)?,
            GenerationV1::new(9)?,
            semantic(41),
            semantic(42),
            semantic(43),
            semantic(44),
            semantic(45),
            semantic(46),
            semantic(47),
        )?;
        let authority = FabricSigningKeyV1::from_secret_bytes([0x51; 32]);
        let lease = PlacementLeaseV3::new(
            authority.key_id_digest(),
            semantic(48),
            target,
            &source_closure,
            &capsule,
            UnixMillisV1::new(DEADLINE_UNIX_MS - 30_000),
            UnixMillisV1::new(DEADLINE_UNIX_MS),
        )?;
        let submission = FabricSubmissionV1::new(
            authority.sign_execution_lease(lease)?,
            source_closure,
            capsule_bytes,
        )?;
        let output_value = PortableValueRecord::Core(PortableOValue::text(OText {
            utf8: "hello world".to_owned(),
            encoding: Some("utf-8".to_owned()),
        })?);
        let candidate = ExecutionCandidateV1::new(
            &capsule,
            CandidateOutcomeV1::Succeeded {
                output: CandidateOutputV1::new(
                    "result",
                    &output_value,
                    OutputValueKindV1::Text,
                    OutputFidelityV1::Structural,
                )?,
            },
            DEADLINE_UNIX_MS - 1,
        )?;
        let candidate_bytes = encode_execution_candidate_v1(&candidate)?;
        let node = FabricSigningKeyV1::from_secret_bytes([0x52; 32]);
        let terminal = node.sign_terminal_candidate(&submission, candidate_bytes, 25)?;
        Ok((submission, terminal, peer))
    }

    #[test]
    fn private_namespaced_root_and_exclusive_lock_survive_clones() -> Result<()> {
        let directory = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(directory.path())?;
        assert_eq!(ledger.root(), directory.path().join("fabric-v1"));
        assert_eq!(ledger.execution_cell_incarnation()?.get(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(ledger.root())?.permissions().mode() & 0o777,
                0o700
            );
        }

        let retained = ledger.clone();
        let locked = FabricAttemptLedgerV1::open(directory.path())
            .expect_err("a live clone must retain the exclusive Fabric root lock");
        assert!(format!("{locked:#}").contains("already locked"));
        drop(ledger);
        assert!(FabricAttemptLedgerV1::open(directory.path()).is_err());
        drop(retained);

        let reopened = FabricAttemptLedgerV1::open(directory.path())?;
        assert_eq!(reopened.execution_cell_incarnation()?.get(), 2);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn lock_and_snapshot_symlinks_are_never_followed() -> Result<()> {
        use std::os::unix::fs::symlink;

        let lock_case = private_tempdir()?;
        let lock_root = lock_case.path().join(FABRIC_LEDGER_DIRECTORY_V1);
        fs::create_dir(&lock_root)?;
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&lock_root, fs::Permissions::from_mode(0o700))?;
        }
        let outside_lock = lock_case.path().join("outside-lock");
        fs::write(&outside_lock, b"outside")?;
        symlink(&outside_lock, lock_root.join(FABRIC_LEDGER_LOCK_FILE_V1))?;
        assert!(FabricAttemptLedgerV1::open(lock_case.path()).is_err());
        assert_eq!(fs::read(outside_lock)?, b"outside");

        let snapshot_case = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(snapshot_case.path())?;
        let snapshot = ledger.root().join(FABRIC_LEDGER_SNAPSHOT_FILE_V1);
        drop(ledger);
        fs::remove_file(&snapshot)?;
        let outside_snapshot = snapshot_case.path().join("outside-snapshot");
        fs::write(&outside_snapshot, b"outside")?;
        symlink(&outside_snapshot, &snapshot)?;
        assert!(FabricAttemptLedgerV1::open(snapshot_case.path()).is_err());
        assert_eq!(fs::read(outside_snapshot)?, b"outside");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_lock_is_rejected_before_external_file_mutation() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let directory = private_tempdir()?;
        let root = directory.path().join(FABRIC_LEDGER_DIRECTORY_V1);
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let outside = directory.path().join("outside-hard-link");
        let original = b"external-lock-bytes";
        fs::write(&outside, original)?;
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))?;
        fs::hard_link(&outside, root.join(FABRIC_LEDGER_LOCK_FILE_V1))?;

        let error = FabricAttemptLedgerV1::open(directory.path())
            .expect_err("a hard-linked lock inode must fail closed before mutation");
        assert!(format!("{error:#}").contains("hard-linked"));
        assert_eq!(fs::read(&outside)?, original);
        assert_eq!(fs::metadata(&outside)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn live_ledger_rejects_root_path_substitution_despite_orphaned_lock() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let directory = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(directory.path())?;
        let original_root = ledger.root().to_path_buf();
        let displaced_root = directory.path().join("displaced-fabric-v1");
        fs::rename(&original_root, &displaced_root)?;
        fs::create_dir(&original_root)?;
        fs::set_permissions(&original_root, fs::Permissions::from_mode(0o700))?;

        let error = ledger
            .execution_cell_incarnation()
            .expect_err("a replacement pathname must not inherit the orphaned lock authority");
        assert!(format!("{error:#}").contains("identity changed"));
        assert!(ledger.is_poisoned());
        Ok(())
    }

    #[test]
    fn replay_fences_nonce_and_attempt_independently() -> Result<()> {
        let directory = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(directory.path())?;
        let incarnation = ledger.execution_cell_incarnation()?;
        let first = binding(incarnation, 1, 10, 20);

        validate_for_accept(&ledger, &first)?;
        let accepted = ledger.consume_and_accept(first.clone())?;
        assert!(accepted.may_execute());
        assert_eq!(accepted.entry().state(), FabricLedgerStateV1::Accepted);
        let duplicate = ledger.consume_and_accept(first.clone())?;
        assert!(!duplicate.may_execute());
        assert_eq!(duplicate.entry(), accepted.entry());

        let same_nonce_different_attempt = binding(incarnation, 2, 10, 21);
        let nonce_error = ledger
            .consume_and_accept(same_nonce_different_attempt)
            .expect_err("an issuer-scoped nonce cannot bind a second attempt");
        assert!(format!("{nonce_error:#}").contains("nonce"));

        let same_attempt_new_nonce = binding(incarnation, 1, 11, 22);
        let attempt_error = ledger
            .consume_and_accept(same_attempt_new_nonce)
            .expect_err("an issuer-scoped attempt cannot be rebound under a new nonce");
        assert!(format!("{attempt_error:#}").contains("attempt coordinate"));
        Ok(())
    }

    #[test]
    fn lifecycle_is_durable_and_only_acceptance_grants_execution() -> Result<()> {
        let directory = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(directory.path())?;
        let attempt = binding(ledger.execution_cell_incarnation()?, 1, 12, 23);

        let received = ledger.record_received(attempt.clone())?;
        assert!(received.was_applied());
        assert_eq!(received.entry().state(), FabricLedgerStateV1::Received);
        let validated = ledger.record_validated(&attempt)?;
        assert_eq!(validated.entry().state(), FabricLedgerStateV1::Validated);
        let accepted = ledger.consume_and_accept(attempt.clone())?;
        assert!(accepted.may_execute());
        let running = ledger.mark_running(&attempt)?;
        assert_eq!(running.entry().state(), FabricLedgerStateV1::Running);
        let duplicate = ledger.mark_running(&attempt)?;
        assert!(!duplicate.was_applied());
        Ok(())
    }

    #[test]
    fn restart_advances_incarnation_and_abandons_incomplete_work() -> Result<()> {
        let directory = private_tempdir()?;
        let first = FabricAttemptLedgerV1::open(directory.path())?;
        let first_incarnation = first.execution_cell_incarnation()?;
        let old = binding(first_incarnation, 1, 13, 24);
        validate_for_accept(&first, &old)?;
        assert!(first.consume_and_accept(old.clone())?.may_execute());
        first.mark_running(&old)?;
        drop(first);

        let second = FabricAttemptLedgerV1::open(directory.path())?;
        let second_incarnation = second.execution_cell_incarnation()?;
        assert_eq!(second_incarnation.get(), first_incarnation.get() + 1);
        let duplicate = second.consume_and_accept(old.clone())?;
        assert!(!duplicate.may_execute());
        assert_eq!(duplicate.entry().state(), FabricLedgerStateV1::Abandoned);
        assert_eq!(duplicate.entry().reason_code(), Some("provider-restarted"));

        let stale_new_attempt = binding(first_incarnation, 2, 14, 25);
        assert!(second.record_received(stale_new_attempt).is_err());
        let current_new_attempt = binding(second_incarnation, 2, 14, 25);
        validate_for_accept(&second, &current_new_attempt)?;
        assert!(second
            .consume_and_accept(current_new_attempt)?
            .may_execute());
        Ok(())
    }

    #[test]
    fn terminal_query_replays_exact_bytes_and_original_generation_after_restart() -> Result<()> {
        let directory = private_tempdir()?;
        let first = FabricAttemptLedgerV1::open(directory.path())?;
        let first_incarnation = first.execution_cell_incarnation()?;
        let (submission, terminal, peer) = terminal_fixture(first_incarnation)?;
        let binding = FabricAttemptBindingV1::from_submission(&submission)?;
        let query = FabricAttemptQueryV1::from_submission(&submission);
        let encoded =
            encode_fabric_response_v1(&FabricResponseV1::TerminalCandidate(terminal.clone()))?;
        let expected_header = encoded.header_bytes().to_vec();
        let expected_candidate = encoded
            .payload_bytes()
            .context("terminal fixture omitted its candidate payload")?
            .to_vec();

        validate_for_accept(&first, &binding)?;
        assert!(first.consume_and_accept(binding.clone())?.may_execute());
        first.mark_running(&binding)?;
        first.record_terminal_candidate(&binding, &terminal)?;

        let found = first.query(&query, &peer)?;
        let FabricLedgerQueryOutcomeV1::Found(entry) = found else {
            bail!("exact terminal query did not find its durable attempt");
        };
        let FabricLedgerCurrentResponseV1::TerminalCandidate(stored) = entry.current_response()
        else {
            bail!("durable attempt did not retain its terminal state");
        };
        assert_eq!(stored.header_bytes(), expected_header);
        assert_eq!(stored.candidate_bytes(), expected_candidate);
        assert!(matches!(
            first.query(&query, &semantic(99))?,
            FabricLedgerQueryOutcomeV1::Conflict(FabricLedgerConflictV1 {
                kind: FabricLedgerConflictKindV1::TlsPrincipalMismatch,
                ..
            })
        ));
        drop(first);

        let second = FabricAttemptLedgerV1::open(directory.path())?;
        assert_eq!(
            second.execution_cell_incarnation()?.get(),
            first_incarnation.get() + 1
        );
        let FabricLedgerQueryOutcomeV1::Found(entry) = second.query(&query, &peer)? else {
            bail!("terminal attempt did not survive provider restart");
        };
        assert_eq!(
            entry.binding().execution_cell_incarnation(),
            first_incarnation,
            "terminal attribution must retain the generation that produced it"
        );
        let FabricLedgerCurrentResponseV1::TerminalCandidate(stored) = entry.current_response()
        else {
            bail!("terminal attempt changed state across restart");
        };
        assert_eq!(stored.header_bytes(), expected_header);
        assert_eq!(stored.candidate_bytes(), expected_candidate);
        Ok(())
    }

    #[test]
    fn acceptance_cannot_be_minted_before_durable_validation() -> Result<()> {
        let directory = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(directory.path())?;
        let attempt = binding(ledger.execution_cell_incarnation()?, 1, 17, 28);

        assert!(ledger.consume_and_accept(attempt.clone()).is_err());
        ledger.record_received(attempt.clone())?;
        let received = ledger.consume_and_accept(attempt.clone())?;
        assert!(!received.may_execute());
        assert_eq!(received.entry().state(), FabricLedgerStateV1::Received);
        ledger.record_validated(&attempt)?;
        assert!(ledger.consume_and_accept(attempt)?.may_execute());
        Ok(())
    }

    #[test]
    fn external_corruption_poisons_the_live_ledger_and_restart_fails_closed() -> Result<()> {
        let directory = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(directory.path())?;
        let attempt = binding(ledger.execution_cell_incarnation()?, 1, 15, 26);
        ledger.record_received(attempt.clone())?;
        let snapshot = ledger.root().join(FABRIC_LEDGER_SNAPSHOT_FILE_V1);
        let mut options = OpenOptions::new();
        options.write(true).truncate(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(&snapshot)?;
        file.write_all(b"corrupt")?;
        file.sync_all()?;

        assert!(ledger.record_validated(&attempt).is_err());
        assert!(ledger.is_poisoned());
        assert!(ledger.consume_and_accept(attempt).is_err());
        drop(ledger);
        assert!(FabricAttemptLedgerV1::open(directory.path()).is_err());
        Ok(())
    }

    #[test]
    fn rejection_and_abandonment_are_immutable_terminal_states() -> Result<()> {
        let directory = private_tempdir()?;
        let ledger = FabricAttemptLedgerV1::open(directory.path())?;
        let rejected = binding(ledger.execution_cell_incarnation()?, 1, 16, 27);
        let first = ledger.record_rejected(
            rejected.clone(),
            "authority-rejected",
            "execution lease was not authorized",
        )?;
        assert_eq!(first.entry().state(), FabricLedgerStateV1::Rejected);
        let duplicate = ledger.record_rejected(
            rejected.clone(),
            "authority-rejected",
            "execution lease was not authorized",
        )?;
        assert!(!duplicate.was_applied());
        assert!(ledger
            .record_abandoned(rejected, "provider-restarted", "must not replace rejection")
            .is_err());
        Ok(())
    }
}
