use std::env;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
#[cfg(unix)]
use std::io::{Read, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
#[cfg(unix)]
use std::process::{Child, Stdio};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use o_lang::execution_fabric_authority::TrustedFabricAuthoritiesV1;
#[cfg(unix)]
use o_lang::hosted_remote::lan_node_process_dir;
use o_lang::hosted_remote::mesh::{MeshNodeRuntime, MeshNodeRuntimeConfig};
use o_lang::hosted_remote::v2::{
    default_hosted_v2_state_dir, read_node_signing_key_v2, read_placement_public_key_v2,
    serve_owned_node_dual_until_shutdown_with_listener_ready,
    serve_owned_node_dual_with_execution_fabric_v1_until_shutdown_with_listener_ready,
    write_new_node_public_key_v2, write_new_node_signing_key_v2, DurableSessionStoreV2,
    HostedDualNodeShutdown, HostedNodeSignerV2, HostedOwnedDualNodeServerConfig,
    HostedOwnedDualNodeWithFabricServerConfigV1, HostedV2RuntimeConfig, HostedV2RuntimeOwner,
    LanOpenPlacementAuthorizerV2, PinnedEd25519PlacementAuthorizerV2, PlacementProofAuthorizerV2,
    DEFAULT_MAX_ACTORS_PER_SESSION_V2, DEFAULT_MAX_OPEN_SESSIONS_V2,
    DEFAULT_MAX_SNAPSHOT_BYTES_PER_ACTOR_V2, DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2,
    DEFAULT_MAX_STATE_BYTES_TOTAL_V2,
};
use o_lang::hosted_remote::{
    accept_mutual_tls, accept_pairing_once, build_client_config, build_server_config,
    connect_mutual_tls, default_ca_path, default_node_cert_path, default_node_key_path,
    discover_lan_nodes, generate_pairing_passcode, hosted_config_dir, join_pairing_once,
    lan_open_config_dir, lan_open_v2_state_dir, lan_peers_config_dir, load_stored_lan_peer,
    read_fabric_node_signing_key_v1, read_fabric_public_key_v1, replace_paired_lan_peer,
    serve_node_with_execution_fabric_v1_and_listener_ready, serve_node_with_listener_ready,
    spawn_lan_bootstrap_server, spawn_lan_discovery_responder, store_paired_lan_peer,
    ClientTlsIdentity, FabricAttemptProviderConfigV1, FabricAttemptProviderV1, HostedNodeRuntime,
    HostedNodeServerConfig, HostedNodeWithFabricServerConfigV1, LanBootstrapBundleV1,
    LanNodeAdvertisementV1, NodeDoctorCheckV1, PairingLocalIdentityV1, PairingPublicIdentityV1,
    PairingResultV1, ServerTlsIdentity, StoredLanPeerPathsV1, DEFAULT_LAN_BOOTSTRAP_PORT,
    DEFAULT_LAN_DISCOVERY_PORT, DEFAULT_LAN_NODE_PORT, DEFAULT_LAN_PAIRING_PORT,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_NODE_BIND, DEFAULT_NODE_ID, LAN_BOOTSTRAP_SCHEMA_V1,
    LAN_SECURITY_MODE,
};
use o_lang::placement::{GenerationV1, StateQuotaLimitsV2};
use o_lang::runtime_exec::validate_native_runtime_binary;
use o_lang::shims::ExtractedShims;

const V2_NODE_EPOCH_HELP: &str = "Stable node-state/deployment epoch bound into durable V2 session identity. Reuse it across normal process restarts. To bump it, use a new state root or archive the old root first; changing this value never evicts or migrates existing sessions.";
const FABRIC_NODE_EPOCH_HELP: &str = "Stable Fabric target node/deployment generation bound into execution leases. Reuse it across normal process restarts. The durable execution-cell incarnation advances separately when the Fabric provider reopens; change this generation only for an intentional deployment epoch.";
const DEFAULT_DETACHED_STARTUP_TIMEOUT_SECONDS: u64 = 120;
#[cfg(unix)]
const DETACHED_STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(unix)]
const DETACHED_STARTUP_TERMINATION_GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const DETACHED_STARTUP_LOG_EXCERPT_BYTES: usize = 16 * 1024;
#[cfg(unix)]
const DETACHED_STARTUP_LOG_POLL_BYTES: usize = 64 * 1024;
const DETACHED_LISTENER_READY_LOG_PREFIX: &str = "o-node: listener ready token=";
const DETACHED_LAUNCH_TOKEN_BYTES: usize = 16;
const GENERATED_PKI_VERIFY_TIMEOUT: Duration = Duration::from_secs(3);
const GENERATED_PKI_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Parser)]
#[command(
    name = "o-node",
    version,
    about = "Serve bounded Ostadix prepared operations over TLS 1.3 mutual authentication"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
// Clap constructs exactly one command per short-lived CLI process. Keep the
// argument structs inline instead of adding allocation and match indirection.
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Start the zero-configuration LAN node as a detached background process.
    Start(StartArgs),
    /// Stop the detached zero-configuration LAN node.
    Stop,
    /// Report whether the detached zero-configuration LAN node is running.
    Status,
    /// Restart the detached zero-configuration LAN node.
    Restart(StartArgs),
    /// Pair once with another node using a short passcode, then remember its public identity.
    Pair(PairArgs),
    /// Provision a local development CA plus node/client identities.
    Pki(PkiArgs),
    /// Initialize the durable V2 node receipt-signing identity.
    Identity(IdentityArgs),
    /// Explicit offline administration of durable V2 state.
    Admin(AdminArgs),
    /// Print descriptive node/catalog metadata; this is not a health claim.
    Profile(ProfileArgs),
    /// Validate the local shim and TLS configuration without listening.
    Doctor(DoctorArgs),
    /// Serve frozen V1, optional durable Hosted V2/Mesh, and explicitly authorized Fabric V1 over mTLS.
    Serve(ServeArgs),
}

#[derive(Debug, Clone, Args)]
struct StartArgs {
    /// Legacy compatibility: let any LAN-reachable client download a shared private key.
    #[arg(long)]
    lan_open: bool,
    /// Seconds allowed for durable recovery and listener initialization.
    #[arg(
        long,
        default_value_t = DEFAULT_DETACHED_STARTUP_TIMEOUT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..=3600)
    )]
    startup_timeout_seconds: u64,
    /// Key algorithm used only when fresh automatic TLS material is required.
    #[arg(
        long = "fresh-pki-key-algorithm",
        value_enum,
        default_value = "rsa-3072"
    )]
    fresh_pki_key_algorithm: PkiKeyAlgorithm,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
enum PkiKeyAlgorithm {
    /// RSA-3072, retained as the production-compatible default.
    #[value(name = "rsa-3072")]
    Rsa3072,
    /// NIST P-256 ECDSA, with comparable classical security and fast key generation.
    #[value(name = "ec-p256")]
    EcP256,
}

impl PkiKeyAlgorithm {
    fn openssl_new_key_args(self) -> &'static [&'static str] {
        match self {
            Self::Rsa3072 => &["rsa:3072"],
            Self::EcP256 => &["ec", "-pkeyopt", "ec_paramgen_curve:prime256v1"],
        }
    }

    fn tls_key_usage(self) -> &'static str {
        match self {
            Self::Rsa3072 => "digitalSignature,keyEncipherment",
            Self::EcP256 => "digitalSignature",
        }
    }

    fn cli_name(self) -> &'static str {
        match self {
            Self::Rsa3072 => "rsa-3072",
            Self::EcP256 => "ec-p256",
        }
    }
}

#[derive(Debug, Args)]
struct PairArgs {
    /// Node ID printed by `o node pair` on the offering machine. Omit to create an offer.
    peer_node_id: Option<String>,
    /// Read the passcode from one line of standard input instead of a hidden terminal prompt.
    #[arg(long)]
    passcode_stdin: bool,
    /// Direct pairing endpoint for routed networks (for example 203.0.113.8:7340).
    #[arg(long)]
    address: Option<String>,
    /// Deliberately replace an existing paired pin (for renewal or interrupted-pairing recovery).
    #[arg(long)]
    replace: bool,
    /// Listener endpoint when creating an offer.
    #[arg(long, default_value = "0.0.0.0:7340")]
    bind: String,
    /// Local hosted service port recorded for later automatic connections.
    #[arg(long, default_value_t = DEFAULT_LAN_NODE_PORT)]
    service_port: u16,
    /// Seconds before an unanswered pairing offer expires.
    #[arg(long, default_value_t = 300)]
    offer_timeout_seconds: u64,
    /// Seconds allowed for each pairing protocol read/write step.
    #[arg(long, default_value_t = 15)]
    io_timeout_seconds: u64,
    /// Milliseconds to search the LAN for the named offering node.
    #[arg(long, default_value_t = 3_000)]
    discovery_timeout_millis: u64,
}

#[derive(Debug, Args)]
struct PkiArgs {
    #[command(subcommand)]
    command: PkiCommand,
}

#[derive(Debug, Subcommand)]
enum PkiCommand {
    /// Generate a non-overwriting development PKI and verify it by mTLS handshake.
    Init(PkiInitArgs),
}

#[derive(Debug, Args)]
struct IdentityArgs {
    #[command(subcommand)]
    command: IdentityCommand,
}

#[derive(Debug, Subcommand)]
enum IdentityCommand {
    /// Create a non-overwriting Ed25519 key for V2 journal receipts.
    Init(IdentityInitArgs),
}

#[derive(Debug, Args)]
struct AdminArgs {
    #[command(subcommand)]
    command: AdminCommand,
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Permanently remove one durably closed session under the exclusive state lock.
    GcClosed(AdminGcClosedArgs),
}

#[derive(Debug, Args)]
struct AdminGcClosedArgs {
    #[arg(long)]
    session_id: String,
    /// Durable V2 state root (default: XDG state ostadix/hosted-v2).
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// V2 Ed25519 receipt key (default: STATE_DIR/node-signing-key.v2).
    #[arg(long)]
    node_signing_key: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct IdentityInitArgs {
    /// Durable V2 state root (default: XDG state ostadix/hosted-v2).
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Node signing key path (default: STATE_DIR/node-signing-key.v2).
    #[arg(long)]
    node_signing_key: Option<PathBuf>,
    /// Public receipt-verification key (default: STATE_DIR/node-signing-public.v2).
    #[arg(long)]
    node_public_key: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PkiInitArgs {
    /// Destination (default: XDG config ostadix/hosted).
    #[arg(long)]
    directory: Option<PathBuf>,
    /// DNS name or IP SAN for the node certificate.
    #[arg(long, default_value = "localhost")]
    server_name: String,
    /// OpenSSL executable to invoke without a shell.
    #[arg(long, default_value = "openssl")]
    openssl: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct RuntimeArgs {
    /// Stable semantic identity. Automatically generated and persisted unless --manual is used.
    #[arg(long)]
    node_id: Option<String>,
    /// Backend shim directory. Defaults to O_BACKENDS_DIR, then bundled shims.
    #[arg(long)]
    shim_dir: Option<PathBuf>,
    /// Native evaluator image used for admitted backend-proxy launches.
    /// Defaults to a sibling `ostadix-evaluator`, then a sibling O development
    /// binary, then PATH `ostadix-evaluator`; shell dispatchers are rejected.
    #[arg(long)]
    runtime_binary: Option<PathBuf>,
    #[arg(long, default_value_t = DEFAULT_MAX_CONNECTIONS)]
    max_connections: usize,
}

#[derive(Debug, Args)]
struct ProfileArgs {
    #[arg(long)]
    node_id: Option<String>,
    #[arg(long, default_value_t = DEFAULT_MAX_CONNECTIONS)]
    max_connections: usize,
}

#[derive(Debug, Clone, Args)]
struct ServerTlsArgs {
    /// Server certificate chain PEM (default: XDG config ostadix/hosted/node-cert.pem).
    #[arg(long)]
    cert: Option<PathBuf>,
    /// Server private key PEM (default: XDG config ostadix/hosted/node-key.pem).
    #[arg(long)]
    key: Option<PathBuf>,
    /// CA PEM used exclusively to authenticate client certificates.
    #[arg(long)]
    client_ca: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[command(flatten)]
    runtime: RuntimeArgs,
    #[command(flatten)]
    tls: ServerTlsArgs,
    /// Disable automatic LAN identity, PKI, and V2 provisioning.
    #[arg(long)]
    manual: bool,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[command(flatten)]
    runtime: RuntimeArgs,
    #[command(flatten)]
    tls: ServerTlsArgs,
    /// Listener address. Defaults to 0.0.0.0:7337 in automatic mode and 127.0.0.1:7337 in manual mode.
    #[arg(long)]
    bind: Option<String>,
    /// Disable discovery, enrollment, automatic identities, and automatic V2 setup.
    #[arg(long)]
    manual: bool,
    /// Legacy compatibility: expose a shared client private key to LAN-reachable callers.
    #[arg(long, conflicts_with = "manual")]
    lan_open: bool,
    /// Keep automatic configuration but do not advertise on the LAN.
    #[arg(long)]
    no_discovery: bool,
    /// With --lan-open, keep automatic configuration but suppress the legacy bootstrap service.
    #[arg(long)]
    no_bootstrap: bool,
    /// Internal identity binding for a detached child launched by `start`.
    #[arg(long, hide = true, value_name = "TOKEN")]
    managed_start_token: Option<String>,
    /// Explicitly enable execution Fabric V1 using this durable provider state base.
    #[arg(long, value_name = "PATH")]
    fabric_state_dir: Option<PathBuf>,
    /// Fabric V1 node receipt-signing key. Required with --fabric-state-dir.
    #[arg(long, value_name = "PATH")]
    fabric_node_signing_key: Option<PathBuf>,
    /// Trusted Fabric execution-authority public key. Repeat to enroll multiple issuers.
    #[arg(long = "fabric-authority-public-key", value_name = "PATH")]
    fabric_authority_public_keys: Vec<PathBuf>,
    #[arg(long, default_value_t = 1, help = FABRIC_NODE_EPOCH_HELP)]
    fabric_node_generation: u64,
    /// Enable durable session protocol V2 using this capability-first state root.
    #[arg(long)]
    v2_state_dir: Option<PathBuf>,
    /// Durable scheduler/actor mesh state root. Automatic mode defaults to V2_STATE_DIR/mesh-v1;
    /// manual mode enables the mesh only when this option is supplied.
    #[arg(long, value_name = "PATH", conflicts_with = "no_mesh")]
    mesh_state_dir: Option<PathBuf>,
    /// Disable the scheduler/actor mesh that automatic mode enables by default.
    #[arg(long, conflicts_with = "mesh_state_dir")]
    no_mesh: bool,
    /// V2 Ed25519 receipt key (default: V2_STATE_DIR/node-signing-key.v2).
    #[arg(long)]
    v2_node_signing_key: Option<PathBuf>,
    /// Hex Ed25519 public key of the placement authority. Required with V2.
    #[arg(long)]
    v2_authority_public_key: Option<PathBuf>,
    #[arg(long, default_value_t = 1, help = V2_NODE_EPOCH_HELP)]
    v2_node_generation: u64,
    /// Monotonic generation of the five canonical state quota limits.
    #[arg(long, default_value_t = 1)]
    v2_state_quota_generation: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_OPEN_SESSIONS_V2)]
    v2_max_open_sessions: u32,
    #[arg(long, default_value_t = DEFAULT_MAX_ACTORS_PER_SESSION_V2)]
    v2_max_actors_per_session: u32,
    #[arg(long, default_value_t = DEFAULT_MAX_SNAPSHOT_BYTES_PER_ACTOR_V2)]
    v2_max_snapshot_bytes_per_actor: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2)]
    v2_max_state_bytes_per_session: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_STATE_BYTES_TOTAL_V2)]
    v2_max_state_bytes_total: u64,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Start(args) => start_detached_node(args),
        Command::Stop => stop_detached_node(),
        Command::Status => detached_node_status(true).map(|_| ()),
        Command::Restart(args) => restart_detached_node(args),
        Command::Pair(args) => pair_node(args),
        Command::Pki(args) => match args.command {
            PkiCommand::Init(args) => init_development_pki(args, PkiKeyAlgorithm::Rsa3072),
        },
        Command::Identity(args) => match args.command {
            IdentityCommand::Init(args) => init_v2_identity(args),
        },
        Command::Admin(args) => match args.command {
            AdminCommand::GcClosed(args) => gc_closed_session(args),
        },
        Command::Profile(args) => {
            let node_id = match args.node_id {
                Some(node_id) => node_id,
                None => ensure_lan_open_material()?.node_id,
            };
            let profile =
                o_lang::hosted_remote::NodeProfileV1::local(node_id, args.max_connections)?;
            println!("{}", serde_json::to_string_pretty(&profile)?);
            Ok(())
        }
        Command::Doctor(args) => doctor(args),
        Command::Serve(args) => serve(args),
    }
}

