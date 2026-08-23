use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use o_lang::eval::{Evaluator, PlacementFragmentBindingsV2};
use o_lang::hosted_remote::v2::{
    build_local_dev_placement_proof_v2, hosted_v2_client_failure_disposition,
    open_capability_commitment_v2, read_node_public_key_v2, read_placement_signing_key_v2,
    validate_local_dev_session_tier_v2, verify_placement_lease_signature_v2,
    write_new_placement_public_key_v2, write_new_placement_signing_key_v2, HostedCommandBindingV2,
    HostedNodeClientV2, HostedPlacementAuthorityV2, HostedPlacementEvidenceV2,
    HostedPlacementIdentityV2, HostedResponseV2, HostedV2ClientFailureDisposition,
    LocalDevPlacementConfigV2, OpenSessionRequestV2, OperationOutcomeV2, OperationStatusV2,
    PlacementLeaseSignerV2, PlacementPurposeV2, PreparedOperationV2, RecoverSessionRequestV2,
    RecoveryTriggerV2, RecoveryWarrantV2, ReplayClassV2, SessionCapabilityV2,
    SessionMutationRequestV2, SessionQueryV2, SessionStateTierV2, SessionStatusV2,
    SignedPlacementLeaseV2, SubmitOperationRequestV2, DEFAULT_MAX_ACTORS_PER_SESSION_V2,
    DEFAULT_MAX_OPEN_SESSIONS_V2, DEFAULT_MAX_SNAPSHOT_BYTES_PER_ACTOR_V2,
    DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2, DEFAULT_MAX_STATE_BYTES_TOTAL_V2,
    DEVELOPMENT_EVIDENCE_LIFETIME_MILLIS_V2, HOSTED_COMMAND_BINDING_SCHEMA_V2, HOSTED_PROTOCOL_V2,
    HOSTED_RECOVERY_WARRANT_SCHEMA_V2,
};
use o_lang::hosted_remote::{
    certificate_leaf_sha256, default_ca_path, default_client_cert_path, default_client_key_path,
    discover_lan_nodes, fetch_lan_bootstrap, hosted_config_dir, lan_client_sessions_dir,
    lan_peers_config_dir, list_stored_lan_peers, load_stored_lan_peer, store_lan_peer,
    unix_time_ms, ClientTlsIdentity, DiscoveredLanNodeV1, HostedNodeClient,
    HostedOperationOutcomeV1, RemotePreparedOperationV1, StoredLanPeerPathsV1, StoredLanPeerV1,
    DEFAULT_LAN_DISCOVERY_MILLIS, DEFAULT_NODE_ADDRESS, DEFAULT_TLS_SERVER_NAME, LAN_SECURITY_MODE,
    MAX_HOSTED_OUTPUT_BYTES, MAX_HOSTED_SOURCE_BYTES, PAIRED_SECURITY_MODE,
    PAIRING_REQUIRED_SECURITY_MODE,
};
use o_lang::ir::BackendRegistry;
use o_lang::placement::{
    CanonicalPlacementRecordV1, GenerationV1, LeaseExpectationV2, LeaseStateBindingV2,
    PlacementLeaseV2, PlacementReservationV1, SemanticDigestV1, StateCapacityObservationV2,
    StateControlExpectationV2, StateControlLeaseV2, StateQuotaLimitsV2, StateReservationV2,
    StateSessionIdV2, TaskAttemptIdV1, UnixMillisV1,
};
use o_lang::runtime_exec::validate_native_runtime_binary;
use o_lang::shims::ExtractedShims;

#[derive(Debug, Parser)]
#[command(
    name = "octl",
    version,
    about = "Ostadix control CLI (bounded hosted-node preview)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect or directly invoke one mutually authenticated hosted node.
    Node(NodeArgs),
}

#[derive(Debug, Args)]
struct NodeArgs {
    #[command(subcommand)]
    command: NodeCommand,
}

#[derive(Debug, Subcommand)]
enum NodeCommand {
    /// Discover reachable LAN nodes and show remembered peers.
    List(NodeListArgs),
    /// Select the preferred node used when ordinary commands omit --node.
    Use(NodeUseArgs),
    /// Fetch the node's descriptive backend catalog and transport limits.
    Profile(NodeQueryArgs),
    /// Fetch node-local readiness checks (not a placement warrant).
    Doctor(NodeQueryArgs),
    /// Run one exact O source document on the automatically selected or explicitly overridden node.
    Run(NodeRunArgs),
    /// Open and operate automatically managed or expert-level durable V2 sessions.
    Session(SessionArgs),
    /// Provision a co-located self-attested development authority and issue exact signed V2 leases.
    Authority(AuthorityArgs),
}

#[derive(Debug, Args)]
struct AuthorityArgs {
    #[command(subcommand)]
    command: AuthorityCommand,
}

#[derive(Debug, Subcommand)]
// Clap owns this one-shot command value; boxing the larger exact-proof
// variants would complicate the public CLI shape without reducing retention.
#[allow(clippy::large_enum_variant)]
enum AuthorityCommand {
    /// Create a non-overwriting Ed25519 placement-authority key pair.
    Init(AuthorityInitArgs),
    /// Mint and envelope one exact canonical PlacementLeaseV2.
    Issue(AuthorityIssueArgs),
    /// Build proof bundles for one co-located self-attested development authority.
    #[command(subcommand)]
    DevMint(AuthorityDevMintCommand),
}

#[derive(Debug, Subcommand)]
// These one-shot argument records are consumed immediately by clap dispatch;
// boxing one variant would only complicate the public command surface.
#[allow(clippy::large_enum_variant)]
enum AuthorityDevMintCommand {
    /// Mint OpenSession through the co-located self-attested development authority.
    Open(AuthorityDevMintOpenArgs),
    /// Mint Execute through the co-located self-attested development authority.
    Execute(AuthorityDevMintExecuteArgs),
    /// Derive recovery through the co-located self-attested development authority.
    Recover(AuthorityDevMintRecoverArgs),
}

#[derive(Debug, Args)]
struct AuthorityInitArgs {
    /// Destination directory (default: XDG config ostadix/hosted/authority).
    #[arg(long)]
    directory: Option<PathBuf>,
    /// Secret signing-key path (default: DIRECTORY/placement-signing-key.v2).
    #[arg(long)]
    signing_key: Option<PathBuf>,
    /// Public verification-key path (default: DIRECTORY/placement-public-key.v2).
    #[arg(long)]
    public_key: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct AuthorityIssueArgs {
    /// Placement-authority secret created by `octl node authority init`.
    #[arg(long)]
    signing_key: PathBuf,
    /// Exact LeaseExpectationV2 JSON, including command and state bindings.
    #[arg(long)]
    expectation: PathBuf,
    /// Interpret --expectation as StateControlExpectationV2. Required for
    /// OpenSession and Recover; Execute always uses LeaseExpectationV2.
    #[arg(long)]
    state_control: bool,
    /// Exact HostedCommandBindingV2 JSON.
    #[arg(long)]
    command: PathBuf,
    /// Full HostedPlacementEvidenceV2 JSON evaluated again by the node.
    #[arg(long)]
    evidence: PathBuf,
    /// StateCapacityObservationV2 JSON; required only for open-session leases.
    #[arg(long)]
    state_capacity_observation: Option<PathBuf>,
    /// Optional 64-hex nonce. A cryptographically random nonce is generated when omitted.
    #[arg(long)]
    lease_nonce_sha256: Option<String>,
    /// Lease lifetime in seconds. Canonical placement V2 permits at most 30 seconds.
    #[arg(long, default_value_t = 20)]
    lifetime_seconds: u64,
    /// New signed-envelope JSON file; existing files are never overwritten.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct LocalDevRuntimeArgs {
    /// Exact backend directory also configured on the co-located node.
    #[arg(long)]
    shim_dir: PathBuf,
    /// Exact native evaluator image also configured on the co-located node.
    #[arg(long)]
    runtime_binary: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct LocalDevPlacementArgs {
    #[arg(long, default_value_t = 1)]
    profile_generation: u64,
    #[arg(long, default_value_t = 1)]
    capacity_generation: u64,
    #[arg(long, default_value_t = 1)]
    compute_cpu_slots: u32,
    #[arg(long, default_value_t = 1024 * 1024)]
    compute_memory_bytes: u64,
    #[arg(long, default_value_t = 0)]
    compute_scratch_bytes: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SessionStateTierArg {
    Stateless,
    CheckpointRestore,
    ReplayReconstructible,
    LiveActorOnly,
}

impl From<SessionStateTierArg> for SessionStateTierV2 {
    fn from(value: SessionStateTierArg) -> Self {
        match value {
            SessionStateTierArg::Stateless => Self::Stateless,
            SessionStateTierArg::CheckpointRestore => Self::CheckpointRestore,
            SessionStateTierArg::ReplayReconstructible => Self::ReplayReconstructible,
            SessionStateTierArg::LiveActorOnly => Self::LiveActorOnly,
        }
    }
}

#[derive(Debug, Args)]
struct AuthorityDevMintOpenArgs {
    #[command(flatten)]
    runtime: LocalDevRuntimeArgs,
    #[command(flatten)]
    placement: LocalDevPlacementArgs,
    #[arg(long)]
    signing_key: PathBuf,
    /// Intended single-fragment source. Its backend/environment footprint is
    /// fixed for the lifetime of the opened session.
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    node_id: String,
    #[arg(long, default_value_t = 1)]
    node_generation: u64,
    /// Client certificate whose leaf SHA-256 owns the session.
    #[arg(long)]
    client_cert: Option<PathBuf>,
    #[arg(long, value_enum)]
    state_tier: SessionStateTierArg,
    #[arg(long)]
    request_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    state_quota_generation: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_OPEN_SESSIONS_V2)]
    max_open_sessions: u32,
    #[arg(long, default_value_t = DEFAULT_MAX_ACTORS_PER_SESSION_V2)]
    max_actors_per_session: u32,
    #[arg(long, default_value_t = DEFAULT_MAX_SNAPSHOT_BYTES_PER_ACTOR_V2)]
    max_snapshot_bytes_per_actor: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2)]
    max_state_bytes_per_session: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_STATE_BYTES_TOTAL_V2)]
    max_state_bytes_total: u64,
    /// Defaults to the node's snapshot maximum for CheckpointRestore, zero otherwise.
    #[arg(long)]
    reserve_snapshot_bytes: Option<u64>,
    #[arg(long, default_value_t = DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2)]
    reserve_state_bytes: u64,
    /// Requested lease lifetime. Development capacity evidence expires after
    /// four seconds, so prefer --submit over a delayed second command.
    #[arg(long, default_value_t = 20)]
    lifetime_seconds: u64,
    #[command(flatten)]
    submission: LocalDevOpenSubmissionArgs,
    /// New mode-0600 client capability, durably written before the matching lease.
    #[arg(long)]
    capability_out: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, Args)]
