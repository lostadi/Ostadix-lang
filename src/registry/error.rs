use std::path::PathBuf;

use thiserror::Error;

/// Fail-closed validation and persistence errors for registry v1.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds the {maximum}-byte limit")]
    TooLong { field: &'static str, maximum: usize },
    #[error("{field} is not a canonical registry namespace: `{value}`")]
    InvalidNamespace { field: &'static str, value: String },
    #[error("{record} has an invalid validity interval")]
    InvalidValidity { record: &'static str },
    #[error("unsupported {record} schema version {found}; expected {expected}")]
    UnsupportedVersion {
        record: &'static str,
        found: u16,
        expected: u16,
    },
    #[error("registry snapshot contains no events")]
    EmptySnapshot,
    #[error("registry snapshot exceeds the {maximum}-event limit")]
    TooManyEvents { maximum: usize },
    #[error("registry store exceeds the {maximum}-snapshot limit")]
    TooManySnapshots { maximum: usize },
    #[error("registry event sequence must be nonzero")]
    ZeroSequence,
    #[error("registry event sequence mismatch: expected {expected}, found {found}")]
    SequenceMismatch { expected: u64, found: u64 },
    #[error("registry event {sequence} does not chain to the preceding event")]
    PreviousEventMismatch { sequence: u64 },
    #[error("registry event timestamps moved backwards at sequence {sequence}")]
    TimestampRollback { sequence: u64 },
    #[error("registry event {sequence} is dated {issued_at_ms}ms, after verifier time {now_ms}ms")]
    FutureEvent {
        sequence: u64,
        issued_at_ms: u64,
        now_ms: u64,
    },
    #[error("registry event {sequence} has an invalid Ed25519 signature")]
    InvalidSignature { sequence: u64 },
    #[error("registry event {sequence} is outside signer authority for namespace `{namespace}`")]
    UnauthorizedSigner { sequence: u64, namespace: String },
    #[error("the namespace-root record must be the first and only root event")]
    InvalidRootEvent,
    #[error("namespace mismatch: event `{event}` does not match body `{body}`")]
    NamespaceMismatch { event: String, body: String },
    #[error("delegation scope `{child}` must be a strict descendant of `{parent}`")]
    InvalidDelegationScope { parent: String, child: String },
    #[error("delegated root `{namespace}` has no valid chain to a pinned trust root")]
    MissingDelegation { namespace: String },
    #[error("duplicate registry root `{namespace}` with the same public key")]
    DuplicateRoot { namespace: String },
    #[error("registry snapshot rollback for root `{namespace}`: current sequence {current}, incoming sequence {incoming}")]
    SnapshotRollback {
        namespace: String,
        current: u64,
        incoming: u64,
    },
    #[error("registry equivocation/fork detected for root `{namespace}` at sequence {sequence}")]
    Equivocation { namespace: String, sequence: u64 },
    #[error(
        "profile generation rolled back for `{namespace}/{node_id}` from {current} to {incoming}"
    )]
    ProfileRollback {
        namespace: String,
        node_id: String,
        current: u64,
        incoming: u64,
    },
    #[error("profile generation {generation} equivocated for `{namespace}/{node_id}`")]
    ProfileEquivocation {
        namespace: String,
        node_id: String,
        generation: u64,
    },
    #[error("profile for `{namespace}/{node_id}` is not yet valid")]
    ProfileNotYetValid { namespace: String, node_id: String },
    #[error("profile for `{namespace}/{node_id}` expired at {expires_at_ms}ms")]
    StaleProfile {
        namespace: String,
        node_id: String,
        expires_at_ms: u64,
    },
    #[error("profile issuer is not bound to the registry event signer")]
    ProfileIssuerMismatch,
    #[error(
        "profile node identity `{profile}` does not match publication identity `{publication}`"
    )]
    ProfileNodeMismatch {
        profile: String,
        publication: String,
    },
    #[error("registry record is not in canonical encoding")]
    NonCanonicalEncoding,
    #[error("registry input `{path}` is {actual} bytes; maximum is {maximum}")]
    InputTooLarge {
        path: PathBuf,
        actual: u64,
        maximum: usize,
    },
    #[error("refusing to overwrite existing registry file `{0}`")]
    AlreadyExists(PathBuf),
    #[error("registry key file is malformed")]
    MalformedKey,
    #[error("registry key file `{path}` must have Unix mode 0600, found {mode:04o}")]
    InsecureKeyPermissions { path: PathBuf, mode: u32 },
    #[error("registry signing key is not authorized in any snapshot for namespace `{0}`")]
    NoWritableSnapshot(String),
    #[error("canonical registry serialization failed: {0}")]
    Canonical(String),
    #[error("registry JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("registry I/O failed for `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("placement profile is invalid: {0}")]
    Placement(String),
}

impl RegistryError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