#[derive(Debug, Clone)]
struct LanOpenNodeMaterial {
    node_id: String,
    server_name: String,
    pki_dir: PathBuf,
    state_dir: PathBuf,
    node_signing_key: PathBuf,
    node_public_key: PathBuf,
    pairing_ca: PathBuf,
    pairing_ca_key: PathBuf,
    client_ca_bundle: PathBuf,
}

struct LanServicesAfterBind {
    material: Option<LanOpenNodeMaterial>,
    service_port: u16,
    supports_v2: bool,
    legacy_lan_open: bool,
    no_discovery: bool,
    no_bootstrap: bool,
}

impl LanServicesAfterBind {
    fn start(self) -> Result<()> {
        let Some(material) = self.material else {
            return Ok(());
        };
        // Discovery is the publication boundary. Start any legacy enrollment
        // endpoint first so no advertisement can name an incomplete service.
        if self.legacy_lan_open && !self.no_bootstrap {
            let _ = spawn_lan_bootstrap_server(
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_LAN_BOOTSTRAP_PORT)),
                lan_bootstrap_bundle(&material, self.service_port)?,
            )?;
        }
        if !self.no_discovery {
            let advertisement = if self.legacy_lan_open {
                LanNodeAdvertisementV1::new(
                    material.node_id.clone(),
                    material.server_name.clone(),
                    self.service_port,
                    DEFAULT_LAN_BOOTSTRAP_PORT,
                    self.supports_v2,
                )?
            } else {
                LanNodeAdvertisementV1::pairing_required(
                    material.node_id.clone(),
                    material.server_name.clone(),
                    self.service_port,
                    DEFAULT_LAN_PAIRING_PORT,
                    self.supports_v2,
                )?
            };
            let _ = spawn_lan_discovery_responder(advertisement, DEFAULT_LAN_DISCOVERY_PORT)?;
        }
        Ok(())
    }
}

fn report_listener_ready(
    listening_address: SocketAddr,
    node_id: &str,
    maximum_connections: usize,
    service_summary: &str,
    lan_services: LanServicesAfterBind,
    shutdown: Option<&HostedDualNodeShutdown>,
    managed_start_token: Option<&str>,
) -> Result<()> {
    lan_services.start()?;
    if shutdown.is_some_and(HostedDualNodeShutdown::is_requested) {
        bail!("o-node shutdown was requested before listener readiness publication");
    }
    let mut output = format!(
        "o-node: serving {node_id} on {listening_address} ({service_summary}; max {maximum_connections} connections)\n"
    );
    if let Some(token) = managed_start_token {
        output.push_str(&format!(
            "{DETACHED_LISTENER_READY_LOG_PREFIX}{token} address={listening_address}; node={node_id}; {service_summary}; max {maximum_connections} connections\n"
        ));
    }
    let mut stderr = io::stderr().lock();
    stderr.write_all(output.as_bytes())?;
    Ok(())
}

fn ensure_lan_open_material() -> Result<LanOpenNodeMaterial> {
    ensure_lan_open_material_with_pki_key_algorithm(PkiKeyAlgorithm::Rsa3072)
}

fn ensure_lan_open_material_with_pki_key_algorithm(
    pki_key_algorithm: PkiKeyAlgorithm,
) -> Result<LanOpenNodeMaterial> {
    let config_dir = lan_open_config_dir();
    ensure_private_directory(&config_dir)?;
    let server_name = automatic_server_name()?;
    let node_id_path = config_dir.join("node-id");
    let node_id = load_or_create_automatic_node_id(&node_id_path, &server_name)?;

    let pki_dir = config_dir.join("pki");
    let server_name_path = pki_dir.join("server-name");
    let required_pki = [
        "ca.pem",
        "ca-key.pem",
        "node-cert.pem",
        "node-key.pem",
        "client-cert.pem",
        "client-key.pem",
    ];
    let complete = required_pki.iter().all(|name| pki_dir.join(name).is_file());
    let matching_name =
        fs::read_to_string(&server_name_path).is_ok_and(|value| value.trim() == server_name);
    if !complete || !matching_name {
        if pki_dir.exists() {
            let backup = sibling_backup_path(&pki_dir, "stale-pki")?;
            fs::rename(&pki_dir, &backup).with_context(|| {
                format!(
                    "failed to archive stale automatic PKI `{}` as `{}`",
                    pki_dir.display(),
                    backup.display()
                )
            })?;
            eprintln!(
                "o-node: archived stale automatic LAN identity at {}",
                backup.display()
            );
        }
        init_development_pki(
            PkiInitArgs {
                directory: Some(pki_dir.clone()),
                server_name: server_name.clone(),
                openssl: PathBuf::from("openssl"),
            },
            pki_key_algorithm,
        )?;
        fs::write(&server_name_path, format!("{server_name}\n"))?;
    }

    // Pairing client authentication is intentionally separate from the
    // historical LAN-open CA. Generate it as an independent, stable migration
    // so adding these files can never rotate the existing node certificate.
    let (pairing_ca, pairing_ca_key) = ensure_pairing_ca(&pki_dir, pki_key_algorithm)?;
    let client_ca_bundle = pki_dir.join("client-ca-bundle.pem");
    refresh_client_ca_bundle(&pairing_ca, &pki_dir.join("ca.pem"), &client_ca_bundle)?;

    let state_dir = lan_open_v2_state_dir();
    let node_signing_key = state_dir.join("node-signing-key.v2");
    let node_public_key = state_dir.join("node-signing-public.v2");
    match (node_signing_key.is_file(), node_public_key.is_file()) {
        (true, true) => {}
        (false, false) => init_v2_identity(IdentityInitArgs {
            state_dir: Some(state_dir.clone()),
            node_signing_key: Some(node_signing_key.clone()),
            node_public_key: Some(node_public_key.clone()),
        })?,
        (true, false) => {
            let signer = read_node_signing_key_v2(&node_signing_key)?;
            write_new_node_public_key_v2(&node_public_key, &signer.public_key())?;
        }
        (false, true) => {
            let backup = sibling_backup_path(&state_dir, "orphaned-v2")?;
            fs::rename(&state_dir, &backup).with_context(|| {
                format!(
                    "failed to archive incomplete automatic V2 state at `{}`",
                    state_dir.display()
                )
            })?;
            eprintln!(
                "o-node: archived incomplete automatic V2 state at {}",
                backup.display()
            );
            init_v2_identity(IdentityInitArgs {
                state_dir: Some(state_dir.clone()),
                node_signing_key: Some(node_signing_key.clone()),
                node_public_key: Some(node_public_key.clone()),
            })?;
        }
    }

    Ok(LanOpenNodeMaterial {
        node_id,
        server_name,
        pki_dir,
        state_dir,
        node_signing_key,
        node_public_key,
        pairing_ca,
        pairing_ca_key,
        client_ca_bundle,
    })
}

fn ensure_pairing_ca(
    pki_dir: &Path,
    pki_key_algorithm: PkiKeyAlgorithm,
) -> Result<(PathBuf, PathBuf)> {
    let certificate = pki_dir.join("pairing-ca.pem");
    let private_key = pki_dir.join("pairing-ca-key.pem");
    let certificate_exists = secure_regular_file_exists(&certificate)?;
    let private_key_exists = secure_regular_file_exists(&private_key)?;
    match (certificate_exists, private_key_exists) {
        (true, true) => {
            secure_key(private_key.clone())?;
            verify_pairing_ca(pki_dir)?;
            return Ok((certificate, private_key));
        }
        (true, false) | (false, true) => {
            bail!(
                "automatic pairing CA is incomplete (certificate={} key={}); refusing to rotate it because that would invalidate paired nodes",
                certificate.display(),
                private_key.display()
            );
        }
        (false, false) => {}
    }

    let temporary = create_private_temp_dir(pki_dir)?;
    fs::write(
        temporary.path().join("pairing-ca.cnf"),
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3_ca\n[dn]\nCN=Ostadix Paired Client Authentication CA\n[v3_ca]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always,issuer\n",
    )?;
    eprintln!(
        "o-node: generating pairing CA key ({})",
        pki_key_algorithm.cli_name()
    );
    let mut command = vec!["req", "-x509", "-newkey"];
    command.extend_from_slice(pki_key_algorithm.openssl_new_key_args());
    command.extend_from_slice(&[
        "-sha256",
        "-days",
        "3650",
        "-nodes",
        "-keyout",
        "pairing-ca-key.pem",
        "-out",
        "pairing-ca.pem",
        "-config",
        "pairing-ca.cnf",
    ]);
    run_openssl(Path::new("openssl"), temporary.path(), &command)?;
    secure_key(temporary.path().join("pairing-ca-key.pem"))?;

    let mut installed = Vec::new();
    for name in ["pairing-ca-key.pem", "pairing-ca.pem"] {
        let target = pki_dir.join(name);
        if let Err(error) = fs::hard_link(temporary.path().join(name), &target) {
            for created in &installed {
                let _ = fs::remove_file(created);
            }
            return Err(error).with_context(|| {
                format!(
                    "failed to install stable pairing CA file `{}`",
                    target.display()
                )
            });
        }
        installed.push(target);
    }
    verify_pairing_ca(pki_dir)?;
    println!("pairing CA key algorithm: {}", pki_key_algorithm.cli_name());
    Ok((certificate, private_key))
}

fn secure_regular_file_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!(
                    "pairing identity path `{}` must be a regular file",
                    path.display()
                );
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect `{}`", path.display())),
    }
}

fn verify_pairing_ca(pki_dir: &Path) -> Result<()> {
    let openssl = Path::new("openssl");
    run_openssl(
        openssl,
        pki_dir,
        &["x509", "-in", "pairing-ca.pem", "-noout", "-checkend", "0"],
    )?;
    run_openssl(
        openssl,
        pki_dir,
        &["verify", "-CAfile", "pairing-ca.pem", "pairing-ca.pem"],
    )?;
    let certificate_public = run_openssl_capture(
        openssl,
        pki_dir,
        &["x509", "-in", "pairing-ca.pem", "-pubkey", "-noout"],
    )?;
    let key_public = run_openssl_capture(
        openssl,
        pki_dir,
        &["pkey", "-in", "pairing-ca-key.pem", "-pubout"],
    )?;
    if certificate_public != key_public {
        bail!("pairing CA certificate and private key do not match; refusing silent rotation");
    }
    Ok(())
}

fn refresh_client_ca_bundle(pairing_ca: &Path, legacy_ca: &Path, destination: &Path) -> Result<()> {
    let mut bytes = fs::read(pairing_ca)
        .with_context(|| format!("failed to read pairing CA `{}`", pairing_ca.display()))?;
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend(
        fs::read(legacy_ca)
            .with_context(|| format!("failed to read legacy LAN CA `{}`", legacy_ca.display()))?,
    );
    write_file_atomic(destination, &bytes, false)
}

fn load_or_create_automatic_node_id(path: &Path, server_name: &str) -> Result<String> {
    if path.is_file() {
        let candidate = fs::read_to_string(path)
            .with_context(|| format!("failed to read `{}`", path.display()))?
            .trim()
            .to_owned();
        if o_lang::hosted_remote::NodeProfileV1::local(candidate.clone(), DEFAULT_MAX_CONNECTIONS)
            .is_ok()
        {
            return Ok(candidate);
        }
        let backup = sibling_backup_path(path, "invalid-node-id")?;
        fs::rename(path, &backup).with_context(|| {
            format!(
                "failed to archive invalid automatic node identity `{}`",
                path.display()
            )
        })?;
        eprintln!(
            "o-node: archived invalid automatic node identity at {}",
            backup.display()
        );
    }

    let generated = generate_automatic_node_id(server_name)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(format!("{generated}\n").as_bytes())?;
            file.sync_all()?;
            Ok(generated)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Concurrent starts converge on whichever stable identity won the
            // create-new race instead of making one invocation fail.
            let candidate = fs::read_to_string(path)
                .with_context(|| format!("failed to read raced-in `{}`", path.display()))?
                .trim()
                .to_owned();
            o_lang::hosted_remote::NodeProfileV1::local(candidate.clone(), DEFAULT_MAX_CONNECTIONS)
                .context("concurrently created automatic node identity is invalid")?;
            Ok(candidate)
        }
        Err(error) => Err(error).with_context(|| format!("failed to create `{}`", path.display())),
    }
}

fn generate_automatic_node_id(server_name: &str) -> Result<String> {
    let mut random = [0_u8; 4];
    getrandom::fill(&mut random).context("failed to generate automatic node identity")?;
    let host = server_name.trim_end_matches(".local");
    let generated = format!("ostadix-{host}-{}", hex::encode(random));
    o_lang::hosted_remote::NodeProfileV1::local(generated.clone(), DEFAULT_MAX_CONNECTIONS)
        .context("generated automatic node identity is invalid")?;
    Ok(generated)
}

fn sibling_backup_path(path: &Path, label: &str) -> Result<PathBuf> {
    let parent = path
        .parent()
        .context("automatic state path has no parent")?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("automatic state path has no portable file name")?;
    for _ in 0..32 {
        let mut random = [0_u8; 6];
        getrandom::fill(&mut random).context("failed to generate backup identity")?;
        let candidate = parent.join(format!("{name}.{label}-{}", hex::encode(random)));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("failed to allocate a unique automatic-state backup path")
}

fn automatic_server_name() -> Result<String> {
    let raw = env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            ProcessCommand::new("hostname")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
        });
    let raw = raw.unwrap_or_else(|| "ostadix-node".to_owned());
    let first = raw.trim().split('.').next().unwrap_or("ostadix-node");
    let mut host = String::with_capacity(first.len().min(63));
    let mut previous_dash = false;
    for character in first.chars() {
        if host.len() >= 63 {
            break;
        }
        let normalized = if character.is_ascii_alphanumeric() {
            previous_dash = false;
            Some(character.to_ascii_lowercase())
        } else if !previous_dash {
            previous_dash = true;
            Some('-')
        } else {
            None
        };
        if let Some(character) = normalized {
            host.push(character);
        }
    }
    let host = host.trim_matches('-');
    let host = if host.is_empty() {
        "ostadix-node"
    } else {
        host
    };
    let name = format!("{host}.local");
    certificate_san(&name)?;
    Ok(name)
}

fn lan_bootstrap_bundle(
    material: &LanOpenNodeMaterial,
    service_port: u16,
) -> Result<LanBootstrapBundleV1> {
    let read = |name: &str| -> Result<String> {
        let path = material.pki_dir.join(name);
        fs::read_to_string(&path)
            .with_context(|| format!("failed to read automatic LAN identity `{}`", path.display()))
    };
    let node_receipt_public_key = fs::read_to_string(&material.node_public_key)
        .with_context(|| {
            format!(
                "failed to read automatic node receipt identity `{}`",
                material.node_public_key.display()
            )
        })?
        .trim()
        .to_owned();
    let bundle = LanBootstrapBundleV1 {
        schema: LAN_BOOTSTRAP_SCHEMA_V1.to_owned(),
        node_id: material.node_id.clone(),
        server_name: material.server_name.clone(),
        service_port,
        security_mode: LAN_SECURITY_MODE.to_owned(),
        ca_pem: read("ca.pem")?,
        client_cert_pem: read("client-cert.pem")?,
        client_key_pem: read("client-key.pem")?,
        node_receipt_public_key: Some(node_receipt_public_key),
    };
    bundle.validate()?;
    Ok(bundle)
}