struct AuthorityDevMintExecuteArgs {
    #[command(flatten)]
    runtime: LocalDevRuntimeArgs,
    #[command(flatten)]
    session: SessionConnectionArgs,
    #[arg(long)]
    signing_key: PathBuf,
    /// Previously signed OpenSession envelope establishing session identity.
    #[arg(long)]
    open_lease: PathBuf,
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    operation_id: String,
    #[arg(long)]
    task_sha256: String,
    #[arg(long, default_value_t = 1)]
    attempt_generation: u64,
    #[arg(long)]
    request_id: Option<String>,
    /// Uses the node's current next sequence when omitted.
    #[arg(long)]
    sequence: Option<u64>,
    #[arg(long, default_value_t = 300)]
    deadline_seconds: u64,
    #[arg(long, default_value_t = MAX_HOSTED_OUTPUT_BYTES as u64)]
    output_limit_bytes: u64,
    /// Requested lease lifetime. Development capacity evidence expires after
    /// four seconds, so prefer --submit over a delayed second command.
    #[arg(long, default_value_t = 20)]
    lifetime_seconds: u64,
    /// Submit immediately through this co-located self-attested development authority.
    #[arg(long)]
    submit: bool,
    /// New exact PreparedOperationV2 JSON consumed by `session exec --prepared-operation`.
    #[arg(long)]
    operation_out: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct LocalDevOpenSubmissionArgs {
    /// Submit immediately through this co-located self-attested development authority.
    #[arg(long)]
    submit: bool,
    /// Co-located node address used only with --submit.
    #[arg(long, default_value = DEFAULT_NODE_ADDRESS)]
    address: String,
    /// DNS name or IP SAN pinned by the node certificate.
    #[arg(long, default_value = DEFAULT_TLS_SERVER_NAME)]
    server_name: String,
    /// Server CA PEM (default: XDG config ostadix/hosted/ca.pem).
    #[arg(long)]
    ca: Option<PathBuf>,
    /// Client private key PEM paired with --client-cert.
    #[arg(long)]
    key: Option<PathBuf>,
    /// Pinned Ed25519 receipt public key written by `o-node identity init`.
    #[arg(long)]
    node_receipt_public_key: Option<PathBuf>,
    #[arg(long, default_value_t = 10)]
    connect_timeout_seconds: u64,
    #[arg(long, default_value_t = 60)]
    io_timeout_seconds: u64,
}

impl LocalDevOpenSubmissionArgs {
    fn connection(&self, client_cert: Option<PathBuf>) -> Result<V2ConnectionArgs> {
        let node_receipt_public_key = self
            .node_receipt_public_key
            .clone()
            .context("--node-receipt-public-key is required with --submit")?;
        Ok(V2ConnectionArgs {
            connection: NodeConnectionArgs {
                node: None,
                address: Some(self.address.clone()),
                server_name: Some(self.server_name.clone()),
                ca: self.ca.clone(),
                cert: client_cert,
                key: self.key.clone(),
                manual: true,
                connect_timeout_seconds: self.connect_timeout_seconds,
                io_timeout_seconds: self.io_timeout_seconds,
            },
            node_receipt_public_key: Some(node_receipt_public_key),
        })
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReplayClassArg {
    Pure,
    Idempotent,
}

impl From<ReplayClassArg> for ReplayClassV2 {
    fn from(value: ReplayClassArg) -> Self {
        match value {
            ReplayClassArg::Pure => Self::Pure,
            ReplayClassArg::Idempotent => Self::Idempotent,
        }
    }
}

#[derive(Debug, Args)]
struct AuthorityDevMintRecoverArgs {
    #[command(flatten)]
    runtime: LocalDevRuntimeArgs,
    #[command(flatten)]
    session: SessionConnectionArgs,
    #[arg(long)]
    signing_key: PathBuf,
    /// Previously signed OpenSession envelope establishing session identity.
    #[arg(long)]
    open_lease: PathBuf,
    /// Single-fragment source used to reconstruct the session placement footprint.
    #[arg(long)]
    source: PathBuf,
    /// Ambiguous operation to recover. Omit for clean actor-loss recovery.
    #[arg(long)]
    operation_id: Option<String>,
    /// Replay classification asserted for --operation-id recovery.
    #[arg(long, value_enum)]
    replay_class: Option<ReplayClassArg>,
    /// Required for idempotent ambiguous recovery; forbidden otherwise.
    #[arg(long)]
    stable_publication_id: Option<String>,
    #[arg(long)]
    warrant_id: Option<String>,
    #[arg(long)]
    request_id: Option<String>,
    /// Uses the node's current next sequence when omitted.
    #[arg(long)]
    sequence: Option<u64>,
    /// Requested lease lifetime. Development capacity evidence expires after
    /// four seconds, so prefer --submit over a delayed second command.
    #[arg(long, default_value_t = 20)]
    lifetime_seconds: u64,
    /// Submit immediately through this co-located self-attested development authority.
    #[arg(long)]
    submit: bool,
    /// New exact RecoveryWarrantV2 JSON.
    #[arg(long)]
    warrant_out: PathBuf,
    /// New signed Recover state-control envelope JSON.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, Args)]
struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Open a zero-configuration LAN session and make it the current session.
    Start(AutoSessionStartArgs),
    /// Open, execute, wait for the result, and close -- all automatically.
    Run(AutoSessionRunArgs),
    /// Execute another source document in the current zero-configuration session.
    Send(AutoSessionSendArgs),
    /// Show the current zero-configuration session and remote status.
    Info(AutoSessionInfoArgs),
    /// Close the current zero-configuration session.
    Stop(AutoSessionStopArgs),
    /// Print the TLS client-certificate principal digest leases must bind.
    Principal(SessionPrincipalArgs),
    /// Open using the precommitted mode-0600 capability bound by the signed lease.
    Open(SessionOpenArgs),
    /// Submit one operation asynchronously; use status to collect the terminal record.
    Exec(SessionExecArgs),
    /// Read durable session/operation status.
    Status(SessionQueryArgs),
    /// Read the current actor-generation observation.
    Actors(SessionQueryArgs),
    /// Explicitly discard reconstructible actor state when no attempt is ambiguous.
    Reset(SessionMutationArgs),
    /// Submit a warrant-gated ambiguous-operation or clean actor-loss recovery request.
    Recover(SessionRecoverArgs),
    /// Explicitly close the session. Closed journal data is retained for admin GC.
    Close(SessionMutationArgs),
}

#[derive(Debug, Args)]
struct SessionPrincipalArgs {
    /// Client certificate chain PEM.
    #[arg(long)]
    cert: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct NodeListArgs {
    /// Spend this many milliseconds listening for LAN advertisements.
    #[arg(long, default_value_t = DEFAULT_LAN_DISCOVERY_MILLIS)]
    timeout_millis: u64,
}

#[derive(Debug, Args)]
struct NodeUseArgs {
    /// Stable node identity shown by `octl node list`.
    node_id: String,
}

#[derive(Debug, Clone, Args)]
struct NodeConnectionArgs {
    /// Prefer this automatically discovered node. Omit to use the remembered
    /// preference, or choose deterministically when no preference exists.
    #[arg(long)]
    node: Option<String>,
    /// Expert override: connect to this exact socket instead of discovering.
    #[arg(long)]
    address: Option<String>,
    /// Expert override: DNS name or IP SAN pinned by the node certificate.
    #[arg(long)]
    server_name: Option<String>,
    /// Expert override: server CA PEM.
    #[arg(long)]
    ca: Option<PathBuf>,
    /// Expert override: client certificate chain PEM.
    #[arg(long)]
    cert: Option<PathBuf>,
    /// Expert override: client private key PEM.
    #[arg(long)]
    key: Option<PathBuf>,
    /// Disable discovery/enrollment and restore the explicit localhost defaults.
    #[arg(long)]
    manual: bool,
    #[arg(long, default_value_t = 10)]
    connect_timeout_seconds: u64,
    #[arg(long, default_value_t = 60)]
    io_timeout_seconds: u64,
}

impl Default for NodeConnectionArgs {
    fn default() -> Self {
        Self {
            node: None,
            address: None,
            server_name: None,
            ca: None,
            cert: None,
            key: None,
            manual: false,
            connect_timeout_seconds: 10,
            io_timeout_seconds: 60,
        }
    }
}

#[derive(Debug, Args)]
struct AutoSessionStartArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
    /// Source whose backend/environment footprint defines the session.
    source: PathBuf,
    /// State model. Stateless is the lowest-friction and most portable default.
    #[arg(long, value_enum, default_value = "stateless")]
    state_tier: SessionStateTierArg,
}

#[derive(Debug, Args)]
struct AutoSessionRunArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
    /// Source to execute in a temporary automatically managed session.
    source: PathBuf,
    #[arg(long, value_enum, default_value = "stateless")]
    state_tier: SessionStateTierArg,
    /// Leave the generated session open and make it current after execution.
    #[arg(long)]
    keep_open: bool,
    #[arg(long, default_value_t = 300)]
    deadline_seconds: u64,
    #[arg(long, default_value_t = MAX_HOSTED_OUTPUT_BYTES as u64)]
    output_limit_bytes: u64,
}

#[derive(Debug, Args)]
struct AutoSessionSendArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
    /// Source to execute in the current automatically managed session.
    source: PathBuf,
    #[arg(long, default_value_t = 300)]
    deadline_seconds: u64,
    #[arg(long, default_value_t = MAX_HOSTED_OUTPUT_BYTES as u64)]
    output_limit_bytes: u64,
}

#[derive(Debug, Args)]
struct AutoSessionInfoArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
}

#[derive(Debug, Args)]
struct AutoSessionStopArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
}

#[derive(Debug, Args)]
struct NodeQueryArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
}

#[derive(Debug, Args)]
struct NodeRunArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
    /// O source file, or `-` for standard input.
    source: PathBuf,
    /// Stable logical task identity. Generated when omitted.
    #[arg(long)]
    task_id: Option<String>,
    /// Unique identity for this execution attempt. Generated when omitted.
    #[arg(long)]
    attempt_id: Option<String>,
    /// Absolute binding override. If omitted, fetch the node profile first.
    #[arg(long)]
    expected_catalog_sha256: Option<String>,
    /// Absolute operation lifetime from submission. A late value is suppressed.
    #[arg(long, default_value_t = 300)]
    deadline_seconds: u64,
    /// Maximum canonical-CBOR bytes in a successful OValue.
    #[arg(long, default_value_t = MAX_HOSTED_OUTPUT_BYTES as u64)]
    output_limit_bytes: u64,
}

#[derive(Debug, Args)]
struct SessionOpenArgs {
    #[command(flatten)]
    connection: V2ConnectionArgs,
    /// Scheduler-issued signed PlacementLeaseV2 JSON.
    #[arg(long)]
    lease: PathBuf,
    /// Precommitted mode-0600 capability bound by the signed Open lease.
    #[arg(long, alias = "capability-out")]
    capability: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct SessionConnectionArgs {
    #[command(flatten)]
    connection: V2ConnectionArgs,
    /// Mode-0600 JSON file written by `octl node session open`.
    #[arg(long)]
    capability: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct V2ConnectionArgs {
    #[command(flatten)]
    connection: NodeConnectionArgs,
    /// Pinned Ed25519 receipt public key written by `o-node identity init`.
    #[arg(long)]
    node_receipt_public_key: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SessionExecArgs {
    #[command(flatten)]
    session: SessionConnectionArgs,
    /// O source file, or `-` for standard input. Omit with --prepared-operation.
    #[arg(required_unless_present = "prepared_operation")]
    source: Option<PathBuf>,
    /// Exact operation sidecar written by `authority dev-mint execute`.
    #[arg(long, conflicts_with_all = ["source", "task_sha256", "attempt_generation", "expected_catalog_sha256", "deadline_unix_ms"])]
    prepared_operation: Option<PathBuf>,
    /// Scheduler-issued signed PlacementLeaseV2 JSON for this exact operation.
    #[arg(long)]
    lease: PathBuf,
    #[arg(long, required_unless_present = "prepared_operation")]
    operation_id: Option<String>,
    /// Canonical 64-hex logical task digest bound by PlacementLeaseV2.
    #[arg(long, required_unless_present = "prepared_operation")]
    task_sha256: Option<String>,
    /// Positive attempt generation bound by PlacementLeaseV2.
    #[arg(long, required_unless_present = "prepared_operation")]
    attempt_generation: Option<u64>,
    #[arg(long)]
    expected_catalog_sha256: Option<String>,
    #[arg(long, default_value_t = 300)]
    deadline_seconds: u64,
    /// Exact absolute deadline for a pre-signed operation; overrides the relative duration.
    #[arg(long, conflicts_with = "deadline_seconds")]
    deadline_unix_ms: Option<u64>,
    #[arg(long, default_value_t = MAX_HOSTED_OUTPUT_BYTES as u64)]
    output_limit_bytes: u64,
}

#[derive(Debug, Args)]
struct SessionQueryArgs {
    #[command(flatten)]
    session: SessionConnectionArgs,
    #[arg(long)]
    operation_id: Option<String>,
}

#[derive(Debug, Args)]
struct SessionMutationArgs {
    #[command(flatten)]
    session: SessionConnectionArgs,
    /// Exact retry identity. Generated when omitted.
    #[arg(long)]
    request_id: Option<String>,
    /// Exact monotonic sequence. Current next sequence is fetched when omitted.
    #[arg(long)]
    sequence: Option<u64>,
}

#[derive(Debug, Args)]
struct SessionRecoverArgs {
    #[command(flatten)]
    session: SessionConnectionArgs,
    /// RecoveryWarrantV2 JSON.
    #[arg(long)]
    warrant: PathBuf,
    /// Scheduler-issued recover-purpose PlacementLeaseV2 JSON.
    #[arg(long)]
    lease: PathBuf,
}

#[derive(Debug, Clone)]
struct ResolvedNodeConnection {
    node_id: Option<String>,
    address: String,
    server_name: String,
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
    node_receipt_public_key: Option<PathBuf>,
    connect_timeout: Duration,
    io_timeout: Duration,
}

impl ResolvedNodeConnection {
    fn tls_identity(&self) -> ClientTlsIdentity {
        ClientTlsIdentity {
            ca_path: self.ca.clone(),
            cert_path: self.cert.clone(),
            key_path: self.key.clone(),
            server_name: self.server_name.clone(),
        }
    }

    fn explicit_args(&self) -> NodeConnectionArgs {
        NodeConnectionArgs {
            node: None,
            address: Some(self.address.clone()),
            server_name: Some(self.server_name.clone()),
            ca: Some(self.ca.clone()),
            cert: Some(self.cert.clone()),
            key: Some(self.key.clone()),
            manual: true,
            connect_timeout_seconds: self.connect_timeout.as_secs(),
            io_timeout_seconds: self.io_timeout.as_secs(),
        }
    }

    fn explicit_v2_args(&self) -> Result<V2ConnectionArgs> {
        Ok(V2ConnectionArgs {
            connection: self.explicit_args(),
            node_receipt_public_key: Some(
                self.node_receipt_public_key
                    .clone()
                    .context("selected node did not advertise durable V2 receipt identity")?,
            ),
        })
    }
}

fn validate_connection_timeouts(args: &NodeConnectionArgs) -> Result<(Duration, Duration)> {
    if args.connect_timeout_seconds == 0 || args.io_timeout_seconds == 0 {
        bail!("node connection timeouts must be positive");
    }
    if args.connect_timeout_seconds > 3600 || args.io_timeout_seconds > 3600 {
        bail!("node connection timeouts may not exceed 3600 seconds");
    }
    Ok((
        Duration::from_secs(args.connect_timeout_seconds),
        Duration::from_secs(args.io_timeout_seconds),
    ))
}

fn explicit_connection_requested(args: &NodeConnectionArgs) -> bool {
    args.manual
        || args.address.is_some()
        || args.server_name.is_some()
        || args.ca.is_some()
        || args.cert.is_some()
        || args.key.is_some()
}

fn preferred_node_path() -> PathBuf {
    lan_peers_config_dir().join("_preferred")
}

fn read_preferred_node() -> Option<String> {
    fs::read_to_string(preferred_node_path())
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn write_preferred_node(node_id: &str) -> Result<()> {
    let root = lan_peers_config_dir();
    ensure_private_client_directory(&root)?;
    let path = preferred_node_path();
    let temporary = root.join(format!("._preferred.{}.tmp", fresh_id("write")?));
    let written = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).with_context(|| {
            format!(
                "failed to reserve preferred-node update `{}`",
                temporary.display()
            )
        })?;
        file.write_all(format!("{node_id}\n").as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("failed to remember preferred node `{node_id}`"))?;
        sync_parent_directory(&path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

fn load_stored_peer_if_present(
    peers_root: &Path,
    node_id: &str,
) -> Result<Option<(StoredLanPeerV1, StoredLanPeerPathsV1)>> {
    let paths = StoredLanPeerPathsV1::for_root(peers_root, node_id)?;
    match fs::symlink_metadata(&paths.directory) {
        Ok(_) => load_stored_lan_peer(peers_root, node_id).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect remembered peer directory `{}`",
                paths.directory.display()
            )
        }),
    }
}

fn choose_discovered_node(
    mut nodes: Vec<DiscoveredLanNodeV1>,
    requested: Option<&str>,
    preferred: Option<&str>,
) -> Result<Option<DiscoveredLanNodeV1>> {
    nodes.sort_by(|left, right| {
        left.advertisement
            .node_id
            .cmp(&right.advertisement.node_id)
            .then_with(|| left.source_ip.to_string().cmp(&right.source_ip.to_string()))
    });
    if let Some(requested) = requested {
        return Ok(nodes
            .into_iter()
            .find(|node| node.advertisement.node_id == requested));
    }
    if let Some(preferred) = preferred {
        if let Some(index) = nodes
            .iter()
            .position(|node| node.advertisement.node_id == preferred)
        {
            return Ok(Some(nodes.remove(index)));
        }
    }
    if nodes.iter().any(|node| !node.source_ip.is_loopback()) {
        nodes.retain(|node| !node.source_ip.is_loopback());
    }
    Ok(nodes.into_iter().next())
}

fn choose_stored_peer(
    mut peers: Vec<(StoredLanPeerV1, StoredLanPeerPathsV1)>,
    requested: Option<&str>,
    preferred: Option<&str>,
) -> Option<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
    peers.sort_by(|left, right| left.0.node_id.cmp(&right.0.node_id));
    if let Some(requested) = requested {
        return peers
            .into_iter()
            .find(|(peer, _)| peer.node_id == requested);
    }
    if let Some(preferred) = preferred {
        if let Some(index) = peers.iter().position(|(peer, _)| peer.node_id == preferred) {
            return Some(peers.remove(index));
        }
    }
    peers.into_iter().next()
}

fn resolved_from_stored(
    peer: StoredLanPeerV1,
    paths: StoredLanPeerPathsV1,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<ResolvedNodeConnection> {
    let receipt = paths
        .node_receipt_public_key
        .is_file()
        .then_some(paths.node_receipt_public_key.clone());
    Ok(ResolvedNodeConnection {
        node_id: Some(peer.node_id),
        address: peer.address,
        server_name: peer.server_name,
        ca: paths.ca,
        cert: paths.client_cert,
        key: paths.client_key,
        node_receipt_public_key: receipt,
        connect_timeout,
        io_timeout,
    })
}

fn resolve_node_connection(args: &NodeConnectionArgs) -> Result<ResolvedNodeConnection> {
    let (connect_timeout, io_timeout) = validate_connection_timeouts(args)?;
    let only_paired_route_override = args.node.is_some()
        && args.address.is_some()
        && !args.manual
        && args.server_name.is_none()
        && args.ca.is_none()
        && args.cert.is_none()
        && args.key.is_none();
    if only_paired_route_override {
        let node_id = args.node.as_deref().expect("checked above");
        let (mut peer, paths) = load_stored_peer_if_present(&lan_peers_config_dir(), node_id)?
            .with_context(|| {
                format!(
                    "node `{node_id}` has no remembered identity; pair it before overriding its route"
                )
            })?;
        if !peer.is_paired() {
            bail!(
                "--node with --address may override only the route of a passcode-paired identity"
            );
        }
        peer.address = args.address.clone().expect("checked above");
        return resolved_from_stored(peer, paths, connect_timeout, io_timeout);
    }
    if explicit_connection_requested(args) {
        if args.node.is_some() && args.manual {
            bail!("--node selects an automatically discovered peer and cannot be combined with --manual");
        }
        return Ok(ResolvedNodeConnection {
            node_id: args.node.clone(),
            address: args
                .address
                .clone()
                .unwrap_or_else(|| DEFAULT_NODE_ADDRESS.to_owned()),
            server_name: args
                .server_name
                .clone()
                .unwrap_or_else(|| DEFAULT_TLS_SERVER_NAME.to_owned()),
            ca: args.ca.clone().unwrap_or_else(default_ca_path),
            cert: args.cert.clone().unwrap_or_else(default_client_cert_path),
            key: args.key.clone().unwrap_or_else(default_client_key_path),
            node_receipt_public_key: None,
            connect_timeout,
            io_timeout,
        });
    }

    let peers_root = lan_peers_config_dir();
    let preferred = read_preferred_node();
    let timeout = Duration::from_millis(DEFAULT_LAN_DISCOVERY_MILLIS);
    let discovered = match discover_lan_nodes(timeout) {
        Ok(nodes) => nodes,
        Err(error) => {
            eprintln!("octl: LAN discovery was unavailable; trying remembered peers: {error:#}");
            Vec::new()
        }
    };
    if let Some(node) =
        choose_discovered_node(discovered, args.node.as_deref(), preferred.as_deref())?
    {
        let node_id = node.advertisement.node_id.clone();
        let existing = load_stored_peer_if_present(&peers_root, &node_id)?;
        if let Some((mut peer, paths)) = existing
            .as_ref()
            .filter(|(peer, _)| peer.security_mode == PAIRED_SECURITY_MODE)
            .cloned()
        {
            // Discovery is only a routing hint for a paired node. Keep every
            // identity field pinned; the subsequent mTLS handshake decides
            // whether this candidate address actually belongs to that peer.
            peer.address = node.service_address().to_string();
            write_preferred_node(&node_id)?;
            return resolved_from_stored(peer, paths, connect_timeout, io_timeout);
        }

        if node.advertisement.security_mode == PAIRING_REQUIRED_SECURITY_MODE {
            bail!(
                "node `{node_id}` requires reciprocal public-key pairing; run `o node pair` on one node, then `o node pair {node_id}` on this node"
            );
        }
        if node.advertisement.security_mode != LAN_SECURITY_MODE {
            bail!(
                "node `{node_id}` advertised unsupported security mode `{}`",
                node.advertisement.security_mode
            );
        }

        // A server must opt into legacy LAN-open mode before this compatibility
        // branch will download its shared private key. Paired pins never enter
        // this branch and therefore cannot be refreshed from plaintext state.
        let enrolled = match fetch_lan_bootstrap(&node, connect_timeout) {
            Ok(bundle) => Some(store_lan_peer(&peers_root, &node, &bundle)?),
            Err(error) => {
                if existing.is_none() {
                    return Err(error).context(format!(
                        "legacy LAN-open node `{node_id}` could not complete enrollment"
                    ));
                }
                eprintln!(
                    "octl: legacy node `{node_id}` was discovered but enrollment refresh failed; using remembered credentials: {error:#}"
                );
                None
            }
        };
        let (mut peer, paths) = enrolled.or(existing).expect("enrolled or existing peer");
        peer.address = node.service_address().to_string();
        peer.server_name = node.advertisement.server_name.clone();
        write_preferred_node(&node_id)?;
        return resolved_from_stored(peer, paths, connect_timeout, io_timeout);
    }

    let remembered = list_stored_lan_peers(&peers_root)?;
    if let Some((peer, paths)) =
        choose_stored_peer(remembered, args.node.as_deref(), preferred.as_deref())
    {
        write_preferred_node(&peer.node_id)?;
        return resolved_from_stored(peer, paths, connect_timeout, io_timeout);
    }

    if let Some(node_id) = &args.node {
        bail!(
            "no reachable or remembered Ostadix node named `{node_id}`; {}",
            "start it with `o node start` on the other machine, then pair it with `o node pair`"
        );
    }
    bail!(
        "no Ostadix LAN node was discovered or remembered; run `o node start` on both machines and pair them with `o node pair`"
    )
}

fn node_list(args: NodeListArgs) -> Result<()> {
    if args.timeout_millis == 0 || args.timeout_millis > 60_000 {
        bail!("--timeout-millis must be between 1 and 60000");
    }
    let preferred = read_preferred_node();
    let discovered = match discover_lan_nodes(Duration::from_millis(args.timeout_millis)) {
        Ok(nodes) => nodes,
        Err(error) => {
            eprintln!("octl: LAN discovery was unavailable; showing remembered peers: {error:#}");
            Vec::new()
        }
    };
    let remembered = list_stored_lan_peers(&lan_peers_config_dir())?;
    let mut rows = Vec::new();
    for node in discovered {
        let node_id = node.advertisement.node_id.clone();
        let address = node.service_address().to_string();
        let remembered = load_stored_peer_if_present(&lan_peers_config_dir(), &node_id)?.is_some();
        let selected = preferred.as_deref() == Some(node_id.as_str());
        rows.push(serde_json::json!({
            "node_id": node_id,
            "address": address,
            "server_name": node.advertisement.server_name,
            "reachable": true,
            "remembered": remembered,
            "selected": selected,
            "security_mode": node.advertisement.security_mode,
            "supports_v2": node.advertisement.supports_v2,
        }));
    }
    for (peer, _) in remembered {
        let already_listed = rows
            .iter()
            .any(|row| row["node_id"].as_str() == Some(peer.node_id.as_str()));
        if already_listed {
            continue;
        }
        let selected = preferred.as_deref() == Some(peer.node_id.as_str());
        rows.push(serde_json::json!({
            "node_id": peer.node_id,
            "address": peer.address,
            "server_name": peer.server_name,
            "reachable": false,
            "remembered": true,
            "selected": selected,
            "security_mode": peer.security_mode,
            "supports_v2": peer.supports_v2,
        }));
    }
    rows.sort_by(|left, right| left["node_id"].as_str().cmp(&right["node_id"].as_str()));
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}

fn node_use(args: NodeUseArgs) -> Result<()> {
    let connection = NodeConnectionArgs {
        node: Some(args.node_id.clone()),
        ..NodeConnectionArgs::default()
    };
    let resolved = resolve_node_connection(&connection)?;
    write_preferred_node(
        resolved
            .node_id
            .as_deref()
            .context("automatically selected node did not report an identity")?,
    )?;
    println!(
        "using {} at {}",
        resolved.node_id.as_deref().unwrap_or("selected-node"),
        resolved.address
    );
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Node(args) => match args.command {
            NodeCommand::List(args) => node_list(args),
            NodeCommand::Use(args) => node_use(args),
            NodeCommand::Profile(args) => {
                let profile = client(args.connection)?.profile()?;
                println!("{}", serde_json::to_string_pretty(&profile)?);
                Ok(())
            }
            NodeCommand::Doctor(args) => {
                let doctor = client(args.connection)?.doctor()?;
                println!("{}", serde_json::to_string_pretty(&doctor)?);
                if !doctor.ready {
                    bail!("remote node doctor reported failed checks");
                }
                Ok(())
            }
            NodeCommand::Run(args) => run(args),
            NodeCommand::Session(args) => session(args),
            NodeCommand::Authority(args) => authority(args),
        },
    }
}

fn authority(args: AuthorityArgs) -> Result<()> {
    match args.command {
        AuthorityCommand::Init(args) => authority_init(args),
        AuthorityCommand::Issue(args) => authority_issue(args),
        AuthorityCommand::DevMint(AuthorityDevMintCommand::Open(args)) => {
            authority_dev_mint_open(args)
        }
        AuthorityCommand::DevMint(AuthorityDevMintCommand::Execute(args)) => {
            authority_dev_mint_execute(args)
        }
        AuthorityCommand::DevMint(AuthorityDevMintCommand::Recover(args)) => {
            authority_dev_mint_recover(args)
        }
    }
}

fn authority_init(args: AuthorityInitArgs) -> Result<()> {
    let directory = args
        .directory
        .unwrap_or_else(|| hosted_config_dir().join("authority"));
    let signing_key = args
        .signing_key
        .unwrap_or_else(|| directory.join("placement-signing-key.v2"));
    let public_key = args
        .public_key
        .unwrap_or_else(|| directory.join("placement-public-key.v2"));
    if signing_key == public_key {
        bail!("placement signing-key and public-key paths must differ");
    }
    let signer = PlacementLeaseSignerV2::generate()?;
    write_new_placement_signing_key_v2(&signing_key, &signer)?;
    if let Err(error) = write_new_placement_public_key_v2(&public_key, &signer.public_key()) {
        let cleanup = fs::remove_file(&signing_key);
        return match cleanup {
            Ok(()) => Err(error).with_context(|| {
                format!(
                    "public-key creation failed; removed newly created signing key `{}`",
                    signing_key.display()
                )
            }),
            Err(cleanup_error) => Err(error).with_context(|| {
                format!(
                    "public-key creation failed and newly created signing key `{}` could not be removed: {cleanup_error}",
                    signing_key.display()
                )
            }),
        };
    }
    println!("issuer_key={}", signer.issuer_key());
    println!("signing_key={}", signing_key.display());
    println!("public_key={}", public_key.display());
    Ok(())
}

fn authority_issue(args: AuthorityIssueArgs) -> Result<()> {
    if args.lifetime_seconds == 0 || args.lifetime_seconds > 30 {
        bail!("--lifetime-seconds must be between 1 and 30");
    }
    let signer = read_placement_signing_key_v2(&args.signing_key)?;
    let command: HostedCommandBindingV2 = read_json_file(&args.command, "hosted command binding")?;
    command.validate()?;
    let evidence: HostedPlacementEvidenceV2 =
        read_json_file(&args.evidence, "hosted placement evidence")?;
    evidence.validate_shape()?;
    let observation: Option<StateCapacityObservationV2> = args
        .state_capacity_observation
        .as_deref()
        .map(|path| read_json_file(path, "state-capacity observation"))
        .transpose()?;
    let expected_state = match command.purpose {
        PlacementPurposeV2::OpenSession => {
            let observation = observation
                .as_ref()
                .context("open-session lease requires --state-capacity-observation")?;
            if observation.issuer_key() != &signer.issuer_key() {
                bail!("state-capacity observation issuer does not match placement authority");
            }
            LeaseStateBindingV2::open(
                observation.semantic_digest()?,
                command.state_reservation.clone(),
            )
        }
        PlacementPurposeV2::Execute | PlacementPurposeV2::Recover => {
            if observation.is_some() {
                bail!("only open-session leases may carry a state-capacity observation");
            }
            LeaseStateBindingV2::existing(
                command.state_session.clone(),
                command
                    .actor_generation
                    .as_ref()
                    .map(CanonicalPlacementRecordV1::semantic_digest)
                    .transpose()?,
            )
        }
    };
    let lease_nonce = match args.lease_nonce_sha256 {
        Some(nonce) => SemanticDigestV1::from_sha256(nonce)?,
        None => {
            let mut random = [0_u8; 32];
            getrandom::fill(&mut random)
                .context("failed to obtain entropy for placement lease nonce")?;
            SemanticDigestV1::hash_bytes("ostadix/hosted/lease-nonce/v2", &random)
        }
    };
    let issued_at = unix_time_ms()?;
    let expires_at = issued_at
        .checked_add(
            args.lifetime_seconds
                .checked_mul(1000)
                .context("placement lease lifetime overflow")?,
        )
        .context("placement lease expiry overflow")?;
    let command_digest = command.semantic_digest()?;
    let authority = if args.state_control {
        if command.purpose == PlacementPurposeV2::Execute {
            bail!("Execute requires an execution LeaseExpectationV2, not --state-control");
        }
        let expectation: StateControlExpectationV2 =
            read_json_file(&args.expectation, "state-control lease expectation")?;
        if expectation.node_id() != command.node_id {
            bail!("state-control expectation and hosted command name different nodes");
        }
        if expectation.hosted_command_binding() != &command_digest {
            bail!("state-control expectation does not bind the exact hosted command digest");
        }
        if expectation.state_binding() != &expected_state {
            bail!("state-control expectation does not bind the exact hosted state authority");
        }
        HostedPlacementAuthorityV2::StateControl(StateControlLeaseV2::new(
            signer.issuer_key(),
            lease_nonce,
            expectation,
            UnixMillisV1::new(issued_at),
            UnixMillisV1::new(expires_at),
        )?)
    } else {
        if command.purpose != PlacementPurposeV2::Execute {
            bail!("OpenSession and Recover require --state-control");
        }
        let expectation: LeaseExpectationV2 =
            read_json_file(&args.expectation, "placement lease expectation")?;
        if expectation.node_id() != command.node_id {
            bail!("lease expectation and hosted command name different nodes");
        }
        if expectation.hosted_command_binding() != &command_digest {
            bail!("lease expectation does not bind the exact hosted command digest");
        }
        if expectation.state_binding() != &expected_state {
            bail!("lease expectation does not bind the exact hosted state authority");
        }
        HostedPlacementAuthorityV2::Execution(PlacementLeaseV2::new(
            signer.issuer_key(),
            lease_nonce,
            expectation,
            UnixMillisV1::new(issued_at),
            UnixMillisV1::new(expires_at),
        )?)
    };
    let envelope = signer.sign(authority, command, evidence, observation)?;
    write_json_new(&args.out, &envelope, "signed placement lease")?;
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    eprintln!(
        "octl: signed placement lease written to {}",
        args.out.display()
    );
    Ok(())
}

fn authority_dev_mint_open(args: AuthorityDevMintOpenArgs) -> Result<()> {
    validate_dev_lifetime(args.lifetime_seconds)?;
    let submit_connection = if args.submission.submit {
        let connection = args
            .submission
            .connection(args.client_cert.clone())
            .context("--submit requires a complete co-located node connection")?;
        // Validate the receipt pin and timeout bounds before reserving output
        // paths. TLS delivery itself happens only after both artifacts fsync.
        client_v2(connection.clone()).context("invalid --submit node connection")?;
        Some(connection)
    } else {
        None
    };
    let signer = read_placement_signing_key_v2(&args.signing_key)?;
    let tier: SessionStateTierV2 = args.state_tier.into();
    let node_generation = GenerationV1::new(args.node_generation)?;
    let state_quota_generation = GenerationV1::new(args.state_quota_generation)?;
    let quotas = StateQuotaLimitsV2::new(
        args.max_open_sessions,
        args.max_actors_per_session,
        args.max_snapshot_bytes_per_actor,
        args.max_state_bytes_per_session,
        args.max_state_bytes_total,
    )?;
    let snapshot_reservation =
        args.reserve_snapshot_bytes
            .unwrap_or(if tier == SessionStateTierV2::CheckpointRestore {
                args.max_snapshot_bytes_per_actor
            } else {
                0
            });
    let state_reservation =
        StateReservationV2::new(1, snapshot_reservation, args.reserve_state_bytes)?;
    state_reservation.validate_against(&quotas)?;
    let source = read_source(&args.source)?;
    let task_attempt = TaskAttemptIdV1::new(
        random_semantic_digest("ostadix/hosted/dev-open-task/v2")?,
        GenerationV1::new(1)?,
    );
    let operation = PreparedOperationV2::new(
        "dev-open-proof",
        task_attempt,
        source,
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms()?.checked_add(60_000).context(
            "co-located self-attested development authority OpenSession proof deadline overflow",
        )?,
        1,
    )?;
    let bindings = prepare_local_dev_bindings(&args.runtime, &operation)?;
    validate_local_dev_session_tier_v2(&bindings, tier)?;
    let now = unix_time_ms()?;
    let proof = build_local_dev_placement_proof_v2(
        &bindings,
        signer.issuer_key(),
        LocalDevPlacementConfigV2 {
            node_id: args.node_id.clone(),
            node_generation,
            profile_generation: GenerationV1::new(args.placement.profile_generation)?,
            capacity_generation: GenerationV1::new(args.placement.capacity_generation)?,
            reservation: development_compute_reservation(&args.placement)?,
            now_unix_ms: now,
        },
        None,
        None,
        true,
    )?;
    let principal_sha256 = certificate_leaf_sha256(
        args.client_cert
            .clone()
            .unwrap_or_else(default_client_cert_path),
    )?;
    let state_session = StateSessionIdV2::new(
        &args.node_id,
        node_generation,
        random_semantic_digest("ostadix/hosted/dev-state-session/v2")?,
    )?;
    let capability = fresh_session_capability(state_session.semantic_digest()?.to_string())?;
    let capability_commitment = open_capability_commitment_v2(&capability)?;
    let request_id = args.request_id.unwrap_or(fresh_id("dev-open")?);
    let command = HostedCommandBindingV2 {
        schema: HOSTED_COMMAND_BINDING_SCHEMA_V2.to_owned(),
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        node_id: args.node_id.clone(),
        principal_sha256,
        state_session,
        session_state_tier: tier,
        client_request_id: request_id,
        client_sequence: 0,
        purpose: PlacementPurposeV2::OpenSession,
        operation_sha256: None,
        recovery_warrant_sha256: None,
        open_capability_commitment: Some(capability_commitment),
        state_quota_generation,
        state_quota_limits: quotas.clone(),
        state_reservation: state_reservation.clone(),
        actor_generation: None,
    };
    command.validate()?;
    let state_observation = StateCapacityObservationV2::new(
        signer.issuer_key(),
        &args.node_id,
        node_generation,
        state_quota_generation,
        quotas,
        0,
        0,
        UnixMillisV1::new(now.saturating_sub(1)),
        UnixMillisV1::new(
            now.checked_add(DEVELOPMENT_EVIDENCE_LIFETIME_MILLIS_V2)
                .context("state observation expiry overflow")?,
        ),
    )?;
    let evidence = proof.evidence;
    let expectation = StateControlExpectationV2::new(
        &args.node_id,
        evidence.node_profile.descriptor_digest()?,
        evidence.node_profile.profile_generation(),
        evidence.capacity_observation.capacity_generation(),
        evidence.capacity_observation.semantic_digest()?,
        proof.eligibility.semantic_digest()?,
        evidence.requirement_footprint.semantic_digest()?,
        evidence.warrant_discharge.semantic_digest()?,
        bindings.backend_implementation_sha256().clone(),
        bindings.realization_pipeline().clone(),
        evidence.trust_policy.semantic_digest()?,
        evidence.reservation.clone(),
        command.semantic_digest()?,
        LeaseStateBindingV2::open(state_observation.semantic_digest()?, state_reservation),
    )?;
    let authority = HostedPlacementAuthorityV2::StateControl(StateControlLeaseV2::new(
        signer.issuer_key(),
        random_semantic_digest("ostadix/hosted/dev-open-lease/v2")?,
        expectation,
        UnixMillisV1::new(now.saturating_sub(1)),
        UnixMillisV1::new(dev_expiry(now, args.lifetime_seconds)?),
    )?);
    let envelope = signer.sign(authority, command, evidence, Some(state_observation))?;
    write_json_pair_new(
        &args.capability_out,
        &capability,
        "co-located self-attested development authority session capability",
        &args.out,
        &envelope,
        "co-located self-attested development authority OpenSession lease",
    )?;
    eprintln!(
        "octl: co-located self-attested development authority OpenSession envelope written to {}; precommitted capability written to {} (not discovery or scheduler service)",
        args.out.display(),
        args.capability_out.display()
    );
    if let Some(connection) = submit_connection {
        session_open(SessionOpenArgs {
            connection,
            lease: args.out,
            capability: args.capability_out,
        })
        .context(
            "co-located self-attested development authority OpenSession artifacts were retained after immediate submission failed",
        )
    } else {
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        Ok(())
    }
}

fn authority_dev_mint_execute(args: AuthorityDevMintExecuteArgs) -> Result<()> {
    validate_dev_lifetime(args.lifetime_seconds)?;
    if args.deadline_seconds == 0 || args.deadline_seconds > 86_400 {
        bail!("--deadline-seconds must be between 1 and 86400");
    }
    let signer = read_placement_signing_key_v2(&args.signing_key)?;
    let open: SignedPlacementLeaseV2 = read_json_file(&args.open_lease, "open-session lease")?;
    let verified_key = verify_placement_lease_signature_v2(&open)?;
    if verified_key != signer.public_key() {
        bail!(
            "open-session envelope was not signed by this co-located self-attested development authority"
        );
    }
    if open.command.purpose != PlacementPurposeV2::OpenSession
        || !matches!(open.authority, HostedPlacementAuthorityV2::StateControl(_))
    {
        bail!("--open-lease is not an OpenSession state-control envelope");
    }
    validate_development_evidence_issuer(&open.evidence, &signer.issuer_key())?;
    let capability = read_capability(&args.session.capability)?;
    if open.command.state_session.semantic_digest()?.as_sha256() != capability.session_id {
        bail!("session capability does not belong to --open-lease");
    }
    let certificate_principal = certificate_leaf_sha256(
        args.session
            .connection
            .connection
            .cert
            .clone()
            .unwrap_or_else(default_client_cert_path),
    )?;
    if certificate_principal != open.command.principal_sha256 {
        bail!("configured client certificate differs from the opened session principal");
    }
    let client = client_v2(args.session.connection.clone())?;
    let response = client.status(SessionQueryV2 {
        credentials: capability.clone().into(),
        operation_id: None,
    })?;
    let HostedResponseV2::Status { session, .. } = response else {
        bail!(
            "node returned the wrong response while deriving co-located self-attested development authority Execute proof"
        )
    };
    if session.session_id != capability.session_id
        || session.state_tier != open.command.session_state_tier
    {
        bail!("live session identity or state tier differs from --open-lease");
    }
    let sequence = args.sequence.unwrap_or(session.next_client_sequence);
    if sequence != session.next_client_sequence {
        bail!(
            "co-located self-attested development authority mint requires the node's exact next sequence {}; got {sequence}",
            session.next_client_sequence
        );
    }
    let operation_now = unix_time_ms()?;
    let operation = PreparedOperationV2::new(
        args.operation_id,
        TaskAttemptIdV1::new(
            SemanticDigestV1::from_sha256(args.task_sha256)?,
            GenerationV1::new(args.attempt_generation)?,
        ),
        read_source(&args.source)?,
        BackendRegistry::global().catalog_sha256(),
        operation_now
            .checked_add(args.deadline_seconds.checked_mul(1000).context(
                "co-located self-attested development authority Execute deadline overflow",
            )?)
            .context("co-located self-attested development authority Execute deadline overflow")?,
        args.output_limit_bytes,
    )?;
    let bindings = prepare_local_dev_bindings(&args.runtime, &operation)?;
    validate_local_dev_session_tier_v2(&bindings, session.state_tier)?;
    let now = unix_time_ms()?;
    let target = open.evidence.node_profile.descriptor().clone();
    let (actor_generation, establishing_logical_environment) =
        if session.state_tier == SessionStateTierV2::Stateless {
            if session.actor.actor_generation.is_some() {
                bail!("stateless session unexpectedly reports a physical actor generation");
            }
            (None, false)
        } else {
            match &session.actor.actor_generation {
                Some(current) => (Some(current.clone()), false),
                None => {
                    if session.actor.next_actor_generation.get() == 0 {
                        bail!("stateful session reports an invalid next actor generation");
                    }
                    (None, true)
                }
            }
        };
    let proof = build_local_dev_placement_proof_v2(
        &bindings,
        signer.issuer_key(),
        LocalDevPlacementConfigV2 {
            node_id: open.command.node_id.clone(),
            node_generation: open.command.state_session.node_generation(),
            profile_generation: open.evidence.node_profile.profile_generation(),
            capacity_generation: open.evidence.capacity_observation.capacity_generation(),
            reservation: open.evidence.reservation.clone(),
            now_unix_ms: now,
        },
        Some(&target),
        actor_generation.as_ref(),
        establishing_logical_environment,
    )?;
    if placement_identity(&proof.evidence)? != placement_identity(&open.evidence)? {
        bail!("prepared execution would switch the placement identity fixed by OpenSession");
    }
    let request_id = args.request_id.unwrap_or(fresh_id("dev-execute")?);
    let command = HostedCommandBindingV2 {
        schema: HOSTED_COMMAND_BINDING_SCHEMA_V2.to_owned(),
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        node_id: open.command.node_id.clone(),
        principal_sha256: open.command.principal_sha256.clone(),
        state_session: open.command.state_session.clone(),
        session_state_tier: session.state_tier,
        client_request_id: request_id,
        client_sequence: sequence,
        purpose: PlacementPurposeV2::Execute,
        operation_sha256: Some(operation.sha256()?),
        recovery_warrant_sha256: None,
        open_capability_commitment: None,
        state_quota_generation: open.command.state_quota_generation,
        state_quota_limits: open.command.state_quota_limits.clone(),
        state_reservation: open.command.state_reservation.clone(),
        actor_generation: actor_generation.clone(),
    };
    command.validate()?;
    let actor_digest = actor_generation
        .as_ref()
        .map(CanonicalPlacementRecordV1::semantic_digest)
        .transpose()?;
    let evidence = proof.evidence;
    let expectation = LeaseExpectationV2::new(
        &command.node_id,
        evidence.node_profile.descriptor_digest()?,
        evidence.node_profile.profile_generation(),
        evidence.capacity_observation.capacity_generation(),
        evidence.capacity_observation.semantic_digest()?,
        proof.eligibility.semantic_digest()?,
        bindings.operation_oir().clone(),
        evidence.requirement_footprint.semantic_digest()?,
        evidence.warrant_discharge.semantic_digest()?,
        bindings.placement_admission().clone(),
        bindings.task_attempt().clone(),
        bindings.backend_implementation_sha256().clone(),
        bindings.realization_pipeline().clone(),
        evidence.trust_policy.semantic_digest()?,
        evidence.reservation.clone(),
        command.semantic_digest()?,
        LeaseStateBindingV2::existing(command.state_session.clone(), actor_digest),
    )?;
    let authority = HostedPlacementAuthorityV2::Execution(PlacementLeaseV2::new(
        signer.issuer_key(),
        random_semantic_digest("ostadix/hosted/dev-execute-lease/v2")?,
        expectation,
        UnixMillisV1::new(now.saturating_sub(1)),
        UnixMillisV1::new(dev_expiry(now, args.lifetime_seconds)?),
    )?);
    let envelope = signer.sign(authority, command, evidence, None)?;
    write_json_pair_new(
        &args.operation_out,
        &operation,
        "prepared operation",
        &args.out,
        &envelope,
        "co-located self-attested development authority Execute lease",
    )?;
    eprintln!(
        "octl: co-located self-attested development authority Execute envelope written to {}; exact operation written to {} (not discovery or scheduler service)",
        args.out.display(),
        args.operation_out.display()
    );
    if args.submit {
        let response = client
            .submit_operation(SubmitOperationRequestV2 {
                credentials: capability.into(),
                client_request_id: envelope.command.client_request_id.clone(),
                client_sequence: envelope.command.client_sequence,
                operation,
                placement_lease: envelope,
            })
            .with_context(|| {
                format!(
                    "co-located self-attested development authority immediate Execute submission failed; exact retry artifacts remain at `{}` and `{}`",
                    args.out.display(),
                    args.operation_out.display()
                )
            })?;
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    }
    Ok(())
}

fn authority_dev_mint_recover(args: AuthorityDevMintRecoverArgs) -> Result<()> {
    validate_dev_lifetime(args.lifetime_seconds)?;
    let signer = read_placement_signing_key_v2(&args.signing_key)?;
    let open: SignedPlacementLeaseV2 = read_json_file(&args.open_lease, "open-session lease")?;
    let verified_key = verify_placement_lease_signature_v2(&open)?;
    if verified_key != signer.public_key() {
        bail!(
            "open-session envelope was not signed by this co-located self-attested development authority"
        );
    }
    if open.command.purpose != PlacementPurposeV2::OpenSession
        || !matches!(open.authority, HostedPlacementAuthorityV2::StateControl(_))
    {
        bail!("--open-lease is not an OpenSession state-control envelope");
    }
    validate_development_evidence_issuer(&open.evidence, &signer.issuer_key())?;

    let capability = read_capability(&args.session.capability)?;
    if open.command.state_session.semantic_digest()?.as_sha256() != capability.session_id {
        bail!("session capability does not belong to --open-lease");
    }
    let certificate_principal = certificate_leaf_sha256(
        args.session
            .connection
            .connection
            .cert
            .clone()
            .unwrap_or_else(default_client_cert_path),
    )?;
    if certificate_principal != open.command.principal_sha256 {
        bail!("configured client certificate differs from the opened session principal");
    }

    let client = client_v2(args.session.connection.clone())?;
    let response = client.status(SessionQueryV2 {
        credentials: capability.clone().into(),
        operation_id: None,
    })?;
    let HostedResponseV2::Status {
        session,
        head_receipt,
    } = response
    else {
        bail!(
            "node returned the wrong response while deriving co-located self-attested development authority Recover proof"
        )
    };
    if session.session_id != capability.session_id
        || session.node_id != open.command.node_id
        || session.principal_sha256 != open.command.principal_sha256
        || session.state_tier != open.command.session_state_tier
    {
        bail!("live session identity, principal, or state tier differs from --open-lease");
    }
    if head_receipt.entry.session_id != session.session_id
        || head_receipt.entry_sha256 != session.journal_head_sha256
    {
        bail!("status projection does not correlate to its signed journal-head receipt");
    }
    if session.status != SessionStatusV2::RecoveryRequired {
        bail!("session is not awaiting recovery");
    }
    if session.state_tier != SessionStateTierV2::CheckpointRestore {
        bail!(
            "co-located self-attested development authority recovery is supported only for checkpoint-restore sessions"
        );
    }
    let actor_generation = session
        .actor
        .actor_generation
        .clone()
        .context("recovery-required session has no established actor generation")?;
    let checkpoint_sha256 = session
        .actor
        .checkpoint_sha256
        .as_deref()
        .context("recovery-required checkpoint session has no durable checkpoint digest")?;
    let checkpoint_bytes = session
        .actor
        .checkpoint_bytes
        .context("recovery-required checkpoint session has no durable checkpoint length")?;
    if checkpoint_bytes == 0 {
        bail!("recovery-required checkpoint session reports an empty durable checkpoint");
    }
    SemanticDigestV1::from_sha256(checkpoint_sha256)?;

    let trigger = if let Some(operation_id) = args.operation_id.as_deref() {
        let ambiguous = session
            .operations
            .get(operation_id)
            .filter(|operation| operation.status == OperationStatusV2::Ambiguous)
            .cloned()
            .with_context(|| {
                format!("operation `{operation_id}` is not ambiguous in the current session")
            })?;
        let replay_class = args
            .replay_class
            .context("--replay-class is required with --operation-id")?;
        RecoveryTriggerV2::AmbiguousOperation {
            operation_id: ambiguous.operation_id,
            operation_sha256: ambiguous.operation_sha256,
            replay_class: replay_class.into(),
            stable_publication_id: args.stable_publication_id.clone(),
        }
    } else {
        if session
            .operations
            .values()
            .any(|operation| operation.status == OperationStatusV2::Ambiguous)
        {
            bail!("session has an ambiguous operation; select it explicitly with --operation-id");
        }
        if args.replay_class.is_some() {
            bail!("--replay-class is valid only with --operation-id");
        }
        if args.stable_publication_id.is_some() {
            bail!("--stable-publication-id is valid only with --operation-id");
        }
        let expected_next_generation = actor_generation
            .generation()
            .get()
            .checked_add(1)
            .context("actor generation overflow while minting recovery")?;
        if session.actor.next_actor_generation.get() != expected_next_generation
            || session.actor.actor_id.is_some()
        {
            bail!("live actor-loss coordinates are not fenced at the exact successor generation");
        }
        RecoveryTriggerV2::ActorLost {
            previous_actor_generation: actor_generation.clone(),
            checkpoint_sha256: checkpoint_sha256.to_owned(),
            checkpoint_bytes,
            recovery_required_head_sha256: session.journal_head_sha256.clone(),
        }
    };
    let sequence = args.sequence.unwrap_or(session.next_client_sequence);
    if sequence != session.next_client_sequence {
        bail!(
            "co-located self-attested development authority mint requires the node's exact next sequence {}; got {sequence}",
            session.next_client_sequence
        );
    }

    let warrant = RecoveryWarrantV2 {
        schema: HOSTED_RECOVERY_WARRANT_SCHEMA_V2.to_owned(),
        warrant_id: args.warrant_id.unwrap_or(fresh_id("dev-recovery-warrant")?),
        session_id: session.session_id.clone(),
        trigger,
        evidence_sha256: session.journal_head_sha256.clone(),
    };
    warrant.validate()?;

    let operation_now = unix_time_ms()?;
    let proof_operation = PreparedOperationV2::new(
        "dev-recover-proof",
        TaskAttemptIdV1::new(
            random_semantic_digest("ostadix/hosted/dev-recover-task/v2")?,
            GenerationV1::new(1)?,
        ),
        read_source(&args.source)?,
        BackendRegistry::global().catalog_sha256(),
        operation_now.checked_add(60_000).context(
            "co-located self-attested development authority Recover proof deadline overflow",
        )?,
        1,
    )?;
    let bindings = prepare_local_dev_bindings(&args.runtime, &proof_operation)?;
    validate_local_dev_session_tier_v2(&bindings, session.state_tier)?;
    let now = unix_time_ms()?;
    let target = open.evidence.node_profile.descriptor().clone();
    let proof = build_local_dev_placement_proof_v2(
        &bindings,
        signer.issuer_key(),
        LocalDevPlacementConfigV2 {
            node_id: open.command.node_id.clone(),
            node_generation: open.command.state_session.node_generation(),
            profile_generation: open.evidence.node_profile.profile_generation(),
            capacity_generation: open.evidence.capacity_observation.capacity_generation(),
            reservation: open.evidence.reservation.clone(),
            now_unix_ms: now,
        },
        Some(&target),
        Some(&actor_generation),
        false,
    )?;
    if placement_identity(&proof.evidence)? != placement_identity(&open.evidence)? {
        bail!("prepared recovery would switch the placement identity fixed by OpenSession");
    }

    let command = HostedCommandBindingV2 {
        schema: HOSTED_COMMAND_BINDING_SCHEMA_V2.to_owned(),
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        node_id: open.command.node_id.clone(),
        principal_sha256: open.command.principal_sha256.clone(),
        state_session: open.command.state_session.clone(),
        session_state_tier: session.state_tier,
        client_request_id: args.request_id.unwrap_or(fresh_id("dev-recover")?),
        client_sequence: sequence,
        purpose: PlacementPurposeV2::Recover,
        operation_sha256: None,
        recovery_warrant_sha256: Some(warrant.sha256()?),
        open_capability_commitment: None,
        state_quota_generation: open.command.state_quota_generation,
        state_quota_limits: open.command.state_quota_limits.clone(),
        state_reservation: open.command.state_reservation.clone(),
        actor_generation: Some(actor_generation.clone()),
    };
    command.validate()?;
    let evidence = proof.evidence;
    let expectation = StateControlExpectationV2::new(
        &command.node_id,
        evidence.node_profile.descriptor_digest()?,
        evidence.node_profile.profile_generation(),
        evidence.capacity_observation.capacity_generation(),
        evidence.capacity_observation.semantic_digest()?,
        proof.eligibility.semantic_digest()?,
        evidence.requirement_footprint.semantic_digest()?,
        evidence.warrant_discharge.semantic_digest()?,
        bindings.backend_implementation_sha256().clone(),
        bindings.realization_pipeline().clone(),
        evidence.trust_policy.semantic_digest()?,
        evidence.reservation.clone(),
        command.semantic_digest()?,
        LeaseStateBindingV2::existing(
            command.state_session.clone(),
            Some(actor_generation.semantic_digest()?),
        ),
    )?;
    let authority = HostedPlacementAuthorityV2::StateControl(StateControlLeaseV2::new(
        signer.issuer_key(),
        random_semantic_digest("ostadix/hosted/dev-recover-lease/v2")?,
        expectation,
        UnixMillisV1::new(now.saturating_sub(1)),
        UnixMillisV1::new(dev_expiry(now, args.lifetime_seconds)?),
    )?);
    let envelope = signer.sign(authority, command, evidence, None)?;
    write_json_pair_new(
        &args.warrant_out,
        &warrant,
        "co-located self-attested development authority recovery warrant",
        &args.out,
        &envelope,
        "co-located self-attested development authority Recover lease",
    )?;
    eprintln!(
        "octl: co-located self-attested development authority Recover envelope written to {}; exact recovery warrant written to {} (not discovery or scheduler service)",
        args.out.display(),
        args.warrant_out.display()
    );
    if args.submit {
        let response = client
            .recover_session(RecoverSessionRequestV2 {
                credentials: capability.into(),
                client_request_id: envelope.command.client_request_id.clone(),
                client_sequence: envelope.command.client_sequence,
                warrant,
                placement_lease: envelope,
            })
            .with_context(|| {
                format!(
                    "co-located self-attested development authority immediate Recover submission failed; exact retry artifacts remain at `{}` and `{}`",
                    args.out.display(),
                    args.warrant_out.display()
                )
            })?;
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    }
    Ok(())
}

fn prepare_local_dev_bindings(
    runtime: &LocalDevRuntimeArgs,
    operation: &PreparedOperationV2,
) -> Result<PlacementFragmentBindingsV2> {
    if !runtime.shim_dir.is_dir() {
        bail!(
            "--shim-dir `{}` is not a directory",
            runtime.shim_dir.display()
        );
    }
    let executable =
        validate_native_runtime_binary(&runtime.runtime_binary).with_context(|| {
            format!(
                "--runtime-binary `{}` is not a supported native evaluator image",
                runtime.runtime_binary.display()
            )
        })?;
    let mut evaluator = Evaluator::new(runtime.shim_dir.clone())
        .with_registered_backends(BackendRegistry::global().registered_backend_tags())
        .with_runtime_executable(executable);
    Ok(evaluator
        .prepare_placement_fragment(&operation.source_utf8, operation.task_attempt.clone())?
        .bindings()
        .clone())
}

fn placement_identity(evidence: &HostedPlacementEvidenceV2) -> Result<HostedPlacementIdentityV2> {
    let scope = evidence.warrant_discharge.exact_scope();
    Ok(HostedPlacementIdentityV2 {
        target_descriptor: evidence.node_profile.descriptor_digest()?,
        requirement_footprint: evidence.requirement_footprint.semantic_digest()?,
        backend_implementation: scope
            .backend_implementation()
            .context("co-located self-attested development authority evidence omits exact backend implementation")?
            .clone(),
        realization_pipeline: scope
            .realization_pipeline()
            .context("co-located self-attested development authority evidence omits exact realization pipeline")?
            .clone(),
        trust_policy: evidence.trust_policy.semantic_digest()?,
        reservation: evidence.reservation.clone(),
    })
}

fn validate_development_evidence_issuer(
    evidence: &HostedPlacementEvidenceV2,
    issuer: &SemanticDigestV1,
) -> Result<()> {
    evidence.validate_shape()?;
    if evidence.node_profile.issuer_key() != issuer
        || evidence.capacity_observation.issuer_key() != issuer
        || evidence
            .warrants
            .iter()
            .any(|warrant| warrant.issuer_key() != issuer)
    {
        bail!(
            "open-session proof records do not name this co-located self-attested development authority"
        );
    }
    Ok(())
}

fn development_compute_reservation(args: &LocalDevPlacementArgs) -> Result<PlacementReservationV1> {
    Ok(PlacementReservationV1::new(
        args.compute_cpu_slots,
        args.compute_memory_bytes,
        args.compute_scratch_bytes,
    )?)
}

fn validate_dev_lifetime(seconds: u64) -> Result<()> {
    if seconds == 0 || seconds > 30 {
        bail!("--lifetime-seconds must be between 1 and 30");
    }
    Ok(())
}

fn dev_expiry(now: u64, seconds: u64) -> Result<u64> {
    now.checked_add(
        seconds
            .checked_mul(1000)
            .context("co-located self-attested development authority lease lifetime overflow")?,
    )
    .context("co-located self-attested development authority lease expiry overflow")
}

fn random_semantic_digest(domain: &'static str) -> Result<SemanticDigestV1> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .context("failed to obtain entropy for co-located self-attested development authority")?;
    Ok(SemanticDigestV1::hash_bytes(domain, &bytes))
}

fn fresh_session_capability(session_id: String) -> Result<SessionCapabilityV2> {
    let mut bearer = [0_u8; 32];
    getrandom::fill(&mut bearer).context("failed to obtain entropy for session capability")?;
    let capability = SessionCapabilityV2 {
        session_id,
        bearer: hex::encode(bearer),
    };
    capability.validate()?;
    Ok(capability)
}

const AUTO_SESSION_SCHEMA_V1: &str = "ostadix.auto-session/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AutoSessionRecordV1 {
    schema: String,
    session_id: String,
    node_id: String,
    state_tier: String,
    created_unix_ms: u64,
}