fn pair_node(args: PairArgs) -> Result<()> {
    validate_pair_args(&args)?;
    let material = ensure_lan_open_material()?;
    match args.peer_node_id.as_deref() {
        None => offer_pairing(&material, &args),
        Some(peer_node_id) => join_pairing(&material, peer_node_id, &args),
    }
}

fn validate_pair_args(args: &PairArgs) -> Result<()> {
    if args.service_port == 0 {
        bail!("--service-port must be nonzero");
    }
    if !(1..=600).contains(&args.offer_timeout_seconds) {
        bail!("--offer-timeout-seconds must be between 1 and 600");
    }
    if !(1..=60).contains(&args.io_timeout_seconds) {
        bail!("--io-timeout-seconds must be between 1 and 60");
    }
    if !(1..=60_000).contains(&args.discovery_timeout_millis) {
        bail!("--discovery-timeout-millis must be between 1 and 60000");
    }
    if args.peer_node_id.is_none() {
        if args.passcode_stdin {
            bail!("--passcode-stdin is only valid when joining a named node");
        }
        if args.address.is_some() {
            bail!("--address is only valid when joining a named node");
        }
    }
    Ok(())
}

fn offer_pairing(material: &LanOpenNodeMaterial, args: &PairArgs) -> Result<()> {
    let listener = TcpListener::bind(&args.bind)
        .with_context(|| format!("failed to bind pairing listener `{}`", args.bind))?;
    listener
        .set_nonblocking(true)
        .context("failed to configure expiring pairing listener")?;
    let listen_address = listener.local_addr()?;
    let passcode = Zeroizing::new(generate_pairing_passcode()?);
    println!("Pairing node: {}", material.node_id);
    println!("Passcode: {}", passcode.as_str());
    println!(
        "Expires in {} seconds after one connection attempt.",
        args.offer_timeout_seconds
    );
    println!("On the other node run: o node pair {}", material.node_id);
    if listen_address.port() != DEFAULT_LAN_PAIRING_PORT {
        println!(
            "Custom listener port: pass `--address <this-node-ip>:{}` on the other node.",
            listen_address.port()
        );
    }
    io::stdout().flush()?;

    let deadline = Instant::now() + Duration::from_secs(args.offer_timeout_seconds);
    let (stream, source) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("pairing offer expired before another node connected");
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                return Err(error).context("pairing listener failed");
            }
        }
    };

    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("pairing offer expired as the one-use connection was accepted");
    }
    let io_timeout = Duration::from_secs(args.io_timeout_seconds).min(remaining);
    let outcome = accept_pairing_once(
        stream,
        passcode.as_str(),
        &material.node_id,
        io_timeout,
        |peer| prepare_pairing_local_identity(material, &peer.node_id, args.service_port),
        |peer| sign_pairing_client_csr(material, peer),
    );
    let result = outcome.context("pairing attempt failed; the one-use offer was consumed")?;
    verify_and_store_pairing(material, source.ip(), &result, args.replace)?;
    if let Err(error) = remember_preferred_pair(&result.peer.node_id) {
        eprintln!(
            "o-node: pairing succeeded, but the preferred-node marker could not be updated: {error:#}"
        );
    }
    println!("Paired with {}.", result.peer.node_id);
    println!("Both nodes retained their own private keys and saved reciprocal public identities.");
    Ok(())
}

fn join_pairing(material: &LanOpenNodeMaterial, peer_node_id: &str, args: &PairArgs) -> Result<()> {
    let passcode = read_pairing_passcode(args.passcode_stdin)?;
    let candidates = pairing_candidates(peer_node_id, args)?;
    let timeout = Duration::from_secs(args.io_timeout_seconds);
    let mut failures = Vec::new();
    let mut connected = None;
    for address in candidates {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                connected = Some((stream, address));
                break;
            }
            Err(error) => failures.push(format!("{address}: {error}")),
        }
    }
    let Some((stream, address)) = connected else {
        bail!(
            "could not reach pairing offer for `{peer_node_id}`{}",
            if failures.is_empty() {
                String::new()
            } else {
                format!(": {}", failures.join("; "))
            }
        );
    };

    let local = prepare_pairing_local_identity(material, peer_node_id, args.service_port)?;
    let outcome = join_pairing_once(
        stream,
        passcode.as_str(),
        peer_node_id,
        timeout,
        local,
        |peer| sign_pairing_client_csr(material, peer),
    );
    let result = outcome.context("pairing authentication failed")?;
    verify_and_store_pairing(material, address.ip(), &result, args.replace)?;
    if let Err(error) = remember_preferred_pair(&result.peer.node_id) {
        eprintln!(
            "o-node: pairing succeeded, but the preferred-node marker could not be updated: {error:#}"
        );
    }
    println!("Paired with {}.", result.peer.node_id);
    println!("Future `o node profile`, `run`, and `session` commands can reuse this identity.");
    Ok(())
}

fn read_pairing_passcode(from_stdin: bool) -> Result<Zeroizing<String>> {
    let mut passcode = if from_stdin {
        let mut line = Zeroizing::new(String::new());
        io::stdin()
            .lock()
            .read_line(&mut line)
            .context("failed to read pairing passcode from standard input")?;
        line
    } else {
        Zeroizing::new(
            rpassword::prompt_password("Passcode: ")
                .context("failed to read pairing passcode from the terminal")?,
        )
    };
    let trimmed_len = passcode.trim_end_matches(['\r', '\n']).len();
    passcode.truncate(trimmed_len);
    if passcode.is_empty() {
        bail!("pairing passcode cannot be empty");
    }
    Ok(passcode)
}

fn pairing_candidates(peer_node_id: &str, args: &PairArgs) -> Result<Vec<SocketAddr>> {
    if let Some(address) = &args.address {
        let mut addresses = address
            .to_socket_addrs()
            .with_context(|| format!("failed to resolve pairing endpoint `{address}`"))?
            .collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        if addresses.is_empty() {
            bail!("pairing endpoint `{address}` resolved to no socket addresses");
        }
        return Ok(addresses);
    }

    let mut matches = discover_lan_nodes(Duration::from_millis(args.discovery_timeout_millis))?
        .into_iter()
        .filter(|node| node.advertisement.node_id == peer_node_id)
        .map(|node| {
            if node.advertisement.is_pairing_required() {
                node.bootstrap_address()
            } else {
                SocketAddr::new(node.source_ip, DEFAULT_LAN_PAIRING_PORT)
            }
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.ip()
            .is_loopback()
            .cmp(&right.ip().is_loopback())
            .then_with(|| left.to_string().cmp(&right.to_string()))
    });
    matches.dedup();
    if matches.is_empty() {
        bail!(
            "node `{peer_node_id}` was not discovered; start it first or pass `--address HOST:PORT`"
        );
    }
    Ok(matches)
}

fn prepare_pairing_local_identity(
    material: &LanOpenNodeMaterial,
    peer_node_id: &str,
    service_port: u16,
) -> Result<PairingLocalIdentityV1> {
    let temporary = create_private_temp_dir(&material.pki_dir)?;
    let digest = Sha256::digest(format!("{}\0{peer_node_id}", material.node_id).as_bytes());
    let subject = format!("/CN=ostadix-pair-{}", hex::encode(&digest[..12]));
    run_openssl(
        Path::new("openssl"),
        temporary.path(),
        &[
            "req",
            "-new",
            "-newkey",
            "rsa:3072",
            "-sha256",
            "-nodes",
            "-keyout",
            "client-key.pem",
            "-out",
            "client.csr",
            "-subj",
            &subject,
        ],
    )?;
    secure_key(temporary.path().join("client-key.pem"))?;
    run_openssl(
        Path::new("openssl"),
        temporary.path(),
        &["req", "-in", "client.csr", "-noout", "-verify"],
    )?;

    let node_receipt_public_key = fs::read_to_string(&material.node_public_key)
        .with_context(|| {
            format!(
                "failed to read local receipt public key `{}`",
                material.node_public_key.display()
            )
        })?
        .trim()
        .to_owned();
    let public = PairingPublicIdentityV1 {
        node_id: material.node_id.clone(),
        server_name: material.server_name.clone(),
        service_port,
        supports_v2: true,
        server_ca_pem: fs::read_to_string(material.pki_dir.join("ca.pem"))?,
        client_issuer_ca_pem: fs::read_to_string(&material.pairing_ca)?,
        client_csr_pem: fs::read_to_string(temporary.path().join("client.csr"))?,
        node_receipt_public_key,
    };
    public.validate()?;
    PairingLocalIdentityV1::new(public, fs::read(temporary.path().join("client-key.pem"))?)
}

fn sign_pairing_client_csr(
    material: &LanOpenNodeMaterial,
    peer: &PairingPublicIdentityV1,
) -> Result<String> {
    peer.validate()?;
    let temporary = create_private_temp_dir(&material.pki_dir)?;
    fs::write(
        temporary.path().join("peer.csr"),
        peer.client_csr_pem.as_bytes(),
    )?;
    fs::write(
        temporary.path().join("client-ext.cnf"),
        "[client_ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=clientAuth\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n",
    )?;
    run_openssl(
        Path::new("openssl"),
        temporary.path(),
        &["req", "-in", "peer.csr", "-noout", "-verify"],
    )?;
    let mut serial = [0_u8; 16];
    getrandom::fill(&mut serial).context("failed to generate pairing certificate serial")?;
    let serial = format!("0x{}", hex::encode(serial));
    let pairing_ca = material
        .pairing_ca
        .to_str()
        .context("pairing CA path is not valid Unicode")?;
    let pairing_ca_key = material
        .pairing_ca_key
        .to_str()
        .context("pairing CA key path is not valid Unicode")?;
    run_openssl(
        Path::new("openssl"),
        temporary.path(),
        &[
            "x509",
            "-req",
            "-in",
            "peer.csr",
            "-CA",
            pairing_ca,
            "-CAkey",
            pairing_ca_key,
            "-set_serial",
            &serial,
            "-out",
            "client-cert.pem",
            "-days",
            "397",
            "-sha256",
            "-extfile",
            "client-ext.cnf",
            "-extensions",
            "client_ext",
        ],
    )?;
    run_openssl(
        Path::new("openssl"),
        temporary.path(),
        &[
            "verify",
            "-CAfile",
            pairing_ca,
            "-purpose",
            "sslclient",
            "client-cert.pem",
        ],
    )?;
    let csr_public = run_openssl_capture(
        Path::new("openssl"),
        temporary.path(),
        &["req", "-in", "peer.csr", "-pubkey", "-noout"],
    )?;
    let certificate_public = run_openssl_capture(
        Path::new("openssl"),
        temporary.path(),
        &["x509", "-in", "client-cert.pem", "-pubkey", "-noout"],
    )?;
    if csr_public != certificate_public {
        bail!("issued pairing certificate does not contain the authenticated CSR public key");
    }
    fs::read_to_string(temporary.path().join("client-cert.pem"))
        .context("failed to read issued pairing client certificate")
}

fn verify_and_store_pairing(
    material: &LanOpenNodeMaterial,
    peer_ip: std::net::IpAddr,
    result: &PairingResultV1,
    replace: bool,
) -> Result<()> {
    result.peer.validate()?;
    let temporary = create_private_temp_dir(&material.pki_dir)?;
    fs::write(
        temporary.path().join("server-ca.pem"),
        result.peer.server_ca_pem.as_bytes(),
    )?;
    fs::write(
        temporary.path().join("client-issuer-ca.pem"),
        result.peer.client_issuer_ca_pem.as_bytes(),
    )?;
    fs::write(
        temporary.path().join("client-cert.pem"),
        result.local_issued_client_cert_pem.as_bytes(),
    )?;
    fs::write(
        temporary.path().join("client-key.pem"),
        result.local_private_client_key_pem(),
    )?;
    secure_key(temporary.path().join("client-key.pem"))?;
    run_openssl(
        Path::new("openssl"),
        temporary.path(),
        &[
            "verify",
            "-CAfile",
            "client-issuer-ca.pem",
            "-purpose",
            "sslclient",
            "client-cert.pem",
        ],
    )?;
    let certificate_public = run_openssl_capture(
        Path::new("openssl"),
        temporary.path(),
        &["x509", "-in", "client-cert.pem", "-pubkey", "-noout"],
    )?;
    let key_public = run_openssl_capture(
        Path::new("openssl"),
        temporary.path(),
        &["pkey", "-in", "client-key.pem", "-pubout"],
    )?;
    if certificate_public != key_public {
        bail!("paired client certificate does not match the locally retained private key");
    }
    build_client_config(&ClientTlsIdentity {
        ca_path: temporary.path().join("server-ca.pem"),
        cert_path: temporary.path().join("client-cert.pem"),
        key_path: temporary.path().join("client-key.pem"),
        server_name: result.peer.server_name.clone(),
    })
    .context("paired TLS identity is unusable")?;

    let service_address = SocketAddr::new(peer_ip, result.peer.service_port);
    let peers_root = lan_peers_config_dir();
    let stored_paths = StoredLanPeerPathsV1::for_root(&peers_root, &result.peer.node_id)?;
    let replace_existing = if replace {
        match fs::symlink_metadata(&stored_paths.directory) {
            Ok(_) => load_stored_lan_peer(&peers_root, &result.peer.node_id)?
                .0
                .is_paired(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect existing paired identity `{}`",
                        stored_paths.directory.display()
                    )
                })
            }
        }
    } else {
        false
    };
    if replace_existing {
        replace_paired_lan_peer(
            &peers_root,
            service_address,
            &result.peer.node_id,
            &result.peer.server_name,
            result.peer.service_port,
            result.peer.supports_v2,
            &result.peer.server_ca_pem,
            &result.local_issued_client_cert_pem,
            result.local_private_client_key_pem(),
            Some(&result.peer.node_receipt_public_key),
        )?;
    } else {
        store_paired_lan_peer(
            &peers_root,
            service_address,
            &result.peer.node_id,
            &result.peer.server_name,
            result.peer.service_port,
            result.peer.supports_v2,
            &result.peer.server_ca_pem,
            &result.local_issued_client_cert_pem,
            result.local_private_client_key_pem(),
            Some(&result.peer.node_receipt_public_key),
        )?;
    }
    Ok(())
}

fn remember_preferred_pair(node_id: &str) -> Result<()> {
    let root = lan_peers_config_dir();
    ensure_private_directory(&root)?;
    write_file_atomic(
        &root.join("_preferred"),
        format!("{node_id}\n").as_bytes(),
        true,
    )
}

#[cfg(unix)]
fn detached_node_paths() -> (PathBuf, PathBuf, PathBuf) {
    let directory = lan_node_process_dir();
    (
        directory.clone(),
        directory.join("o-node.pid"),
        directory.join("o-node.log"),
    )
}

#[cfg(unix)]
fn ensure_detached_process_directory(path: &Path) -> Result<()> {
    ensure_private_directory(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set mode 0700 on `{}`", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn acquire_detached_lifecycle_lock(path: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to open startup lock `{}`", path.display()))?;
    lock.set_permissions(fs::Permissions::from_mode(0o600))?;
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(lock);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error)
                .with_context(|| format!("failed to lock startup lock `{}`", path.display()));
        }
    }
}

#[cfg(unix)]
fn register_detached_startup_interrupts() -> Result<Arc<AtomicBool>> {
    use signal_hook::consts::{SIGINT, SIGTERM};

    let interrupted = Arc::new(AtomicBool::new(false));
    // These registrations intentionally live until this short-lived `start`
    // command exits. Unregistering signal-hook actions does not restore the
    // previous/default disposition.
    let _ = signal_hook::flag::register(SIGINT, Arc::clone(&interrupted))
        .context("failed to register SIGINT handling for detached startup")?;
    let _ = signal_hook::flag::register(SIGTERM, Arc::clone(&interrupted))
        .context("failed to register SIGTERM handling for detached startup")?;
    Ok(interrupted)
}