#[derive(Debug, Clone)]
struct AutoSessionHandle {
    directory: PathBuf,
    capability: PathBuf,
    open_lease: PathBuf,
    record: AutoSessionRecordV1,
}

impl SessionStateTierArg {
    fn cli_name(self) -> &'static str {
        match self {
            Self::Stateless => "stateless",
            Self::CheckpointRestore => "checkpoint-restore",
            Self::ReplayReconstructible => "replay-reconstructible",
            Self::LiveActorOnly => "live-actor-only",
        }
    }
}

fn ensure_private_client_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create `{}`", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn auto_session_current_path() -> PathBuf {
    lan_client_sessions_dir().join("_current")
}

fn write_current_auto_session(directory: &Path) -> Result<()> {
    let root = lan_client_sessions_dir();
    ensure_private_client_directory(&root)?;
    let name = directory
        .file_name()
        .and_then(|value| value.to_str())
        .context("automatic session directory has no portable name")?;
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        bail!("automatic session directory name is invalid");
    }
    let path = auto_session_current_path();
    fs::write(&path, format!("{name}\n"))?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn clear_current_auto_session(directory: &Path) -> Result<()> {
    let path = auto_session_current_path();
    let expected = directory
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if fs::read_to_string(&path)
        .ok()
        .is_some_and(|value| value.trim() == expected)
    {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn load_current_auto_session() -> Result<AutoSessionHandle> {
    let current = auto_session_current_path();
    let name = fs::read_to_string(&current).with_context(|| {
        concat!(
            "no current automatic session; use `octl node session start SOURCE` or ",
            "`octl node session run SOURCE --keep-open`"
        )
    })?;
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        bail!("current automatic session pointer is invalid");
    }
    let directory = lan_client_sessions_dir().join(name);
    let record: AutoSessionRecordV1 =
        read_json_file(&directory.join("session.json"), "automatic session record")?;
    if record.schema != AUTO_SESSION_SCHEMA_V1 {
        bail!(
            "unsupported automatic session record schema `{}`",
            record.schema
        );
    }
    let capability_path = directory.join("capability.json");
    let capability = read_capability(&capability_path)?;
    if capability.session_id != record.session_id {
        bail!("automatic session record does not match its capability");
    }
    Ok(AutoSessionHandle {
        directory: directory.clone(),
        capability: capability_path,
        open_lease: directory.join("open-lease.json"),
        record,
    })
}

fn ensure_auto_authority() -> Result<PathBuf> {
    let directory = lan_peers_config_dir().join("_authority");
    ensure_private_client_directory(&directory)?;
    let signing = directory.join("placement-signing-key.v2");
    let public = directory.join("placement-public-key.v2");
    match (signing.is_file(), public.is_file()) {
        (true, true) => {
            read_placement_signing_key_v2(&signing)
                .context("automatic LAN placement authority key is unreadable")?;
        }
        (true, false) => {
            let signer = read_placement_signing_key_v2(&signing)?;
            write_new_placement_public_key_v2(&public, &signer.public_key())?;
        }
        (false, true) => {
            fs::remove_file(&public)?;
            let signer = PlacementLeaseSignerV2::generate()?;
            write_new_placement_signing_key_v2(&signing, &signer)?;
            write_new_placement_public_key_v2(&public, &signer.public_key())?;
        }
        (false, false) => {
            let signer = PlacementLeaseSignerV2::generate()?;
            write_new_placement_signing_key_v2(&signing, &signer)?;
            write_new_placement_public_key_v2(&public, &signer.public_key())?;
        }
    }
    Ok(signing)
}

fn resolve_auto_local_runtime() -> Result<(LocalDevRuntimeArgs, Option<ExtractedShims>)> {
    let (shim_dir, guard) = if let Some(path) = env::var_os("O_BACKENDS_DIR")
        .or_else(|| env::var_os("BACKENDS_DIR"))
        .filter(|path| !path.is_empty())
    {
        (PathBuf::from(path), None)
    } else if let Some(root) = env::var_os("O_LANG_ROOT").filter(|path| !path.is_empty()) {
        let path = PathBuf::from(root).join("backends");
        if path.is_dir() {
            (path, None)
        } else {
            let extracted = o_lang::shims::extract_bundled_shims("octl_auto_session_shims")?;
            (extracted.path().to_path_buf(), Some(extracted))
        }
    } else if Path::new("backends").is_dir() {
        (PathBuf::from("backends"), None)
    } else {
        let extracted = o_lang::shims::extract_bundled_shims("octl_auto_session_shims")
            .context("failed to extract bundled backend shims for automatic session")?;
        (extracted.path().to_path_buf(), Some(extracted))
    };

    let current = env::current_exe().context("failed to locate octl executable")?;
    let mut candidates = Vec::new();
    if let Some(explicit) = env::var_os("O_RUNTIME_BINARY").filter(|path| !path.is_empty()) {
        candidates.push(PathBuf::from(explicit));
    }
    if let Some(directory) = current.parent() {
        candidates.push(directory.join("ostadix-evaluator"));
        candidates.push(directory.join("O"));
    }
    if let Ok(path) = which::which("ostadix-evaluator") {
        candidates.push(path);
    }
    if let Ok(path) = which::which("O") {
        candidates.push(path);
    }
    let mut rejected = Vec::new();
    for candidate in candidates {
        match validate_native_runtime_binary(&candidate) {
            Ok(runtime_binary) => {
                return Ok((
                    LocalDevRuntimeArgs {
                        shim_dir,
                        runtime_binary,
                    },
                    guard,
                ))
            }
            Err(error) => rejected.push(format!("{} ({error})", candidate.display())),
        }
    }
    bail!(
        "automatic session could not find a native Ostadix evaluator beside octl; run setup.sh{}",
        if rejected.is_empty() {
            String::new()
        } else {
            format!("; rejected candidates: {}", rejected.join(", "))
        }
    )
}

fn run_internal_octl(command: &mut ProcessCommand, label: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to start internal {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "internal {label} failed with {}\n{}{}",
        output.status,
        stderr.trim(),
        if stdout.trim().is_empty() {
            String::new()
        } else {
            format!("\n{}", stdout.trim())
        }
    )
}

fn append_manual_connection(
    command: &mut ProcessCommand,
    resolved: &ResolvedNodeConnection,
    receipt: bool,
) -> Result<()> {
    command
        .arg("--manual")
        .arg("--address")
        .arg(&resolved.address)
        .arg("--server-name")
        .arg(&resolved.server_name)
        .arg("--ca")
        .arg(&resolved.ca)
        .arg("--cert")
        .arg(&resolved.cert)
        .arg("--key")
        .arg(&resolved.key)
        .arg("--connect-timeout-seconds")
        .arg(resolved.connect_timeout.as_secs().to_string())
        .arg("--io-timeout-seconds")
        .arg(resolved.io_timeout.as_secs().to_string());
    if receipt {
        command.arg("--node-receipt-public-key").arg(
            resolved
                .node_receipt_public_key
                .as_ref()
                .context("selected node does not expose durable V2 receipt identity")?,
        );
    }
    Ok(())
}

fn append_open_submission_connection(
    command: &mut ProcessCommand,
    resolved: &ResolvedNodeConnection,
) -> Result<()> {
    command
        .arg("--address")
        .arg(&resolved.address)
        .arg("--server-name")
        .arg(&resolved.server_name)
        .arg("--ca")
        .arg(&resolved.ca)
        .arg("--key")
        .arg(&resolved.key)
        .arg("--node-receipt-public-key")
        .arg(
            resolved
                .node_receipt_public_key
                .as_ref()
                .context("selected node does not expose durable V2 receipt identity")?,
        )
        .arg("--connect-timeout-seconds")
        .arg(resolved.connect_timeout.as_secs().to_string())
        .arg("--io-timeout-seconds")
        .arg(resolved.io_timeout.as_secs().to_string());
    Ok(())
}

fn automatic_connection_for_session(
    mut requested: NodeConnectionArgs,
    node_id: Option<&str>,
) -> Result<ResolvedNodeConnection> {
    if !explicit_connection_requested(&requested) && requested.node.is_none() {
        requested.node = node_id.map(str::to_owned);
    }
    let resolved = resolve_node_connection(&requested)?;
    if let (Some(expected), Some(actual)) = (node_id, resolved.node_id.as_deref()) {
        if expected != actual {
            bail!("session belongs to node `{expected}`, but connection selected `{actual}`");
        }
    }
    if resolved.node_receipt_public_key.is_none() {
        bail!("selected node does not support automatic durable sessions");
    }
    Ok(resolved)
}

fn auto_open_session_core(
    connection: NodeConnectionArgs,
    source: &Path,
    state_tier: SessionStateTierArg,
) -> Result<AutoSessionHandle> {
    let resolved = automatic_connection_for_session(connection, None)?;
    let node_id = resolved
        .node_id
        .clone()
        .context("automatic node selection did not produce a node identity")?;
    let (runtime, _shim_guard) = resolve_auto_local_runtime()?;
    let signing_key = ensure_auto_authority()?;
    let root = lan_client_sessions_dir();
    ensure_private_client_directory(&root)?;
    let directory = root.join(fresh_id("session")?);
    ensure_private_client_directory(&directory)?;
    let source_path = directory.join("open-source.O");
    fs::write(&source_path, read_source(source)?)?;
    let capability = directory.join("capability.json");
    let open_lease = directory.join("open-lease.json");

    let mut command = ProcessCommand::new(env::current_exe()?);
    command
        .arg("node")
        .arg("authority")
        .arg("dev-mint")
        .arg("open")
        .arg("--shim-dir")
        .arg(&runtime.shim_dir)
        .arg("--runtime-binary")
        .arg(&runtime.runtime_binary)
        .arg("--signing-key")
        .arg(&signing_key)
        .arg("--source")
        .arg(&source_path)
        .arg("--node-id")
        .arg(&node_id)
        .arg("--state-tier")
        .arg(state_tier.cli_name())
        .arg("--client-cert")
        .arg(&resolved.cert)
        .arg("--submit")
        .arg("--capability-out")
        .arg(&capability)
        .arg("--out")
        .arg(&open_lease);
    append_open_submission_connection(&mut command, &resolved)?;
    if let Err(error) = run_internal_octl(&mut command, "automatic OpenSession") {
        let _ = fs::remove_dir_all(&directory);
        return Err(error);
    }
    let session_capability = read_capability(&capability)?;
    let record = AutoSessionRecordV1 {
        schema: AUTO_SESSION_SCHEMA_V1.to_owned(),
        session_id: session_capability.session_id,
        node_id,
        state_tier: state_tier.cli_name().to_owned(),
        created_unix_ms: unix_time_ms()?,
    };
    write_json_new(
        &directory.join("session.json"),
        &record,
        "automatic session record",
    )?;
    Ok(AutoSessionHandle {
        directory,
        capability,
        open_lease,
        record,
    })
}

fn auto_send_session_core(
    handle: &AutoSessionHandle,
    connection: NodeConnectionArgs,
    source: &Path,
    deadline_seconds: u64,
    output_limit_bytes: u64,
) -> Result<OperationOutcomeV2> {
    if deadline_seconds == 0 || deadline_seconds > 86_400 {
        bail!("--deadline-seconds must be between 1 and 86400");
    }
    if output_limit_bytes == 0 || output_limit_bytes > MAX_HOSTED_OUTPUT_BYTES as u64 {
        bail!(
            "--output-limit-bytes must be between 1 and {}",
            MAX_HOSTED_OUTPUT_BYTES
        );
    }
    let resolved = automatic_connection_for_session(connection, Some(&handle.record.node_id))?;
    let (runtime, _shim_guard) = resolve_auto_local_runtime()?;
    let signing_key = ensure_auto_authority()?;
    let operation_id = fresh_id("operation")?;
    let operation_dir = handle.directory.join(&operation_id);
    ensure_private_client_directory(&operation_dir)?;
    let source_path = operation_dir.join("source.O");
    fs::write(&source_path, read_source(source)?)?;
    let operation_out = operation_dir.join("operation.json");
    let execute_lease = operation_dir.join("execute-lease.json");
    let task_sha256 = random_semantic_digest("ostadix/auto-session/task/v1")?.to_string();

    let mut command = ProcessCommand::new(env::current_exe()?);
    command
        .arg("node")
        .arg("authority")
        .arg("dev-mint")
        .arg("execute")
        .arg("--shim-dir")
        .arg(&runtime.shim_dir)
        .arg("--runtime-binary")
        .arg(&runtime.runtime_binary)
        .arg("--signing-key")
        .arg(&signing_key)
        .arg("--open-lease")
        .arg(&handle.open_lease)
        .arg("--source")
        .arg(&source_path)
        .arg("--operation-id")
        .arg(&operation_id)
        .arg("--task-sha256")
        .arg(task_sha256)
        .arg("--attempt-generation")
        .arg("1")
        .arg("--deadline-seconds")
        .arg(deadline_seconds.to_string())
        .arg("--output-limit-bytes")
        .arg(output_limit_bytes.to_string())
        .arg("--submit")
        .arg("--operation-out")
        .arg(&operation_out)
        .arg("--out")
        .arg(&execute_lease)
        .arg("--capability")
        .arg(&handle.capability);
    append_manual_connection(&mut command, &resolved, true)?;
    run_internal_octl(&mut command, "automatic Execute")?;

    let capability = read_capability(&handle.capability)?;
    let client = client_v2(resolved.explicit_v2_args()?)?;
    let deadline = Instant::now() + Duration::from_secs(deadline_seconds.saturating_add(15));
    loop {
        let response = client.status(SessionQueryV2 {
            credentials: capability.clone().into(),
            operation_id: Some(operation_id.clone()),
        })?;
        let HostedResponseV2::Status { session, .. } = response else {
            bail!("node returned the wrong response while waiting for automatic operation");
        };
        let operation = session
            .operations
            .get(&operation_id)
            .with_context(|| format!("node status omitted accepted operation `{operation_id}`"))?;
        match operation.status {
            OperationStatusV2::Accepted | OperationStatusV2::Running => {
                if Instant::now() >= deadline {
                    bail!("automatic operation did not become terminal before its wait deadline");
                }
                thread::sleep(Duration::from_millis(100));
            }
            OperationStatusV2::Succeeded | OperationStatusV2::Failed => {
                return operation
                    .outcome
                    .clone()
                    .context("terminal operation omitted its outcome");
            }
            OperationStatusV2::NotStarted | OperationStatusV2::Ambiguous => {
                bail!(
                    "automatic operation ended in non-terminally-replayable state {:?}; retained artifacts at {}",
                    operation.status,
                    operation_dir.display()
                );
            }
        }
    }
}

fn close_auto_session_core(
    handle: &AutoSessionHandle,
    connection: NodeConnectionArgs,
) -> Result<HostedResponseV2> {
    let resolved = automatic_connection_for_session(connection, Some(&handle.record.node_id))?;
    let capability = read_capability(&handle.capability)?;
    let client = client_v2(resolved.explicit_v2_args()?)?;
    let sequence = current_next_sequence(&client, &capability)?;
    client.close_session(SessionMutationRequestV2 {
        credentials: capability.into(),
        client_request_id: fresh_id("auto-close")?,
        client_sequence: sequence,
    })
}

fn auto_session_start(args: AutoSessionStartArgs) -> Result<()> {
    let handle = auto_open_session_core(args.connection, &args.source, args.state_tier)?;
    write_current_auto_session(&handle.directory)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "session_id": handle.record.session_id,
            "node_id": handle.record.node_id,
            "state_tier": handle.record.state_tier,
            "status": "open",
            "artifacts": handle.directory,
        }))?
    );
    Ok(())
}

fn auto_session_run(args: AutoSessionRunArgs) -> Result<()> {
    let handle = auto_open_session_core(args.connection.clone(), &args.source, args.state_tier)?;
    // If execution becomes ambiguous, leaving this as current gives the user a
    // direct recovery/status path instead of hiding the surviving session.
    write_current_auto_session(&handle.directory)?;
    let source_snapshot = handle.directory.join("open-source.O");
    let outcome = auto_send_session_core(
        &handle,
        args.connection.clone(),
        &source_snapshot,
        args.deadline_seconds,
        args.output_limit_bytes,
    )?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    let succeeded = matches!(&outcome, OperationOutcomeV2::Succeeded { .. });
    if args.keep_open {
        eprintln!(
            "octl: session {} remains open and is now current",
            handle.record.session_id
        );
    } else {
        match close_auto_session_core(&handle, args.connection) {
            Ok(_) => {
                clear_current_auto_session(&handle.directory)?;
                eprintln!("octl: temporary session closed automatically");
            }
            Err(error) => eprintln!(
                "octl: operation completed, but automatic close failed; session remains current: {error:#}"
            ),
        }
    }
    if !succeeded {
        bail!("automatic session operation failed");
    }
    Ok(())
}

fn auto_session_send(args: AutoSessionSendArgs) -> Result<()> {
    let handle = load_current_auto_session()?;
    let outcome = auto_send_session_core(
        &handle,
        args.connection,
        &args.source,
        args.deadline_seconds,
        args.output_limit_bytes,
    )?;
    let succeeded = matches!(&outcome, OperationOutcomeV2::Succeeded { .. });
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    if !succeeded {
        bail!("automatic session operation failed");
    }
    Ok(())
}