#[cfg(unix)]
struct DetachedStartupLogObserver {
    file: File,
    ready_line_prefix: Vec<u8>,
    matching_line_prefix: bool,
    matched_prefix_bytes: usize,
    ready_line_prefix_matched: bool,
    listener_ready: bool,
}

#[cfg(unix)]
impl DetachedStartupLogObserver {
    fn open(path: &Path, start_offset: u64, launch_token: &str) -> io::Result<Self> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(start_offset))?;
        Ok(Self {
            file,
            ready_line_prefix: format!(
                "{DETACHED_LISTENER_READY_LOG_PREFIX}{launch_token} address="
            )
            .into_bytes(),
            matching_line_prefix: true,
            matched_prefix_bytes: 0,
            ready_line_prefix_matched: false,
            listener_ready: false,
        })
    }

    fn poll_listener_ready(&mut self) -> io::Result<bool> {
        if self.listener_ready {
            return Ok(true);
        }
        let prefix = &self.ready_line_prefix;
        let mut buffer = [0_u8; 4096];
        let mut remaining_bytes = DETACHED_STARTUP_LOG_POLL_BYTES;
        while remaining_bytes > 0 {
            let read_length = buffer.len().min(remaining_bytes);
            let length = self.file.read(&mut buffer[..read_length])?;
            if length == 0 {
                return Ok(self.listener_ready);
            }
            remaining_bytes -= length;
            for byte in &buffer[..length] {
                if *byte == b'\n' {
                    if self.ready_line_prefix_matched {
                        self.listener_ready = true;
                        return Ok(true);
                    }
                    self.matching_line_prefix = true;
                    self.matched_prefix_bytes = 0;
                    self.ready_line_prefix_matched = false;
                    continue;
                }
                if !self.matching_line_prefix {
                    continue;
                }
                if *byte != prefix[self.matched_prefix_bytes] {
                    self.matching_line_prefix = false;
                    continue;
                }
                self.matched_prefix_bytes += 1;
                if self.matched_prefix_bytes == prefix.len() {
                    self.matching_line_prefix = false;
                    self.ready_line_prefix_matched = true;
                }
            }
        }
        Ok(false)
    }
}

#[cfg(unix)]
struct DetachedStartupChildGuard {
    child: Child,
    pid_path: PathBuf,
    pid: u32,
    armed: bool,
}

#[cfg(unix)]
impl DetachedStartupChildGuard {
    fn new(child: Child, pid_path: PathBuf) -> Self {
        let pid = child.id();
        Self {
            child,
            pid_path,
            pid,
            armed: true,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        if self.child.try_wait()?.is_none() {
            let signal_result = unsafe { libc::kill(self.pid as i32, libc::SIGTERM) };
            if signal_result != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            let deadline = Instant::now() + DETACHED_STARTUP_TERMINATION_GRACE;
            while Instant::now() < deadline {
                if self.child.try_wait()?.is_some() {
                    break;
                }
                thread::sleep(DETACHED_STARTUP_POLL_INTERVAL);
            }
            if self.child.try_wait()?.is_none() {
                self.child.kill()?;
                self.child.wait()?;
            }
        }
        remove_pid_file_if_matches(&self.pid_path, self.pid)?;
        self.armed = false;
        Ok(())
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

#[cfg(unix)]
impl Drop for DetachedStartupChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.cleanup();
        }
    }
}

#[cfg(unix)]
fn remove_pid_file_if_matches(path: &Path, expected_pid: u32) -> io::Result<()> {
    match fs::read_to_string(path) {
        Ok(value)
            if value
                .lines()
                .next()
                .and_then(|line| line.trim().parse::<u32>().ok())
                == Some(expected_pid) =>
        {
            fs::remove_file(path)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn new_detached_launch_token() -> Result<String> {
    let mut bytes = [0_u8; DETACHED_LAUNCH_TOKEN_BYTES];
    getrandom::fill(&mut bytes).context("failed to generate detached launch identity")?;
    Ok(hex::encode(bytes))
}

fn validate_detached_launch_token(token: &str) -> Result<()> {
    if token.len() != DETACHED_LAUNCH_TOKEN_BYTES * 2
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid detached launch identity")
    }
    Ok(())
}

fn start_detached_node(args: StartArgs) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = args;
        bail!("detached o-node start is currently supported on Unix-like systems");
    }
    #[cfg(unix)]
    {
        let (directory, pid_path, log_path) = detached_node_paths();
        ensure_detached_process_directory(&directory)?;
        let _lifecycle_lock =
            acquire_detached_lifecycle_lock(&directory.join("o-node.lifecycle.lock"))?;
        start_detached_node_locked(args, pid_path, log_path)
    }
}

#[cfg(unix)]
fn start_detached_node_locked(args: StartArgs, pid_path: PathBuf, log_path: PathBuf) -> Result<()> {
    if detached_node_status_locked(false, &pid_path, &log_path)? {
        println!("o-node is already running (managed PID exists; readiness was not re-probed)");
        return Ok(());
    }
    // Provision synchronously so configuration errors are shown in this
    // terminal instead of being buried in a detached log.
    let material = ensure_lan_open_material_with_pki_key_algorithm(args.fresh_pki_key_algorithm)?;
    println!(
        "fresh PKI key algorithm selection: {}",
        args.fresh_pki_key_algorithm.cli_name()
    );
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&log_path)
        .with_context(|| format!("failed to open `{}`", log_path.display()))?;
    let log_metadata = log.metadata()?;
    if !log_metadata.is_file() {
        bail!(
            "detached node log `{}` is not a regular file",
            log_path.display()
        );
    }
    log.set_permissions(fs::Permissions::from_mode(0o600))?;
    if log_metadata.len() > 0 {
        writeln!(&mut log)?;
    }
    writeln!(&mut log, "=== o-node detached start ===")?;
    log.flush()?;
    let startup_log_offset = log.metadata()?.len();
    let launch_token = new_detached_launch_token()?;
    let mut startup_log =
        DetachedStartupLogObserver::open(&log_path, startup_log_offset, &launch_token)
            .with_context(|| format!("failed to observe `{}`", log_path.display()))?;
    let startup_interrupted = register_detached_startup_interrupts()?;
    let current = env::current_exe().context("failed to locate o-node executable")?;
    let mut command = ProcessCommand::new(current);
    command
        .arg("serve")
        .arg("--managed-start-token")
        .arg(&launch_token)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    if args.lan_open {
        command.arg("--lan-open");
    }
    // SAFETY: setsid is async-signal-safe and the closure performs no
    // allocation or lock-taking after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().context("failed to detach o-node serve")?;
    let mut child = DetachedStartupChildGuard::new(child, pid_path.clone());
    if let Err(error) = write_file_atomic(
        &pid_path,
        format!("{}\n{launch_token}\n", child.child_mut().id()).as_bytes(),
        true,
    ) {
        let error = error.context("failed to persist detached o-node PID");
        if let Err(cleanup_error) = child.cleanup() {
            return Err(error).context(format!(
                "startup cleanup also failed; retained PID tracking at `{}`: {cleanup_error}",
                pid_path.display()
            ));
        }
        return Err(error);
    }
    let startup_timeout = Duration::from_secs(args.startup_timeout_seconds);
    if let Err(error) = wait_for_detached_node_startup(
        child.child_mut(),
        &mut startup_log,
        &log_path,
        startup_log_offset,
        startup_timeout,
        &startup_interrupted,
    ) {
        if let Err(cleanup_error) = child.cleanup() {
            return Err(error).context(format!(
                "startup cleanup also failed; retained PID tracking at `{}`: {cleanup_error}",
                pid_path.display()
            ));
        }
        return Err(error);
    }
    if startup_interrupted.load(Ordering::SeqCst) {
        let error = anyhow::anyhow!(
            "o-node startup interrupted by SIGINT or SIGTERM{}",
            format_startup_diagnostics(&log_path, startup_log_offset)
        );
        if let Err(cleanup_error) = child.cleanup() {
            return Err(error).context(format!(
                "startup cleanup also failed; retained PID tracking at `{}`: {cleanup_error}",
                pid_path.display()
            ));
        }
        return Err(error);
    }
    child.disarm();
    println!("o-node started: {}", material.node_id);
    println!("log: {}", log_path.display());
    println!(
        "scheduler/actor mesh: {} (automatic)",
        material.state_dir.join("mesh-v1").display()
    );
    if args.lan_open {
        println!("legacy LAN-open enrollment enabled explicitly");
    } else {
        println!("pair from this machine with `o node pair`");
    }
    Ok(())
}

#[cfg(unix)]
fn wait_for_detached_node_startup(
    child: &mut Child,
    startup_log: &mut DetachedStartupLogObserver,
    log_path: &Path,
    startup_log_offset: u64,
    timeout: Duration,
    interrupted: &AtomicBool,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if interrupted.load(Ordering::SeqCst) {
            bail!(
                "o-node startup interrupted by SIGINT or SIGTERM{}",
                format_startup_diagnostics(log_path, startup_log_offset)
            );
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "o-node exited during startup with {status}{}",
                format_startup_diagnostics(log_path, startup_log_offset)
            );
        }
        if startup_log.poll_listener_ready()? {
            // Resolve an exit racing with the readiness write before reporting
            // success to the caller.
            if let Some(status) = child.try_wait()? {
                bail!(
                    "o-node exited during startup with {status}{}",
                    format_startup_diagnostics(log_path, startup_log_offset)
                );
            }
            if interrupted.load(Ordering::SeqCst) {
                bail!(
                    "o-node startup interrupted by SIGINT or SIGTERM{}",
                    format_startup_diagnostics(log_path, startup_log_offset)
                );
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "o-node did not bind its hosted listener within {} seconds{}",
                timeout.as_secs(),
                format_startup_diagnostics(log_path, startup_log_offset)
            );
        }
        thread::sleep(DETACHED_STARTUP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn format_startup_diagnostics(path: &Path, start_offset: u64) -> String {
    match read_startup_log_excerpt(path, start_offset, DETACHED_STARTUP_LOG_EXCERPT_BYTES) {
        Ok(Some(excerpt)) => format!(
            "\nstartup diagnostics:\n{excerpt}\nfull log: {}",
            path.display()
        ),
        Ok(None) => format!("; inspect {}", path.display()),
        Err(error) => format!(
            "; inspect {} (failed to read startup diagnostics: {error})",
            path.display()
        ),
    }
}

#[cfg(unix)]
fn read_startup_log_excerpt(
    path: &Path,
    start_offset: u64,
    maximum_bytes: usize,
) -> io::Result<Option<String>> {
    if maximum_bytes == 0 {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let end_offset = file.metadata()?.len();
    if end_offset <= start_offset {
        return Ok(None);
    }
    let maximum_bytes = u64::try_from(maximum_bytes).unwrap_or(u64::MAX);
    let read_offset = start_offset.max(end_offset.saturating_sub(maximum_bytes));
    file.seek(SeekFrom::Start(read_offset))?;
    let mut bytes = Vec::new();
    file.take(maximum_bytes).read_to_end(&mut bytes)?;
    let excerpt = String::from_utf8_lossy(&bytes);
    let excerpt = excerpt.trim();
    if excerpt.is_empty() {
        return Ok(None);
    }
    if read_offset > start_offset {
        Ok(Some(format!("[... startup log truncated ...]\n{excerpt}")))
    } else {
        Ok(Some(excerpt.to_owned()))
    }
}

fn restart_detached_node(args: StartArgs) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = args;
        bail!("detached o-node restart is currently supported on Unix-like systems");
    }
    #[cfg(unix)]
    {
        let (directory, pid_path, log_path) = detached_node_paths();
        ensure_detached_process_directory(&directory)?;
        let _lifecycle_lock =
            acquire_detached_lifecycle_lock(&directory.join("o-node.lifecycle.lock"))?;
        stop_detached_node_locked(&pid_path)?;
        if detached_node_status_locked(false, &pid_path, &log_path)? {
            bail!("o-node is still running after stop; refusing to start a duplicate")
        }
        start_detached_node_locked(args, pid_path, log_path)
    }
}

fn stop_detached_node() -> Result<()> {
    #[cfg(not(unix))]
    bail!("detached o-node stop is currently supported on Unix-like systems");
    #[cfg(unix)]
    {
        let (directory, pid_path, _) = detached_node_paths();
        ensure_detached_process_directory(&directory)?;
        let _lifecycle_lock =
            acquire_detached_lifecycle_lock(&directory.join("o-node.lifecycle.lock"))?;
        stop_detached_node_locked(&pid_path)
    }
}