fn auto_session_info(args: AutoSessionInfoArgs) -> Result<()> {
    let handle = load_current_auto_session()?;
    let resolved = automatic_connection_for_session(args.connection, Some(&handle.record.node_id))?;
    let capability = read_capability(&handle.capability)?;
    let response = client_v2(resolved.explicit_v2_args()?)?.status(SessionQueryV2 {
        credentials: capability.into(),
        operation_id: None,
    })?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn auto_session_stop(args: AutoSessionStopArgs) -> Result<()> {
    let handle = load_current_auto_session()?;
    let response = close_auto_session_core(&handle, args.connection)?;
    clear_current_auto_session(&handle.directory)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn session(args: SessionArgs) -> Result<()> {
    match args.command {
        SessionCommand::Start(args) => auto_session_start(args),
        SessionCommand::Run(args) => auto_session_run(args),
        SessionCommand::Send(args) => auto_session_send(args),
        SessionCommand::Info(args) => auto_session_info(args),
        SessionCommand::Stop(args) => auto_session_stop(args),
        SessionCommand::Principal(args) => {
            println!(
                "{}",
                certificate_leaf_sha256(args.cert.unwrap_or_else(default_client_cert_path))?
            );
            Ok(())
        }
        SessionCommand::Open(args) => session_open(args),
        SessionCommand::Exec(args) => session_exec(args),
        SessionCommand::Status(args) => session_status(args, false),
        SessionCommand::Actors(args) => session_status(args, true),
        SessionCommand::Reset(args) => session_mutation(args, false),
        SessionCommand::Recover(args) => session_recover(args),
        SessionCommand::Close(args) => session_mutation(args, true),
    }
}

fn session_open(args: SessionOpenArgs) -> Result<()> {
    let lease: SignedPlacementLeaseV2 = read_json_file(&args.lease, "placement lease")?;
    let state_tier = lease.command.session_state_tier;
    let capability = read_capability(&args.capability)?;
    let request = OpenSessionRequestV2 {
        client_request_id: lease.command.client_request_id.clone(),
        state_tier,
        capability_commitment: open_capability_commitment_v2(&capability)?,
        proposed_capability: capability.clone(),
        placement_lease: lease,
    };
    if let Err(error) = request.validate() {
        return Err(error).context(format!(
            "local OpenSession validation failed before network send; precommitted capability retained at `{}` because it may belong to an earlier ambiguous retry",
            args.capability.display()
        ));
    }
    let client = match client_v2(args.connection) {
        Ok(client) => client,
        Err(error) => {
            return Err(error).context(format!(
                "OpenSession failed before network send; precommitted capability retained at `{}` because it may belong to an earlier ambiguous retry",
                args.capability.display()
            ));
        }
    };
    let response = match client.open_session(request) {
        Ok(response) => response,
        Err(error) => match hosted_v2_client_failure_disposition(&error) {
            Some(HostedV2ClientFailureDisposition::PreSend) => {
                return Err(error).context(format!(
                    "this OpenSession attempt did not send; precommitted capability retained at `{}` because an earlier exact attempt may already have committed",
                    args.capability.display()
                ));
            }
            Some(
                HostedV2ClientFailureDisposition::ServerRejected
                | HostedV2ClientFailureDisposition::Ambiguous,
            )
            | None => {
                return Err(error).context(format!(
                        "OpenSession has no signed non-commit proof; capability retained at `{}`; retry the exact same lease and capability, then query status",
                        args.capability.display()
                    ));
            }
        },
    };
    let HostedResponseV2::SessionOpened {
        capability: returned_capability,
        receipt,
    } = response
    else {
        bail!(
            "node returned an uncorrelated response after OpenSession delivery; capability retained at `{}` for exact retry",
            args.capability.display()
        )
    };
    if returned_capability != capability {
        bail!(
            "node echoed different capability bytes after OpenSession delivery; capability retained at `{}` for exact retry",
            args.capability.display()
        );
    }
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    eprintln!(
        "octl: session opened with precommitted capability {} (bearer omitted from stdout)",
        args.capability.display()
    );
    Ok(())
}

fn session_exec(args: SessionExecArgs) -> Result<()> {
    let capability = read_capability(&args.session.capability)?;
    let v2_client = client_v2(args.session.connection.clone())?;
    let operation = match args.prepared_operation {
        Some(path) => {
            let operation: PreparedOperationV2 = read_json_file(&path, "prepared operation")?;
            operation.validate()?;
            operation
        }
        None => {
            if args.deadline_seconds == 0 || args.deadline_seconds > 24 * 60 * 60 {
                bail!("--deadline-seconds must be between 1 and 86400");
            }
            let source_path = args
                .source
                .as_ref()
                .context("source is required without --prepared-operation")?;
            let source = read_source(source_path)?;
            if source.len() > MAX_HOSTED_SOURCE_BYTES {
                bail!(
                    "source length {} exceeds hosted maximum {}",
                    source.len(),
                    MAX_HOSTED_SOURCE_BYTES
                );
            }
            let expected_catalog = match args.expected_catalog_sha256 {
                Some(digest) => digest,
                None => {
                    client(args.session.connection.connection.clone())?
                        .profile()
                        .context("failed to fetch V1 node profile for exact catalog binding")?
                        .backend_catalog_sha256
                }
            };
            let deadline = match args.deadline_unix_ms {
                Some(deadline) => deadline,
                None => unix_time_ms()?
                    .checked_add(
                        args.deadline_seconds
                            .checked_mul(1000)
                            .context("deadline duration overflow")?,
                    )
                    .context("absolute deadline overflow")?,
            };
            PreparedOperationV2::new(
                args.operation_id
                    .context("--operation-id is required without --prepared-operation")?,
                TaskAttemptIdV1::new(
                    SemanticDigestV1::from_sha256(
                        args.task_sha256
                            .context("--task-sha256 is required without --prepared-operation")?,
                    )?,
                    GenerationV1::new(args.attempt_generation.context(
                        "--attempt-generation is required without --prepared-operation",
                    )?)?,
                ),
                source,
                expected_catalog,
                deadline,
                args.output_limit_bytes,
            )?
        }
    };
    let lease: SignedPlacementLeaseV2 = read_json_file(&args.lease, "placement lease")?;
    if lease.command.operation_sha256.as_deref() != Some(operation.sha256()?.as_str()) {
        bail!("placement lease does not bind the exact prepared operation");
    }
    let response = v2_client.submit_operation(SubmitOperationRequestV2 {
        credentials: capability.into(),
        client_request_id: lease.command.client_request_id.clone(),
        client_sequence: lease.command.client_sequence,
        operation,
        placement_lease: lease,
    })?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn session_status(args: SessionQueryArgs, actors: bool) -> Result<()> {
    let capability = read_capability(&args.session.capability)?;
    let client = client_v2(args.session.connection)?;
    let query = SessionQueryV2 {
        credentials: capability.into(),
        operation_id: args.operation_id,
    };
    let response = if actors {
        client.actors(query)?
    } else {
        client.status(query)?
    };
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn session_mutation(args: SessionMutationArgs, close: bool) -> Result<()> {
    let capability = read_capability(&args.session.capability)?;
    let client = client_v2(args.session.connection)?;
    let sequence = match args.sequence {
        Some(sequence) => sequence,
        None => current_next_sequence(&client, &capability)?,
    };
    let request = SessionMutationRequestV2 {
        credentials: capability.into(),
        client_request_id: args.request_id.unwrap_or(fresh_id(if close {
            "close"
        } else {
            "reset"
        })?),
        client_sequence: sequence,
    };
    let response = if close {
        client.close_session(request)?
    } else {
        client.reset_session(request)?
    };
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn session_recover(args: SessionRecoverArgs) -> Result<()> {
    let capability = read_capability(&args.session.capability)?;
    let warrant: RecoveryWarrantV2 = read_json_file(&args.warrant, "recovery warrant")?;
    let lease: SignedPlacementLeaseV2 = read_json_file(&args.lease, "placement lease")?;
    let client = client_v2(args.session.connection)?;
    let response = client.recover_session(RecoverSessionRequestV2 {
        credentials: capability.into(),
        client_request_id: lease.command.client_request_id.clone(),
        client_sequence: lease.command.client_sequence,
        warrant,
        placement_lease: lease,
    })?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn current_next_sequence(
    client: &HostedNodeClientV2,
    capability: &SessionCapabilityV2,
) -> Result<u64> {
    match client.status(SessionQueryV2 {
        credentials: capability.clone().into(),
        operation_id: None,
    })? {
        HostedResponseV2::Status { session, .. } => Ok(session.next_client_sequence),
        _ => bail!("node returned the wrong response while reading session sequence"),
    }
}

fn client_v2(args: V2ConnectionArgs) -> Result<HostedNodeClientV2> {
    let explicit_receipt_key = args.node_receipt_public_key;
    let resolved = resolve_node_connection(&args.connection)?;
    let receipt_path = explicit_receipt_key
        .or_else(|| resolved.node_receipt_public_key.clone())
        .context(concat!(
            "durable V2 requires a node receipt key; automatic LAN enrollment supplies it, ",
            "while manual mode requires --node-receipt-public-key"
        ))?;
    let receipt_key = read_node_public_key_v2(&receipt_path)?;
    let tls_identity = resolved.tls_identity();
    let mut client = HostedNodeClientV2::new(resolved.address, tls_identity, receipt_key);
    client.connect_timeout = resolved.connect_timeout;
    client.io_timeout = resolved.io_timeout;
    Ok(client)
}

fn read_capability(path: &Path) -> Result<SessionCapabilityV2> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "session capability `{}` must not be accessible by group or other users",
                path.display()
            );
        }
    }
    let capability: SessionCapabilityV2 = read_json_file(path, "session capability")?;
    capability.validate()?;
    Ok(capability)
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path, label: &str) -> Result<T> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} path `{}` must be a regular file", path.display());
    }
    if metadata.len() > 2 * 1024 * 1024 {
        bail!("{label} file exceeds the hosted frame bound");
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to decode {label} JSON `{}`", path.display()))
}

fn reserve_new_private_file(path: &Path, label: &str) -> Result<fs::File> {
    let parent = usable_parent(path);
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {label} parent `{}`", parent.display()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .with_context(|| format!("refusing to overwrite {label} `{}`", path.display()))
}

fn remove_reserved_output(path: &Path, reason: &str) -> Result<()> {
    fs::remove_file(path).with_context(|| {
        format!(
            "{reason}; failed to remove newly reserved output `{}`",
            path.display()
        )
    })?;
    sync_parent_directory(path)
}

fn write_json_new(path: &Path, value: &impl serde::Serialize, label: &str) -> Result<()> {
    let mut file = reserve_new_private_file(path, label)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    sync_parent_directory(path)?;
    Ok(())
}

fn write_json_pair_new(
    first_path: &Path,
    first_value: &impl serde::Serialize,
    first_label: &str,
    second_path: &Path,
    second_value: &impl serde::Serialize,
    second_label: &str,
) -> Result<()> {
    if first_path == second_path {
        bail!("{first_label} and {second_label} output paths must differ");
    }
    let mut first = reserve_new_private_file(first_path, first_label)?;
    let mut second = match reserve_new_private_file(second_path, second_label) {
        Ok(file) => file,
        Err(error) => {
            drop(first);
            remove_reserved_output(first_path, "second output reservation failed")?;
            return Err(error);
        }
    };
    let written = (|| -> Result<()> {
        serde_json::to_writer_pretty(&mut first, first_value)?;
        first.write_all(b"\n")?;
        first.sync_all()?;
        serde_json::to_writer_pretty(&mut second, second_value)?;
        second.write_all(b"\n")?;
        second.sync_all()?;
        sync_parent_directory(first_path)?;
        if usable_parent(first_path) != usable_parent(second_path) {
            sync_parent_directory(second_path)?;
        }
        Ok(())
    })();
    if let Err(error) = written {
        drop(first);
        drop(second);
        let first_cleanup = fs::remove_file(first_path);
        let second_cleanup = fs::remove_file(second_path);
        return Err(error).with_context(|| {
            format!(
                "failed to write paired co-located self-attested development authority outputs; cleanup results: first={first_cleanup:?}, second={second_cleanup:?}"
            )
        });
    }
    Ok(())
}