#[cfg(unix)]
fn stop_detached_node_locked(pid_path: &Path) -> Result<()> {
    let Some(identity) = read_detached_pid(pid_path)? else {
        println!("o-node is not running");
        return Ok(());
    };
    let pid = identity.pid;
    if !process_is_detached_node(&identity)? {
        remove_pid_file_if_matches(pid_path, pid as u32)?;
        println!("o-node was not running; removed stale PID file");
        return Ok(());
    }
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to signal o-node");
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !process_is_alive(pid) {
            remove_pid_file_if_matches(pid_path, pid as u32)?;
            println!("o-node stopped");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!("o-node is still draining accepted work after 10 seconds")
}

fn detached_node_status(print: bool) -> Result<bool> {
    #[cfg(not(unix))]
    {
        let _ = print;
        bail!("detached o-node status is currently supported on Unix-like systems");
    }
    #[cfg(unix)]
    {
        let (directory, pid_path, log_path) = detached_node_paths();
        if !directory.exists() {
            if print {
                println!("stopped (no managed PID; unmanaged listeners are not probed)");
            }
            return Ok(false);
        }
        ensure_detached_process_directory(&directory)?;
        let _lifecycle_lock =
            acquire_detached_lifecycle_lock(&directory.join("o-node.lifecycle.lock"))?;
        detached_node_status_locked(print, &pid_path, &log_path)
    }
}

#[cfg(unix)]
fn detached_node_status_locked(print: bool, pid_path: &Path, log_path: &Path) -> Result<bool> {
    let Some(identity) = read_detached_pid(pid_path)? else {
        if print {
            println!("stopped (no managed PID; unmanaged listeners are not probed)");
        }
        return Ok(false);
    };
    let pid = identity.pid;
    let running = process_is_detached_node(&identity)?;
    if !running {
        remove_pid_file_if_matches(pid_path, pid as u32)?;
    }
    if print {
        if running {
            println!("running pid={pid} log={}", log_path.display());
        } else {
            println!("stopped (stale managed PID removed; unmanaged listeners are not probed)");
        }
    }
    Ok(running)
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedProcessIdentity {
    pid: i32,
    launch_token: Option<String>,
}

#[cfg(unix)]
fn read_detached_pid(path: &Path) -> Result<Option<DetachedProcessIdentity>> {
    if !path.is_file() {
        return Ok(None);
    }
    let value = fs::read_to_string(path)?;
    let mut lines = value.lines();
    let pid = lines
        .next()
        .context("o-node PID file is empty")?
        .trim()
        .parse::<i32>()
        .with_context(|| format!("invalid o-node PID file `{}`", path.display()))?;
    if pid <= 0 {
        bail!("invalid o-node PID {pid}");
    }
    let launch_token = lines.next().map(str::trim).map(str::to_owned);
    if lines.next().is_some() {
        bail!("invalid o-node PID file `{}`", path.display());
    }
    if let Some(token) = launch_token.as_deref() {
        validate_detached_launch_token(token)
            .with_context(|| format!("invalid o-node PID file `{}`", path.display()))?;
    }
    Ok(Some(DetachedProcessIdentity { pid, launch_token }))
}

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn inspect_live_process_with_ps(pid: i32, field: &str) -> Result<Option<String>> {
    let pid_value = pid.to_string();
    let field_specification = format!("{field}=");
    let output = ProcessCommand::new("ps")
        .args(["-ww", "-p", &pid_value, "-o", &field_specification])
        .output();
    let output = match output {
        Ok(output) => output,
        Err(_) if !process_is_alive(pid) => return Ok(None),
        Err(error) => {
            return Err(error).context(format!(
                "failed to inspect managed o-node PID {pid} field `{field}`"
            ))
        }
    };
    if !output.status.success() {
        if !process_is_alive(pid) {
            return Ok(None);
        }
        bail!(
            "could not inspect live managed o-node PID {pid}: `ps -o {field}=` exited with {}",
            output.status
        );
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        if !process_is_alive(pid) {
            return Ok(None);
        }
        bail!("could not inspect live managed o-node PID {pid}: `ps -o {field}=` was empty");
    }
    Ok(Some(value))
}

#[cfg(unix)]
fn detached_executable_matches(executable: &Path, expected: &Path) -> bool {
    if executable.is_absolute() && expected.is_absolute() {
        executable == expected
    } else {
        executable
            .file_name()
            .zip(expected.file_name())
            .is_some_and(|(actual, expected)| actual == expected)
    }
}

#[cfg(unix)]
fn detached_ps_command_arguments(
    observed_executable: &str,
    expected: &Path,
    command: &str,
) -> Option<Vec<String>> {
    if !detached_executable_matches(Path::new(observed_executable), expected) {
        return None;
    }
    let command = command.trim();
    let expected_text = expected.to_string_lossy();
    let argument_text = command
        .strip_prefix(expected_text.as_ref())
        .and_then(|remainder| {
            remainder
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
                .then_some(remainder)
        });
    if let Some(argument_text) = argument_text {
        let mut arguments = vec![expected_text.into_owned()];
        arguments.extend(argument_text.split_whitespace().map(str::to_owned));
        return Some(arguments);
    }

    // If `command=` also exposes only a basename, accept an unambiguous,
    // whitespace-free argv[0]. A different or truncated path fails closed.
    let mut fields = command.split_whitespace();
    let argument_zero = fields.next()?;
    if !detached_executable_matches(Path::new(argument_zero), expected) {
        return None;
    }
    let mut arguments = vec![expected_text.into_owned()];
    arguments.extend(fields.map(str::to_owned));
    Some(arguments)
}

#[cfg(unix)]
fn process_is_detached_node(identity: &DetachedProcessIdentity) -> Result<bool> {
    let pid = identity.pid;
    if !process_is_alive(pid) {
        return Ok(false);
    }
    let expected = env::current_exe().context("failed to locate the current o-node executable")?;

    #[cfg(target_os = "linux")]
    {
        if let Ok(bytes) = fs::read(format!("/proc/{pid}/cmdline")) {
            let arguments = bytes
                .split(|byte| *byte == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect::<Vec<_>>();
            return Ok(detached_command_matches(
                &arguments,
                &expected,
                identity.launch_token.as_deref(),
            ));
        }
    }

    let Some(executable) = inspect_live_process_with_ps(pid, "comm")? else {
        return Ok(false);
    };
    let Some(command) = inspect_live_process_with_ps(pid, "command")? else {
        return Ok(false);
    };
    let arguments = detached_ps_command_arguments(&executable, &expected, &command).with_context(|| {
        format!(
            "could not reconcile `ps` executable and command fields for live managed o-node PID {pid}"
        )
    })?;
    Ok(detached_command_matches(
        &arguments,
        &expected,
        identity.launch_token.as_deref(),
    ))
}

#[cfg(unix)]
fn detached_command_matches(
    arguments: &[String],
    expected: &Path,
    launch_token: Option<&str>,
) -> bool {
    let Some(executable) = arguments.first() else {
        return false;
    };
    let executable = Path::new(executable);
    let executable_matches = detached_executable_matches(executable, expected);
    let token_matches = launch_token.is_none_or(|expected_token| {
        arguments
            .windows(2)
            .any(|pair| pair[0] == "--managed-start-token" && pair[1].as_str() == expected_token)
    });
    executable_matches
        && token_matches
        && arguments.iter().skip(1).any(|argument| argument == "serve")
}

fn gc_closed_session(args: AdminGcClosedArgs) -> Result<()> {
    let state_dir = args.state_dir.unwrap_or_else(default_hosted_v2_state_dir);
    let key_path = args
        .node_signing_key
        .unwrap_or_else(|| state_dir.join("node-signing-key.v2"));
    let signer = read_node_signing_key_v2(&key_path)?;
    let store = DurableSessionStoreV2::open(&state_dir, signer)?;
    let receipt = store.gc_closed_session(&args.session_id)?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    eprintln!(
        "o-node: permanently removed closed session {} (session files are not recoverable; signed authority-journal anchors remain)",
        args.session_id
    );
    Ok(())
}

fn init_development_pki(args: PkiInitArgs, pki_key_algorithm: PkiKeyAlgorithm) -> Result<()> {
    let san = certificate_san(&args.server_name)?;
    let destination = args.directory.unwrap_or_else(hosted_config_dir);
    ensure_private_directory(&destination)?;
    let installed_names = [
        "ca.pem",
        "ca-key.pem",
        "node-cert.pem",
        "node-key.pem",
        "client-cert.pem",
        "client-key.pem",
    ];
    let existing = installed_names
        .iter()
        .map(|name| destination.join(name))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if !existing.is_empty() {
        bail!(
            "refusing to overwrite existing PKI files: {}",
            existing
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let temporary = create_private_temp_dir(&destination)?;
    fs::write(
        temporary.path().join("ca.cnf"),
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3_ca\n[dn]\nCN=Ostadix Hosted Development CA\n[v3_ca]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always,issuer\n",
    )?;
    fs::write(
        temporary.path().join("node.cnf"),
        format!(
            "[req]\nprompt=no\ndistinguished_name=dn\nreq_extensions=node_ext\n[dn]\nCN={}\n[node_ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,{}\nextendedKeyUsage=serverAuth\nsubjectAltName={}\n",
            args.server_name,
            pki_key_algorithm.tls_key_usage(),
            san
        ),
    )?;
    fs::write(
        temporary.path().join("client.cnf"),
        format!(
            "[req]\nprompt=no\ndistinguished_name=dn\nreq_extensions=client_ext\n[dn]\nCN=ostadix-development-client\n[client_ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,{}\nextendedKeyUsage=clientAuth\n",
            pki_key_algorithm.tls_key_usage()
        ),
    )?;

    eprintln!(
        "o-node: generating development CA key ({})",
        pki_key_algorithm.cli_name()
    );
    let mut command = vec!["req", "-x509", "-newkey"];
    command.extend_from_slice(pki_key_algorithm.openssl_new_key_args());
    command.extend_from_slice(&[
        "-sha256",
        "-days",
        "3650",
        "-nodes",
        "-keyout",
        "ca-key.pem",
        "-out",
        "ca.pem",
        "-config",
        "ca.cnf",
    ]);
    run_openssl(&args.openssl, temporary.path(), &command)?;
    eprintln!("o-node: generated development CA certificate");

    eprintln!(
        "o-node: generating development node key ({})",
        pki_key_algorithm.cli_name()
    );
    let mut command = vec!["req", "-new", "-newkey"];
    command.extend_from_slice(pki_key_algorithm.openssl_new_key_args());
    command.extend_from_slice(&[
        "-sha256",
        "-nodes",
        "-keyout",
        "node-key.pem",
        "-out",
        "node.csr",
        "-config",
        "node.cnf",
    ]);
    run_openssl(&args.openssl, temporary.path(), &command)?;
    eprintln!("o-node: generated development node CSR");
    run_openssl(
        &args.openssl,
        temporary.path(),
        &[
            "x509",
            "-req",
            "-in",
            "node.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca-key.pem",
            "-CAcreateserial",
            "-out",
            "node-cert.pem",
            "-days",
            "825",
            "-sha256",
            "-extfile",
            "node.cnf",
            "-extensions",
            "node_ext",
        ],
    )?;
    eprintln!("o-node: generated development node certificate");
    eprintln!(
        "o-node: generating development client key ({})",
        pki_key_algorithm.cli_name()
    );
    let mut command = vec!["req", "-new", "-newkey"];
    command.extend_from_slice(pki_key_algorithm.openssl_new_key_args());
    command.extend_from_slice(&[
        "-sha256",
        "-nodes",
        "-keyout",
        "client-key.pem",
        "-out",
        "client.csr",
        "-config",
        "client.cnf",
    ]);
    run_openssl(&args.openssl, temporary.path(), &command)?;
    eprintln!("o-node: generated development client CSR");
    run_openssl(
        &args.openssl,
        temporary.path(),
        &[
            "x509",
            "-req",
            "-in",
            "client.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca-key.pem",
            "-CAserial",
            "ca.srl",
            "-out",
            "client-cert.pem",
            "-days",
            "825",
            "-sha256",
            "-extfile",
            "client.cnf",
            "-extensions",
            "client_ext",
        ],
    )?;
    eprintln!("o-node: generated development client certificate");

    secure_key(temporary.path().join("ca-key.pem"))?;
    secure_key(temporary.path().join("node-key.pem"))?;
    secure_key(temporary.path().join("client-key.pem"))?;
    eprintln!("o-node: verifying generated development PKI over loopback mTLS");
    verify_generated_pki(temporary.path(), &args.server_name)?;
    eprintln!("o-node: generated development PKI passed loopback mTLS verification");

    let mut installed = Vec::new();
    for name in installed_names {
        let target = destination.join(name);
        // The staging directory is a child of the destination, so hard-linking
        // is same-filesystem and atomically refuses a raced-in target instead
        // of replacing it.
        if let Err(error) = fs::hard_link(temporary.path().join(name), &target) {
            for created in &installed {
                let _ = fs::remove_file(created);
            }
            return Err(error).with_context(|| {
                format!(
                    "failed to install generated PKI file `{}`",
                    target.display()
                )
            });
        }
        installed.push(target);
    }

    println!(
        "development PKI key algorithm: {}",
        pki_key_algorithm.cli_name()
    );
    println!("development PKI initialized at {}", destination.display());
    println!("server name: {}", args.server_name);
    println!(
        "node certificate: {}",
        destination.join("node-cert.pem").display()
    );
    println!(
        "client certificate: {}",
        destination.join("client-cert.pem").display()
    );
    println!(
        "CA private key: {} (move offline before non-development use)",
        destination.join("ca-key.pem").display()
    );
    Ok(())
}

fn init_v2_identity(args: IdentityInitArgs) -> Result<()> {
    let state_dir = args.state_dir.unwrap_or_else(default_hosted_v2_state_dir);
    let key_path = args
        .node_signing_key
        .unwrap_or_else(|| state_dir.join("node-signing-key.v2"));
    let public_path = args
        .node_public_key
        .unwrap_or_else(|| state_dir.join("node-signing-public.v2"));
    if key_path.exists() || public_path.exists() {
        bail!(
            "refusing to overwrite hosted V2 identity files: key={} public={}",
            key_path.display(),
            public_path.display()
        );
    }
    let signer = HostedNodeSignerV2::generate()?;
    // Opening the store establishes and verifies the 0700 capability root.
    let _store = DurableSessionStoreV2::open(&state_dir, signer.clone())?;
    write_new_node_signing_key_v2(&key_path, &signer)?;
    if let Err(error) = write_new_node_public_key_v2(&public_path, &signer.public_key()) {
        let _ = fs::remove_file(&key_path);
        return Err(error).context("failed to install hosted V2 public identity");
    }
    println!("hosted V2 state initialized at {}", state_dir.display());
    println!("node signing key: {}", key_path.display());
    println!("node public-key file: {}", public_path.display());
    println!("node public key: {}", signer.public_key_hex());
    println!("node key id: {}", signer.key_id());
    Ok(())
}

fn doctor(mut args: DoctorArgs) -> Result<()> {
    if !args.manual {
        let material = ensure_lan_open_material()?;
        args.runtime.node_id.get_or_insert(material.node_id);
        args.tls
            .cert
            .get_or_insert_with(|| material.pki_dir.join("node-cert.pem"));
        args.tls
            .key
            .get_or_insert_with(|| material.pki_dir.join("node-key.pem"));
        args.tls.client_ca.get_or_insert(material.pairing_ca);
    }
    let (shim_dir, _shim_guard) = resolve_shim_dir(args.runtime.shim_dir.clone())?;
    let runtime = runtime_from_args(args.runtime, shim_dir)?;
    let mut doctor = runtime.doctor()?;
    let identity = tls_identity(args.tls);
    let tls_check = match build_server_config(&identity) {
        Ok(_) => NodeDoctorCheckV1 {
            name: "tls-1.3-mutual-auth-config".to_string(),
            ok: true,
            detail: format!(
                "server-cert={} client-ca={} (private key loaded but not printed)",
                identity.cert_path.display(),
                identity.client_ca_path.display()
            ),
        },
        Err(error) => NodeDoctorCheckV1 {
            name: "tls-1.3-mutual-auth-config".to_string(),
            ok: false,
            detail: format!("{error:#}"),
        },
    };
    doctor.checks.push(tls_check);
    doctor.ready = doctor.checks.iter().all(|check| check.ok);
    println!("{}", serde_json::to_string_pretty(&doctor)?);
    if !doctor.ready {
        bail!("o-node doctor found one or more failed checks");
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct FabricServePathsV1 {
    state_base: PathBuf,
    node_signing_key: PathBuf,
    authority_public_keys: Vec<PathBuf>,
    node_generation: GenerationV1,
}

fn resolve_fabric_serve_paths(
    state_base: Option<PathBuf>,
    node_signing_key: Option<PathBuf>,
    authority_public_keys: Vec<PathBuf>,
    node_generation: u64,
) -> Result<Option<FabricServePathsV1>> {
    let Some(state_base) = state_base else {
        if node_signing_key.is_some() || !authority_public_keys.is_empty() {
            bail!("--fabric-state-dir is required when any Fabric key option is supplied");
        }
        return Ok(None);
    };
    let node_signing_key = node_signing_key.context(
        "--fabric-node-signing-key is required when --fabric-state-dir enables Fabric V1",
    )?;
    if authority_public_keys.is_empty() {
        bail!(
            "at least one --fabric-authority-public-key is required when --fabric-state-dir enables Fabric V1"
        );
    }
    Ok(Some(FabricServePathsV1 {
        state_base,
        node_signing_key,
        authority_public_keys,
        node_generation: GenerationV1::new(node_generation)?,
    }))
}

fn validate_shared_node_generation(
    fabric: Option<&FabricServePathsV1>,
    v2_enabled: bool,
    v2_node_generation: u64,
) -> Result<()> {
    if let Some(fabric) = fabric {
        if v2_enabled && fabric.node_generation.get() != v2_node_generation {
            bail!(
                "--fabric-node-generation ({}) must equal --v2-node-generation ({v2_node_generation}) when both protocols identify the same o-node deployment",
                fabric.node_generation.get()
            );
        }
    }
    Ok(())
}

fn open_fabric_provider(
    node_id: &str,
    paths: FabricServePathsV1,
) -> Result<Arc<FabricAttemptProviderV1>> {
    let node_signer =
        read_fabric_node_signing_key_v1(&paths.node_signing_key).with_context(|| {
            format!(
                "failed to read Fabric node signing key `{}`",
                paths.node_signing_key.display()
            )
        })?;
    let mut trusted_authorities = TrustedFabricAuthoritiesV1::new();
    for path in &paths.authority_public_keys {
        let public_key = read_fabric_public_key_v1(path).with_context(|| {
            format!(
                "failed to read Fabric authority public key `{}`",
                path.display()
            )
        })?;
        trusted_authorities.enroll(public_key);
    }
    let state_base = paths.state_base;
    let provider = FabricAttemptProviderV1::open(FabricAttemptProviderConfigV1 {
        state_base: state_base.clone(),
        node_id: node_id.to_owned(),
        node_generation: paths.node_generation,
        node_signer,
        trusted_authorities,
    })
    .with_context(|| {
        format!(
            "failed to open execution Fabric V1 provider beneath `{}`",
            state_base.display()
        )
    })?;
    Ok(Arc::new(provider))
}

fn service_summary_with_fabric(base: &str, provider: Option<&FabricAttemptProviderV1>) -> String {
    match provider {
        Some(provider) => format!(
            "{base}; execution Fabric V1 enabled; Fabric node key id {}; execution-cell incarnation {}",
            provider.node_key_id(),
            provider.execution_cell_incarnation().get()
        ),
        None => base.to_owned(),
    }
}

fn serve(mut args: ServeArgs) -> Result<()> {
    if let Some(token) = args.managed_start_token.as_deref() {
        validate_detached_launch_token(token)?;
    }
    let managed_start_token = args.managed_start_token.take();
    let fabric_paths = resolve_fabric_serve_paths(
        args.fabric_state_dir.take(),
        args.fabric_node_signing_key.take(),
        std::mem::take(&mut args.fabric_authority_public_keys),
        args.fabric_node_generation,
    )?;
    let automatic_mode = !args.manual;
    let legacy_lan_open = automatic_mode && args.lan_open;
    let automatic = if automatic_mode {
        let material = ensure_lan_open_material()?;
        args.runtime.node_id.get_or_insert(material.node_id.clone());
        args.tls
            .cert
            .get_or_insert_with(|| material.pki_dir.join("node-cert.pem"));
        args.tls
            .key
            .get_or_insert_with(|| material.pki_dir.join("node-key.pem"));
        args.tls.client_ca.get_or_insert_with(|| {
            if legacy_lan_open {
                material.client_ca_bundle.clone()
            } else {
                material.pairing_ca.clone()
            }
        });
        args.bind
            .get_or_insert_with(|| format!("0.0.0.0:{DEFAULT_LAN_NODE_PORT}"));
        args.v2_state_dir
            .get_or_insert_with(|| material.state_dir.clone());
        args.v2_node_signing_key
            .get_or_insert_with(|| material.node_signing_key.clone());
        Some(material)
    } else {
        args.runtime
            .node_id
            .get_or_insert_with(|| DEFAULT_NODE_ID.to_owned());
        args.bind
            .get_or_insert_with(|| DEFAULT_NODE_BIND.to_owned());
        None
    };

    let mesh_state_dir = resolve_mesh_state_dir(
        automatic_mode,
        args.no_mesh,
        args.v2_state_dir.as_deref(),
        args.mesh_state_dir.take(),
    )?;

    let bind_address = args
        .bind
        .take()
        .expect("serve bind is resolved for automatic and manual modes");
    let service_port = parse_bind_port(&bind_address)?;
    let (shim_dir, _shim_guard) = resolve_shim_dir(args.runtime.shim_dir.clone())?;
    let v1_runtime = runtime_from_args(args.runtime, shim_dir)?;
    validate_shared_node_generation(
        fabric_paths.as_ref(),
        args.v2_state_dir.is_some(),
        args.v2_node_generation,
    )?;
    let fabric_provider = fabric_paths
        .map(|paths| open_fabric_provider(&v1_runtime.node_id, paths))
        .transpose()?;

    if automatic.is_some() {
        if legacy_lan_open {
            eprintln!(
                "{}",
                concat!(
                    "o-node: legacy LAN-open mode enabled explicitly -- LAN reachability is ",
                    "treated as permission to download a shared private key and execute"
                )
            );
        } else {
            eprintln!(
                "o-node: paired mode enabled -- run `o node pair` on one machine, then enter its one-use passcode on the other"
            );
        }
    }
    let lan_services = LanServicesAfterBind {
        material: automatic,
        service_port,
        supports_v2: args.v2_state_dir.is_some(),
        legacy_lan_open,
        no_discovery: args.no_discovery,
        no_bootstrap: args.no_bootstrap,
    };

    if let Some(state_dir) = args.v2_state_dir {
        let shutdown = HostedDualNodeShutdown::new();
        // Register termination handling before opening the durable root. A
        // signal delivered during startup therefore cannot strand a newly
        // acquired state lock behind the old implicit-Drop lifecycle.
        #[cfg(unix)]
        let _termination_signals = NodeTerminationSignalGuard::install(shutdown.clone())?;
        let node_key_path = args
            .v2_node_signing_key
            .unwrap_or_else(|| state_dir.join("node-signing-key.v2"));
        let signer = read_node_signing_key_v2(&node_key_path)?;
        let authorizer: Arc<dyn PlacementProofAuthorizerV2> = if automatic_mode {
            if args.v2_authority_public_key.is_some() {
                eprintln!(
                    "o-node: --v2-authority-public-key is ignored in automatic LAN mode; use --manual to pin it"
                );
            }
            Arc::new(LanOpenPlacementAuthorizerV2)
        } else {
            let authority_path = args.v2_authority_public_key.context(
                "--v2-authority-public-key is required when --v2-state-dir enables V2 in manual mode",
            )?;
            let authority = read_placement_public_key_v2(&authority_path)?;
            Arc::new(PinnedEd25519PlacementAuthorizerV2::new(authority))
        };
        let node_state_epoch = GenerationV1::new(args.v2_node_generation)?;
        let state_quota_generation = GenerationV1::new(args.v2_state_quota_generation)?;
        let state_quotas = StateQuotaLimitsV2::new(
            args.v2_max_open_sessions,
            args.v2_max_actors_per_session,
            args.v2_max_snapshot_bytes_per_actor,
            args.v2_max_state_bytes_per_session,
            args.v2_max_state_bytes_total,
        )?;
        let store = DurableSessionStoreV2::open(&state_dir, signer)?;
        let v2_runtime = HostedV2RuntimeOwner::open(
            HostedV2RuntimeConfig {
                node_id: v1_runtime.node_id.clone(),
                node_generation: node_state_epoch,
                shim_dir: v1_runtime.shim_dir.clone(),
                runtime_executable: v1_runtime.runtime_executable.clone(),
                state_quota_generation,
                state_quotas,
            },
            store,
            authorizer,
        )
        .with_context(|| {
            format!(
                "failed to open durable V2 state at `{}` with stable node-state/deployment epoch {}; reuse the epoch across normal restarts, and use a new state root or archive the old root before bumping it; epoch changes never evict existing sessions",
                state_dir.display(),
                node_state_epoch.get()
            )
        })?;
        let mesh_runtime = mesh_state_dir
            .as_ref()
            .map(|mesh_state_dir| {
                let mut config =
                    MeshNodeRuntimeConfig::new(v1_runtime.node_id.clone(), mesh_state_dir.clone());
                config.max_concurrent_actors = u32::try_from(v1_runtime.max_concurrent_connections)
                    .context(
                        "node max-connections exceeds the mesh actor-capacity representation",
                    )?;
                MeshNodeRuntime::open(config).with_context(|| {
                    format!(
                        "failed to open durable scheduler/actor mesh state at `{}`",
                        mesh_state_dir.display()
                    )
                })
            })
            .transpose()?;
        eprintln!(
            "o-node: durable V2 node-state/deployment epoch {} is stable across normal restarts; bump only with a new state root or after archiving `{}`; changing the epoch never evicts or migrates existing sessions",
            node_state_epoch.get(),
            state_dir.display()
        );
        let unreadable_sessions = v2_runtime.handle().unreadable_sessions()?;
        if !unreadable_sessions.is_empty() {
            eprintln!(
                "o-node: retained {} unreadable durable V2 session(s); no session was evicted. A node-state epoch change requires a new state root or an archived old root",
                unreadable_sessions.len()
            );
            for diagnostic in unreadable_sessions {
                eprintln!("o-node: retained unreadable V2 session: {diagnostic}");
            }
        }
        if let Some(mesh_state_dir) = mesh_state_dir.as_ref() {
            eprintln!(
                "o-node: scheduler/actor mesh enabled at `{}` with capacity for {} concurrent actors",
                mesh_state_dir.display(),
                v1_runtime.max_concurrent_connections
            );
        }
        let ready_node_id = v1_runtime.node_id.clone();
        let ready_maximum_connections = v1_runtime.max_concurrent_connections;
        let base_service_summary = if mesh_runtime.is_some() {
            "TLS 1.3 mTLS; frozen V1 + durable V2; scheduler/actor mesh enabled"
        } else {
            "TLS 1.3 mTLS; frozen V1 + durable V2; scheduler/actor mesh disabled"
        };
        let ready_service_summary =
            service_summary_with_fabric(base_service_summary, fabric_provider.as_deref());
        let listener_ready_shutdown = shutdown.clone();
        let hosted = HostedOwnedDualNodeServerConfig {
            bind_address,
            v1_runtime,
            v2_runtime,
            mesh_runtime,
            tls_identity: tls_identity(args.tls),
        };
        if let Some(fabric_provider) = fabric_provider {
            return serve_owned_node_dual_with_execution_fabric_v1_until_shutdown_with_listener_ready(
                HostedOwnedDualNodeWithFabricServerConfigV1 {
                    hosted,
                    fabric_provider,
                },
                shutdown,
                move |listening_address| {
                    report_listener_ready(
                        listening_address,
                        &ready_node_id,
                        ready_maximum_connections,
                        &ready_service_summary,
                        lan_services,
                        Some(&listener_ready_shutdown),
                        managed_start_token.as_deref(),
                    )
                },
            );
        }
        return serve_owned_node_dual_until_shutdown_with_listener_ready(
            hosted,
            shutdown,
            move |listening_address| {
                report_listener_ready(
                    listening_address,
                    &ready_node_id,
                    ready_maximum_connections,
                    &ready_service_summary,
                    lan_services,
                    Some(&listener_ready_shutdown),
                    managed_start_token.as_deref(),
                )
            },
        );
    }
    if args.v2_node_signing_key.is_some() || args.v2_authority_public_key.is_some() {
        bail!("--v2-state-dir is required when any V2 key option is supplied");
    }
    let config = HostedNodeServerConfig {
        bind_address,
        runtime: v1_runtime,
        tls_identity: tls_identity(args.tls),
    };
    let ready_node_id = config.runtime.node_id.clone();
    let ready_maximum_connections = config.runtime.max_concurrent_connections;
    let ready_service_summary = service_summary_with_fabric(
        "TLS 1.3 mTLS; frozen V1; scheduler/actor mesh disabled",
        fabric_provider.as_deref(),
    );
    if let Some(fabric_provider) = fabric_provider {
        return serve_node_with_execution_fabric_v1_and_listener_ready(
            HostedNodeWithFabricServerConfigV1 {
                hosted: config,
                fabric_provider,
            },
            move |listening_address| {
                report_listener_ready(
                    listening_address,
                    &ready_node_id,
                    ready_maximum_connections,
                    &ready_service_summary,
                    lan_services,
                    None,
                    managed_start_token.as_deref(),
                )
            },
        );
    }
    serve_node_with_listener_ready(config, move |listening_address| {
        report_listener_ready(
            listening_address,
            &ready_node_id,
            ready_maximum_connections,
            &ready_service_summary,
            lan_services,
            None,
            managed_start_token.as_deref(),
        )
    })
}

fn resolve_mesh_state_dir(
    automatic_mode: bool,
    no_mesh: bool,
    v2_state_dir: Option<&Path>,
    explicit_mesh_state_dir: Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    if no_mesh {
        if explicit_mesh_state_dir.is_some() {
            bail!("--no-mesh conflicts with --mesh-state-dir");
        }
        return Ok(None);
    }
    if let Some(mesh_state_dir) = explicit_mesh_state_dir {
        if v2_state_dir.is_none() {
            bail!("--mesh-state-dir requires --v2-state-dir in manual mode");
        }
        return Ok(Some(mesh_state_dir));
    }
    if automatic_mode {
        let v2_state_dir = v2_state_dir
            .context("automatic scheduler/actor mesh requires a durable V2 state root")?;
        return Ok(Some(v2_state_dir.join("mesh-v1")));
    }
    Ok(None)
}

fn parse_bind_port(bind: &str) -> Result<u16> {
    bind.rsplit_once(':')
        .context("node bind address must include a port")?
        .1
        .parse::<u16>()
        .with_context(|| format!("node bind address `{bind}` has an invalid port"))
}

#[cfg(unix)]
struct NodeTerminationSignalGuard {
    handle: signal_hook::iterator::Handle,
    worker: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl NodeTerminationSignalGuard {
    fn install(shutdown: HostedDualNodeShutdown) -> Result<Self> {
        use signal_hook::consts::{SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([SIGINT, SIGTERM])
            .context("failed to register SIGINT/SIGTERM handling for durable V2 shutdown")?;
        let handle = signals.handle();
        let worker = thread::Builder::new()
            .name("ostadix-node-signals".to_owned())
            .spawn(move || {
                let mut received_first = false;
                for signal in &mut signals {
                    match observe_node_termination_signal(
                        &shutdown,
                        &mut received_first,
                        signal,
                    ) {
                        NodeTerminationSignalAction::Drain => {
                            eprintln!(
                                "o-node: received signal {signal}; stopping admission and draining accepted work (send another termination signal to force exit)"
                            );
                        }
                        NodeTerminationSignalAction::Force(signal) => {
                            eprintln!(
                                "o-node: received a second termination signal {signal}; forcing process termination"
                            );
                            if let Err(error) =
                                signal_hook::low_level::emulate_default_handler(signal)
                            {
                                eprintln!(
                                    "o-node: failed to restore the default signal handler: {error}; aborting"
                                );
                            }
                            std::process::abort();
                        }
                    }
                }
            })
            .context("failed to spawn o-node termination-signal worker")?;
        Ok(Self {
            handle,
            worker: Some(worker),
        })
    }
}

#[cfg(unix)]
impl Drop for NodeTerminationSignalGuard {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(worker) = self.worker.take() {
            if let Err(payload) = worker.join() {
                let detail = payload
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".to_owned());
                eprintln!("o-node: termination-signal worker panicked: {detail}");
            }
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeTerminationSignalAction {
    Drain,
    Force(i32),
}

#[cfg(unix)]
fn observe_node_termination_signal(
    shutdown: &HostedDualNodeShutdown,
    received_first: &mut bool,
    signal: i32,
) -> NodeTerminationSignalAction {
    if *received_first {
        NodeTerminationSignalAction::Force(signal)
    } else {
        *received_first = true;
        shutdown.request();
        NodeTerminationSignalAction::Drain
    }
}

fn runtime_from_args(args: RuntimeArgs, shim_dir: PathBuf) -> Result<HostedNodeRuntime> {
    let runtime_executable = resolve_runtime_binary(args.runtime_binary)?;
    Ok(HostedNodeRuntime {
        node_id: args.node_id.unwrap_or_else(|| DEFAULT_NODE_ID.to_owned()),
        shim_dir,
        runtime_executable,
        max_concurrent_connections: args.max_connections,
    })
}

fn resolve_runtime_binary(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return validate_native_runtime_binary(&path).with_context(|| {
            format!(
                "--runtime-binary `{}` is not a supported native evaluator image",
                path.display()
            )
        });
    }

    let current = env::current_exe().context("failed to locate o-node executable")?;
    let mut candidates = current
        .parent()
        .into_iter()
        .flat_map(|directory| [directory.join("ostadix-evaluator"), directory.join("O")])
        .collect::<Vec<_>>();
    if let Ok(installed) = which::which("ostadix-evaluator") {
        if !candidates.contains(&installed) {
            candidates.push(installed);
        }
    }
    let mut rejected = Vec::new();
    for candidate in candidates {
        match validate_native_runtime_binary(&candidate) {
            Ok(path) => return Ok(path),
            Err(error) => rejected.push(format!("{} ({error})", candidate.display())),
        }
    }
    bail!(
        "could not find a native evaluator image; run setup.sh, install `ostadix-evaluator` beside o-node, or pass --runtime-binary{}",
        if rejected.is_empty() {
            String::new()
        } else {
            format!("; rejected candidates: {}", rejected.join(", "))
        }
    )
}

fn tls_identity(args: ServerTlsArgs) -> ServerTlsIdentity {
    ServerTlsIdentity {
        client_ca_path: args.client_ca.unwrap_or_else(default_ca_path),
        cert_path: args.cert.unwrap_or_else(default_node_cert_path),
        key_path: args.key.unwrap_or_else(default_node_key_path),
    }
}

fn certificate_san(server_name: &str) -> Result<String> {
    if let Ok(address) = server_name.parse::<std::net::IpAddr>() {
        return Ok(format!("IP:{address}"));
    }
    if server_name.is_empty() || server_name.len() > 253 || !server_name.is_ascii() {
        bail!("PKI server name must be a non-empty ASCII DNS name up to 253 bytes");
    }
    for label in server_name.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            bail!("invalid DNS label `{label}` in PKI server name");
        }
    }
    Ok(format!("DNS:{server_name}"))
}

fn ensure_private_directory(path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "PKI destination `{}` must be a real directory, not a symlink or file",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(path).with_context(|| {
                format!("failed to create PKI destination `{}`", path.display())
            })?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect PKI destination `{}`", path.display()))
        }
    }
    Ok(())
}

fn create_private_temp_dir(parent: &std::path::Path) -> Result<TemporaryPkiDirectory> {
    for _ in 0..32 {
        let mut random = [0_u8; 12];
        getrandom::fill(&mut random).context("failed to obtain entropy for PKI staging")?;
        let path = parent.join(format!(".pki-init-{}", hex::encode(random)));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => return Ok(TemporaryPkiDirectory(path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create private PKI staging directory in `{}`",
                        parent.display()
                    )
                })
            }
        }
    }
    bail!("failed to allocate a unique PKI staging directory after 32 attempts")
}

fn run_openssl(
    executable: &std::path::Path,
    directory: &std::path::Path,
    args: &[&str],
) -> Result<()> {
    let output = ProcessCommand::new(executable)
        .current_dir(directory)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch OpenSSL `{}`", executable.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = if stderr.chars().count() > 4096 {
        format!(
            "{} [truncated]",
            stderr.chars().take(4096).collect::<String>()
        )
    } else {
        stderr.into_owned()
    };
    bail!(
        "OpenSSL subcommand `{}` failed with {}: {}",
        args.first().copied().unwrap_or("<none>"),
        output.status,
        detail.trim()
    )
}

fn run_openssl_capture(executable: &Path, directory: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = ProcessCommand::new(executable)
        .current_dir(directory)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch OpenSSL `{}`", executable.display()))?;
    if output.status.success() {
        let mut bytes = output.stdout;
        while bytes.last().is_some_and(u8::is_ascii_whitespace) {
            bytes.pop();
        }
        return Ok(bytes);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.chars().take(4096).collect::<String>();
    bail!(
        "OpenSSL subcommand `{}` failed with {}: {}{}",
        args.first().copied().unwrap_or("<none>"),
        output.status,
        detail.trim(),
        if stderr.chars().count() > 4096 {
            " [truncated]"
        } else {
            ""
        }
    )
}

fn write_file_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .context("atomic file destination has no parent")?;
    ensure_private_directory(parent)?;
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).context("failed to generate atomic-file staging name")?;
    let temporary = parent.join(format!(
        ".{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("pairing"),
        hex::encode(random)
    ));
    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(if private { 0o600 } else { 0o644 });
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create `{}`", temporary.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
            .with_context(|| format!("failed to atomically install `{}`", path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn secure_key(path: PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set mode 0600 on `{}`", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn verify_generated_pki(directory: &std::path::Path, server_name: &str) -> Result<()> {
    verify_generated_pki_with_timeout(directory, server_name, GENERATED_PKI_VERIFY_TIMEOUT)
}

fn verify_generated_pki_with_timeout(
    directory: &std::path::Path,
    server_name: &str,
    timeout: Duration,
) -> Result<()> {
    if timeout.is_zero() {
        bail!("generated-PKI verification timeout must be nonzero");
    }
    let server_identity = ServerTlsIdentity {
        client_ca_path: directory.join("ca.pem"),
        cert_path: directory.join("node-cert.pem"),
        key_path: directory.join("node-key.pem"),
    };
    let client_identity = ClientTlsIdentity {
        ca_path: directory.join("ca.pem"),
        cert_path: directory.join("client-cert.pem"),
        key_path: directory.join("client-key.pem"),
        server_name: server_name.to_string(),
    };
    build_client_config(&client_identity).context("generated client TLS identity is invalid")?;
    let server_config = build_server_config(&server_identity)
        .context("generated server TLS identity is invalid")?;

    let listener = TcpListener::bind("127.0.0.1:0")
        .context("failed to bind loopback socket for generated-PKI verification")?;
    let address = listener.local_addr()?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("generated-PKI verification deadline overflowed")?;
    let server = thread::Builder::new()
        .name("ostadix-generated-pki-verifier".to_string())
        .spawn(move || -> Result<()> {
            let tcp = accept_generated_pki_tcp_until(&listener, deadline)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("generated-PKI verification deadline expired before server handshake");
            }
            let deadline_guard = GeneratedPkiDeadlineGuard::arm(&tcp, deadline)?;
            let handshake_result = accept_mutual_tls(tcp, server_config, remaining, remaining);
            if deadline_guard.expired() || Instant::now() >= deadline {
                bail!("generated-PKI verification deadline expired during server handshake");
            }
            let _stream = handshake_result?;
            Ok(())
        })
        .context("failed to create generated-PKI verification server thread")?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let client_result = if remaining.is_zero() {
        Err(anyhow::anyhow!(
            "generated-PKI verification deadline expired before client handshake"
        ))
    } else {
        connect_mutual_tls(&address.to_string(), &client_identity, remaining, remaining)
    };
    let server_result = server
        .join()
        .map_err(|_| anyhow::anyhow!("generated-PKI verification thread panicked"))?;
    match (client_result, server_result) {
        (Ok(_client), Ok(())) if Instant::now() < deadline => Ok(()),
        (Ok(_client), Ok(())) => {
            bail!("generated-PKI verification completed after its absolute deadline")
        }
        (Err(client_error), Ok(())) => {
            Err(client_error).context("generated PKI failed client-side loopback mTLS verification")
        }
        (Ok(_client), Err(server_error)) => {
            Err(server_error).context("generated PKI failed server-side loopback mTLS verification")
        }
        (Err(client_error), Err(server_error)) => {
            bail!(
                "generated PKI failed loopback mTLS verification on both sides: client: {client_error:#}; server: {server_error:#}"
            )
        }
    }
}

fn accept_generated_pki_tcp_until(listener: &TcpListener, deadline: Instant) -> Result<TcpStream> {
    listener
        .set_nonblocking(true)
        .context("failed to configure expiring generated-PKI verification listener")?;
    loop {
        match listener.accept() {
            Ok((tcp, _)) => {
                if Instant::now() >= deadline {
                    let _ = tcp.shutdown(std::net::Shutdown::Both);
                    bail!("generated-PKI verification deadline expired as client connected");
                }
                tcp.set_nonblocking(false)
                    .context("failed to restore blocking generated-PKI verification socket")?;
                return Ok(tcp);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!("generated-PKI verification timed out waiting for loopback client");
                }
                thread::sleep(GENERATED_PKI_ACCEPT_POLL_INTERVAL.min(remaining));
            }
            Err(error) => return Err(error).context("generated-PKI verification accept failed"),
        }
    }
}

struct GeneratedPkiDeadlineGuard {
    cancel: Option<std::sync::mpsc::SyncSender<()>>,
    watchdog: Option<thread::JoinHandle<()>>,
    expired: Arc<std::sync::atomic::AtomicBool>,
}

impl GeneratedPkiDeadlineGuard {
    fn arm(socket: &TcpStream, deadline: Instant) -> Result<Self> {
        let watched = socket
            .try_clone()
            .context("failed to clone generated-PKI socket for deadline enforcement")?;
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let expired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watchdog_expired = Arc::clone(&expired);
        let watchdog = thread::Builder::new()
            .name("ostadix-generated-pki-deadline".to_string())
            .spawn(move || {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if matches!(
                    cancelled.recv_timeout(remaining),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    watchdog_expired.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = watched.shutdown(std::net::Shutdown::Both);
                }
            })
            .context("failed to create generated-PKI deadline watchdog")?;
        Ok(Self {
            cancel: Some(cancel),
            watchdog: Some(watchdog),
            expired,
        })
    }

    fn expired(&self) -> bool {
        self.expired.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for GeneratedPkiDeadlineGuard {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.try_send(());
        }
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

struct TemporaryPkiDirectory(PathBuf);

impl TemporaryPkiDirectory {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TemporaryPkiDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn resolve_shim_dir(explicit: Option<PathBuf>) -> Result<(PathBuf, Option<ExtractedShims>)> {
    if let Some(path) = explicit {
        return Ok((path, None));
    }
    if let Some(path) = env::var_os("O_BACKENDS_DIR")
        .or_else(|| env::var_os("BACKENDS_DIR"))
        .filter(|path| !path.is_empty())
    {
        return Ok((PathBuf::from(path), None));
    }
    let extracted = o_lang::shims::extract_bundled_shims("o_node_shims")
        .context("failed to extract bundled backend shims")?;
    Ok((extracted.path().to_path_buf(), Some(extracted)))
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::CommandFactory;

    #[test]
    fn pairing_cli_requires_explicit_replacement_and_keeps_passcode_out_of_arguments() {
        let cli = Cli::try_parse_from([
            "o-node",
            "pair",
            "ostadix-peer",
            "--passcode-stdin",
            "--replace",
            "--address",
            "203.0.113.8:7340",
        ])
        .unwrap();
        let Command::Pair(args) = cli.command else {
            panic!("pair subcommand was not parsed");
        };
        assert_eq!(args.peer_node_id.as_deref(), Some("ostadix-peer"));
        assert!(args.passcode_stdin);
        assert!(args.replace);
        assert_eq!(args.address.as_deref(), Some("203.0.113.8:7340"));
    }

    #[test]
    fn v2_node_generation_help_defines_a_stable_state_epoch() {
        let mut command = Cli::command();
        let serve = command.find_subcommand_mut("serve").unwrap();
        let generation = serve
            .get_arguments()
            .find(|argument| argument.get_id() == "v2_node_generation")
            .unwrap();
        let help = generation.get_help().unwrap().to_string();
        assert_eq!(help, V2_NODE_EPOCH_HELP);
        assert!(help.contains("Reuse it across normal process restarts"));
        assert!(help.contains("new state root or archive the old root"));
        assert!(help.contains("never evicts or migrates existing sessions"));
    }

    #[test]
    fn fabric_cli_parses_explicit_state_key_authorities_and_generation() {
        let cli = Cli::try_parse_from([
            "o-node",
            "serve",
            "--manual",
            "--fabric-state-dir",
            "fabric-state",
            "--fabric-node-signing-key",
            "fabric-node.key",
            "--fabric-authority-public-key",
            "authority-a.pub",
            "--fabric-authority-public-key",
            "authority-b.pub",
            "--fabric-node-generation",
            "7",
        ])
        .unwrap();
        let Command::Serve(args) = cli.command else {
            panic!("serve subcommand was not parsed");
        };
        assert_eq!(args.fabric_state_dir, Some(PathBuf::from("fabric-state")));
        assert_eq!(
            args.fabric_node_signing_key,
            Some(PathBuf::from("fabric-node.key"))
        );
        assert_eq!(
            args.fabric_authority_public_keys,
            [
                PathBuf::from("authority-a.pub"),
                PathBuf::from("authority-b.pub")
            ]
        );
        assert_eq!(args.fabric_node_generation, 7);
    }

    #[test]
    fn fabric_cli_requires_a_complete_explicit_authority_configuration() {
        assert!(resolve_fabric_serve_paths(None, None, Vec::new(), 1)
            .unwrap()
            .is_none());

        let missing_state = resolve_fabric_serve_paths(
            None,
            Some(PathBuf::from("fabric-node.key")),
            vec![PathBuf::from("authority.pub")],
            1,
        )
        .unwrap_err();
        assert!(missing_state.to_string().contains("--fabric-state-dir"));

        let missing_node_key = resolve_fabric_serve_paths(
            Some(PathBuf::from("fabric-state")),
            None,
            vec![PathBuf::from("authority.pub")],
            1,
        )
        .unwrap_err();
        assert!(missing_node_key
            .to_string()
            .contains("--fabric-node-signing-key"));

        let missing_authority = resolve_fabric_serve_paths(
            Some(PathBuf::from("fabric-state")),
            Some(PathBuf::from("fabric-node.key")),
            Vec::new(),
            1,
        )
        .unwrap_err();
        assert!(missing_authority
            .to_string()
            .contains("--fabric-authority-public-key"));

        let resolved = resolve_fabric_serve_paths(
            Some(PathBuf::from("fabric-state")),
            Some(PathBuf::from("fabric-node.key")),
            vec![PathBuf::from("authority.pub")],
            7,
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.state_base, PathBuf::from("fabric-state"));
        assert_eq!(resolved.node_signing_key, PathBuf::from("fabric-node.key"));
        assert_eq!(
            resolved.authority_public_keys,
            [PathBuf::from("authority.pub")]
        );
        assert_eq!(resolved.node_generation.get(), 7);
    }

    #[test]
    fn fabric_and_v2_share_one_node_deployment_generation() {
        let fabric = resolve_fabric_serve_paths(
            Some(PathBuf::from("fabric-state")),
            Some(PathBuf::from("fabric-node.key")),
            vec![PathBuf::from("authority.pub")],
            7,
        )
        .unwrap()
        .unwrap();
        validate_shared_node_generation(Some(&fabric), false, 8).unwrap();
        validate_shared_node_generation(Some(&fabric), true, 7).unwrap();
        let error = validate_shared_node_generation(Some(&fabric), true, 8).unwrap_err();
        assert!(error.to_string().contains("same o-node deployment"));
    }

    #[test]
    fn fabric_node_generation_help_distinguishes_restart_incarnation() {
        let mut command = Cli::command();
        let serve = command.find_subcommand_mut("serve").unwrap();
        let generation = serve
            .get_arguments()
            .find(|argument| argument.get_id() == "fabric_node_generation")
            .unwrap();
        let help = generation.get_help().unwrap().to_string();
        assert_eq!(help, FABRIC_NODE_EPOCH_HELP);
        assert!(help.contains("Reuse it across normal process restarts"));
        assert!(help.contains("execution-cell incarnation advances separately"));
        assert!(help.contains("intentional deployment epoch"));
    }

    #[test]
    fn disabled_fabric_preserves_the_existing_readiness_summary() {
        let existing = "TLS 1.3 mTLS; frozen V1; scheduler/actor mesh disabled";
        assert_eq!(service_summary_with_fabric(existing, None), existing);
    }

    #[test]
    fn mesh_cli_rejects_an_explicit_state_root_with_no_mesh() {
        let error = Cli::try_parse_from([
            "o-node",
            "serve",
            "--mesh-state-dir",
            "mesh-state",
            "--no-mesh",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn mesh_policy_defaults_only_automatic_mode_under_the_v2_root() {
        let v2_state_dir = Path::new("durable-v2");
        assert_eq!(
            resolve_mesh_state_dir(true, false, Some(v2_state_dir), None).unwrap(),
            Some(v2_state_dir.join("mesh-v1"))
        );
        assert_eq!(
            resolve_mesh_state_dir(false, false, Some(v2_state_dir), None).unwrap(),
            None
        );
        assert_eq!(
            resolve_mesh_state_dir(true, true, Some(v2_state_dir), None).unwrap(),
            None
        );
    }

    #[test]
    fn manual_mesh_is_explicit_and_requires_durable_v2() {
        let v2_state_dir = Path::new("durable-v2");
        let mesh_state_dir = PathBuf::from("chosen-mesh");
        assert_eq!(
            resolve_mesh_state_dir(
                false,
                false,
                Some(v2_state_dir),
                Some(mesh_state_dir.clone())
            )
            .unwrap(),
            Some(mesh_state_dir.clone())
        );
        let error = resolve_mesh_state_dir(false, false, None, Some(mesh_state_dir)).unwrap_err();
        assert!(error
            .to_string()
            .contains("--mesh-state-dir requires --v2-state-dir in manual mode"));
    }

    #[test]
    fn listener_readiness_is_not_published_after_shutdown_request() {
        let shutdown = HostedDualNodeShutdown::new();
        assert!(shutdown.request());
        let error = report_listener_ready(
            "127.0.0.1:7337".parse().unwrap(),
            "shutdown-test",
            1,
            "test service",
            LanServicesAfterBind {
                material: None,
                service_port: 7337,
                supports_v2: true,
                legacy_lan_open: false,
                no_discovery: true,
                no_bootstrap: true,
            },
            Some(&shutdown),
            Some("0123456789abcdef0123456789abcdef"),
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "o-node shutdown was requested before listener readiness publication"
        );
    }

    #[cfg(unix)]
    #[test]
    fn detached_pid_guard_only_matches_this_binary_in_serve_mode() {
        let expected = Path::new("/tmp/o-node");
        assert!(detached_command_matches(
            &["/tmp/o-node".to_owned(), "serve".to_owned()],
            expected,
            None,
        ));
        assert!(!detached_command_matches(
            &["/tmp/o-node".to_owned(), "profile".to_owned()],
            expected,
            None,
        ));
        assert!(!detached_command_matches(
            &["/usr/bin/sleep".to_owned(), "serve".to_owned()],
            expected,
            None,
        ));
        assert!(!detached_command_matches(
            &["/opt/o-node".to_owned(), "serve".to_owned()],
            expected,
            None,
        ));
        assert!(detached_command_matches(
            &["o-node".to_owned(), "serve".to_owned()],
            expected,
            None,
        ));
        let token = "0123456789abcdef0123456789abcdef";
        let expected_with_spaces = Path::new("/tmp/path with spaces/o-node");
        assert!(detached_command_matches(
            &[
                "/tmp/path with spaces/o-node".to_owned(),
                "serve".to_owned(),
                "--managed-start-token".to_owned(),
                token.to_owned(),
            ],
            expected_with_spaces,
            Some(token),
        ));
        assert!(!detached_command_matches(
            &[
                "/tmp/path with spaces/not-o-node".to_owned(),
                "serve".to_owned(),
                "--managed-start-token".to_owned(),
                token.to_owned(),
            ],
            expected_with_spaces,
            Some(token),
        ));
        assert!(!detached_command_matches(
            &[
                "/tmp/path with spaces/o-node".to_owned(),
                "serve".to_owned(),
                "--managed-start-token".to_owned(),
                "ffffffffffffffffffffffffffffffff".to_owned(),
            ],
            expected_with_spaces,
            Some(token),
        ));

        let ps_arguments = detached_ps_command_arguments(
            "o-node",
            expected_with_spaces,
            &format!("/tmp/path with spaces/o-node serve --managed-start-token {token}"),
        )
        .unwrap();
        assert!(detached_command_matches(
            &ps_arguments,
            expected_with_spaces,
            Some(token),
        ));
        assert!(detached_ps_command_arguments(
            "o-node",
            expected_with_spaces,
            "/tmp/different path/o-node serve"
        )
        .is_none());
        assert!(detached_ps_command_arguments(
            "not-o-node",
            expected_with_spaces,
            "/tmp/path with spaces/o-node serve"
        )
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn detached_startup_log_scope_ignores_prior_attempts() {
        let launch_token = "0123456789abcdef0123456789abcdef";
        let prior_launch_token = "ffffffffffffffffffffffffffffffff";
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("o-node.log");
        fs::write(
            &path,
            format!(
                "{DETACHED_LISTENER_READY_LOG_PREFIX}{prior_launch_token} address=127.0.0.1:7337\nold run\n"
            ),
        )
        .unwrap();
        let start_offset = fs::metadata(&path).unwrap().len();
        let mut observer =
            DetachedStartupLogObserver::open(&path, start_offset, launch_token).unwrap();
        let mut log = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(log, "failed to bind current run").unwrap();
        writeln!(
            log,
            "{DETACHED_LISTENER_READY_LOG_PREFIX}{prior_launch_token} address=127.0.0.1:7337"
        )
        .unwrap();
        log.flush().unwrap();

        assert!(!observer.poll_listener_ready().unwrap());
        let excerpt = read_startup_log_excerpt(&path, start_offset, 1024)
            .unwrap()
            .unwrap();
        assert!(excerpt.starts_with("failed to bind current run\n"));
        assert!(excerpt.contains(prior_launch_token));
        assert!(!excerpt.contains("old run"));

        let ready_line_prefix =
            format!("{DETACHED_LISTENER_READY_LOG_PREFIX}{launch_token} address=");
        let prefix = ready_line_prefix.as_bytes();
        let split = prefix.len() / 2;
        log.write_all(&prefix[..split]).unwrap();
        log.flush().unwrap();
        assert!(!observer.poll_listener_ready().unwrap());
        log.write_all(&prefix[split..]).unwrap();
        log.flush().unwrap();
        assert!(!observer.poll_listener_ready().unwrap());
        writeln!(log, "127.0.0.1:7337").unwrap();
        log.flush().unwrap();
        assert!(observer.poll_listener_ready().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn detached_startup_observer_does_not_lose_an_early_marker() {
        let launch_token = "0123456789abcdef0123456789abcdef";
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("o-node.log");
        fs::write(&path, "attempt starts here\n").unwrap();
        let start_offset = fs::metadata(&path).unwrap().len();
        let mut observer =
            DetachedStartupLogObserver::open(&path, start_offset, launch_token).unwrap();
        let mut log = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            log,
            "{DETACHED_LISTENER_READY_LOG_PREFIX}{launch_token} address=127.0.0.1:7337"
        )
        .unwrap();
        log.write_all(&vec![b'x'; DETACHED_STARTUP_LOG_EXCERPT_BYTES * 2])
            .unwrap();
        log.flush().unwrap();

        assert!(observer.poll_listener_ready().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn detached_startup_observer_bounds_each_poll() {
        let launch_token = "0123456789abcdef0123456789abcdef";
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("o-node.log");
        let mut contents = vec![b'x'; DETACHED_STARTUP_LOG_POLL_BYTES];
        contents.push(b'\n');
        contents.extend_from_slice(
            format!("{DETACHED_LISTENER_READY_LOG_PREFIX}{launch_token} address=127.0.0.1:7337\n")
                .as_bytes(),
        );
        fs::write(&path, contents).unwrap();
        let mut observer = DetachedStartupLogObserver::open(&path, 0, launch_token).unwrap();

        assert!(!observer.poll_listener_ready().unwrap());
        assert_eq!(
            observer.file.stream_position().unwrap(),
            DETACHED_STARTUP_LOG_POLL_BYTES as u64
        );
        assert!(observer.poll_listener_ready().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn detached_startup_log_excerpt_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("o-node.log");
        fs::write(&path, "0123456789abcdef").unwrap();
        let excerpt = read_startup_log_excerpt(&path, 0, 8).unwrap().unwrap();
        assert_eq!(excerpt, "[... startup log truncated ...]\n89abcdef");
    }

    #[cfg(unix)]
    #[test]
    fn detached_pid_file_supports_legacy_and_token_bound_records() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("o-node.pid");
        fs::write(&path, "123\n").unwrap();
        assert_eq!(
            read_detached_pid(&path).unwrap(),
            Some(DetachedProcessIdentity {
                pid: 123,
                launch_token: None,
            })
        );

        let token = "0123456789abcdef0123456789abcdef";
        fs::write(&path, format!("456\n{token}\n")).unwrap();
        assert_eq!(
            read_detached_pid(&path).unwrap(),
            Some(DetachedProcessIdentity {
                pid: 456,
                launch_token: Some(token.to_owned()),
            })
        );
        fs::write(&path, "456\nnot-a-token\n").unwrap();
        assert!(read_detached_pid(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn detached_lifecycle_lock_serializes_pid_transitions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("o-node.lifecycle.lock");
        let _first = acquire_detached_lifecycle_lock(&path).unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let result = unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(result, -1);
        let lock_error = io::Error::last_os_error().raw_os_error();
        assert!(
            lock_error.is_some_and(|code| { code == libc::EWOULDBLOCK || code == libc::EAGAIN })
        );
    }

    #[cfg(unix)]
    #[test]
    fn detached_startup_guard_reaps_child_and_only_removes_its_pid() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("o-node.pid");
        let child = ProcessCommand::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        fs::write(&pid_path, format!("{pid}\n")).unwrap();
        {
            let _guard = DetachedStartupChildGuard::new(child, pid_path.clone());
        }
        assert!(!process_is_alive(pid as i32));
        assert!(!pid_path.exists());

        fs::write(&pid_path, "999999\n").unwrap();
        remove_pid_file_if_matches(&pid_path, pid).unwrap();
        assert_eq!(fs::read_to_string(&pid_path).unwrap(), "999999\n");
    }

    #[cfg(unix)]
    #[test]
    fn detached_startup_interrupt_is_observed_before_success() {
        let launch_token = "0123456789abcdef0123456789abcdef";
        let root = tempfile::tempdir().unwrap();
        let log_path = root.path().join("o-node.log");
        let pid_path = root.path().join("o-node.pid");
        fs::write(
            &log_path,
            format!("{DETACHED_LISTENER_READY_LOG_PREFIX}{launch_token} address=127.0.0.1:7337\n"),
        )
        .unwrap();
        let mut observer = DetachedStartupLogObserver::open(&log_path, 0, launch_token).unwrap();
        let child = ProcessCommand::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        fs::write(&pid_path, format!("{pid}\n")).unwrap();
        let mut child = DetachedStartupChildGuard::new(child, pid_path.clone());
        let interrupted = AtomicBool::new(true);

        let error = wait_for_detached_node_startup(
            child.child_mut(),
            &mut observer,
            &log_path,
            0,
            Duration::from_secs(30),
            &interrupted,
        )
        .unwrap_err();
        assert!(error.to_string().contains("startup interrupted"));
        child.cleanup().unwrap();
        assert!(!process_is_alive(pid as i32));
        assert!(!pid_path.exists());
    }

    #[test]
    fn development_pki_server_name_is_config_injection_safe() {
        assert_eq!(certificate_san("localhost").unwrap(), "DNS:localhost");
        assert_eq!(certificate_san("127.0.0.1").unwrap(), "IP:127.0.0.1");
        assert!(certificate_san("bad\n[evil]").is_err());
        assert!(certificate_san("*.example.test").is_err());
        assert!(certificate_san("-bad.example").is_err());
    }

    #[test]
    fn detached_start_pki_algorithm_is_explicit_and_defaults_to_rsa() {
        let Cli {
            command: Command::Start(defaults),
        } = Cli::try_parse_from(["o-node", "start"]).unwrap()
        else {
            panic!("start command did not parse")
        };
        assert_eq!(defaults.fresh_pki_key_algorithm, PkiKeyAlgorithm::Rsa3072);

        let Cli {
            command: Command::Start(explicit),
        } = Cli::try_parse_from(["o-node", "start", "--fresh-pki-key-algorithm", "ec-p256"])
            .unwrap()
        else {
            panic!("start command did not parse")
        };
        assert_eq!(explicit.fresh_pki_key_algorithm, PkiKeyAlgorithm::EcP256);
    }

    #[test]
    fn generated_pki_accept_expires_when_no_client_connects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let started = Instant::now();
        let deadline = started.checked_add(Duration::from_millis(100)).unwrap();
        let error = accept_generated_pki_tcp_until(&listener, deadline).unwrap_err();
        assert!(error
            .to_string()
            .contains("timed out waiting for loopback client"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "generated-PKI listener did not honor its deadline"
        );
    }

    #[test]
    fn generated_pki_deadline_interrupts_a_stalled_accepted_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let _client = TcpStream::connect(address).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let started = Instant::now();
        let deadline = started.checked_add(Duration::from_millis(100)).unwrap();
        let deadline_guard = GeneratedPkiDeadlineGuard::arm(&server, deadline).unwrap();
        let mut byte = [0_u8; 1];
        assert!(std::io::Read::read_exact(&mut server, &mut byte).is_err());
        assert!(deadline_guard.expired());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "generated-PKI watchdog did not interrupt the stalled socket"
        );
    }

    #[cfg(unix)]
    #[test]
    fn first_termination_signal_drains_and_second_forces() {
        let shutdown = HostedDualNodeShutdown::new();
        let mut received_first = false;
        assert_eq!(
            observe_node_termination_signal(&shutdown, &mut received_first, libc::SIGTERM),
            NodeTerminationSignalAction::Drain
        );
        assert!(shutdown.is_requested());
        assert_eq!(
            observe_node_termination_signal(&shutdown, &mut received_first, libc::SIGINT),
            NodeTerminationSignalAction::Force(libc::SIGINT)
        );
    }

    #[test]
    fn development_pki_is_verified_and_never_overwrites() {
        let Ok(openssl) = which::which("openssl") else {
            eprintln!("skipping development PKI integration test: openssl not found");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        for (directory_name, pki_key_algorithm) in [
            ("rsa-3072", PkiKeyAlgorithm::Rsa3072),
            ("ec-p256", PkiKeyAlgorithm::EcP256),
        ] {
            let destination = root.path().join(directory_name);
            init_development_pki(
                PkiInitArgs {
                    directory: Some(destination.clone()),
                    server_name: "localhost".to_string(),
                    openssl: openssl.clone(),
                },
                pki_key_algorithm,
            )
            .unwrap();
            for name in [
                "ca.pem",
                "ca-key.pem",
                "node-cert.pem",
                "node-key.pem",
                "client-cert.pem",
                "client-key.pem",
            ] {
                assert!(destination.join(name).is_file(), "missing {name}");
            }
            #[cfg(unix)]
            for name in ["ca-key.pem", "node-key.pem", "client-key.pem"] {
                let mode = fs::metadata(destination.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "wrong private-key mode for {name}");
            }
            if pki_key_algorithm == PkiKeyAlgorithm::EcP256 {
                let started = Instant::now();
                let error = verify_generated_pki_with_timeout(
                    &destination,
                    "not a valid server name",
                    Duration::from_millis(250),
                )
                .unwrap_err();
                assert!(
                    format!("{error:#}").contains("invalid TLS server name"),
                    "unexpected pre-connect verification error: {error:#}"
                );
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "pre-connect client failure left the verifier thread blocked"
                );
            }
            let error = init_development_pki(
                PkiInitArgs {
                    directory: Some(destination),
                    server_name: "localhost".to_string(),
                    openssl: openssl.clone(),
                },
                pki_key_algorithm,
            )
            .unwrap_err();
            assert!(error.to_string().contains("refusing to overwrite"));
        }

        let pairing = root.path().join("pairing");
        fs::create_dir(&pairing).unwrap();
        let (_, pairing_key) = ensure_pairing_ca(&pairing, PkiKeyAlgorithm::EcP256).unwrap();
        let ec_detail = run_openssl_capture(
            &openssl,
            &pairing,
            &["pkey", "-in", "pairing-ca-key.pem", "-text", "-noout"],
        )
        .unwrap();
        assert!(String::from_utf8(ec_detail)
            .unwrap()
            .contains("ASN1 OID: prime256v1"));
        let original_key = fs::read(&pairing_key).unwrap();
        ensure_pairing_ca(&pairing, PkiKeyAlgorithm::Rsa3072).unwrap();
        assert_eq!(fs::read(pairing_key).unwrap(), original_key);
    }

    #[test]
    fn runtime_binary_must_be_a_native_executable() {
        let current = std::env::current_exe().unwrap();
        assert_eq!(
            validate_native_runtime_binary(&current).unwrap(),
            current.canonicalize().unwrap()
        );

        let root = tempfile::tempdir().unwrap();
        let wrapper = root.path().join("O-wrapper");
        fs::write(&wrapper, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let error = validate_native_runtime_binary(&wrapper).unwrap_err();
        assert!(error.to_string().contains("script or unsupported"));
    }
}