fn usable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_parent_directory(path: &Path) -> Result<()> {
    fs::File::open(usable_parent(path))?
        .sync_all()
        .with_context(|| format!("failed to fsync output directory for `{}`", path.display()))
}

fn run(args: NodeRunArgs) -> Result<()> {
    if args.deadline_seconds == 0 || args.deadline_seconds > 24 * 60 * 60 {
        bail!("--deadline-seconds must be between 1 and 86400");
    }
    let source = read_source(&args.source)?;
    if source.len() > MAX_HOSTED_SOURCE_BYTES {
        bail!(
            "source length {} exceeds hosted maximum {}",
            source.len(),
            MAX_HOSTED_SOURCE_BYTES
        );
    }
    let client = client(args.connection)?;
    let expected_catalog = match args.expected_catalog_sha256 {
        Some(digest) => digest,
        None => {
            client
                .profile()
                .context("failed to fetch node profile for catalog binding")?
                .backend_catalog_sha256
        }
    };
    let now = unix_time_ms()?;
    let lifetime_ms = args
        .deadline_seconds
        .checked_mul(1000)
        .context("deadline duration overflow")?;
    let deadline = now
        .checked_add(lifetime_ms)
        .context("absolute deadline overflow")?;
    let task_id = args.task_id.unwrap_or(fresh_id("task")?);
    let attempt_id = args.attempt_id.unwrap_or(fresh_id("attempt")?);
    let operation = RemotePreparedOperationV1::new(
        task_id,
        attempt_id,
        source,
        expected_catalog,
        deadline,
        args.output_limit_bytes,
    )?;
    let receipt = client.run(operation)?;
    let succeeded = matches!(&receipt.outcome, HostedOperationOutcomeV1::Succeeded { .. });
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    if !succeeded {
        bail!("remote prepared operation did not succeed");
    }
    Ok(())
}

fn client(args: NodeConnectionArgs) -> Result<HostedNodeClient> {
    let resolved = resolve_node_connection(&args)?;
    let tls_identity = resolved.tls_identity();
    let mut client = HostedNodeClient::new(resolved.address, tls_identity);
    client.connect_timeout = resolved.connect_timeout;
    client.io_timeout = resolved.io_timeout;
    Ok(client)
}

fn read_source(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .context("failed to read O source from standard input")?;
        return Ok(source);
    }
    fs::read_to_string(path)
        .with_context(|| format!("failed to read O source `{}`", path.display()))
}

fn fresh_id(prefix: &str) -> Result<String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).context("failed to obtain entropy for operation identity")?;
    Ok(format!("{prefix}-{}", hex::encode(random)))
}
