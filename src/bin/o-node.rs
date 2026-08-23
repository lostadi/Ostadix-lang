use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use o_lang::hosted_remote::mesh::{MeshNodeRuntime, MeshNodeRuntimeConfig};
use o_lang::hosted_remote::v2::{
    default_hosted_v2_state_dir, read_node_signing_key_v2, read_placement_public_key_v2,
    serve_owned_node_dual_until_shutdown, write_new_node_public_key_v2,
    write_new_node_signing_key_v2, DurableSessionStoreV2, HostedDualNodeShutdown,
    HostedNodeSignerV2, HostedOwnedDualNodeServerConfig, HostedV2RuntimeConfig,
    HostedV2RuntimeOwner, LanOpenPlacementAuthorizerV2, PinnedEd25519PlacementAuthorizerV2,
    PlacementProofAuthorizerV2, DEFAULT_MAX_ACTORS_PER_SESSION_V2, DEFAULT_MAX_OPEN_SESSIONS_V2,
    DEFAULT_MAX_SNAPSHOT_BYTES_PER_ACTOR_V2, DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2,
    DEFAULT_MAX_STATE_BYTES_TOTAL_V2,
};
use o_lang::hosted_remote::{
    accept_mutual_tls, accept_pairing_once, build_client_config, build_server_config,
    connect_mutual_tls, default_ca_path, default_node_cert_path, default_node_key_path,
    discover_lan_nodes, generate_pairing_passcode, hosted_config_dir, join_pairing_once,
    lan_node_process_dir, lan_open_config_dir, lan_open_v2_state_dir, lan_peers_config_dir,
    load_stored_lan_peer, replace_paired_lan_peer, serve_node, spawn_lan_bootstrap_server,
    spawn_lan_discovery_responder, store_paired_lan_peer, ClientTlsIdentity, HostedNodeRuntime,
    HostedNodeServerConfig, LanBootstrapBundleV1, LanNodeAdvertisementV1, NodeDoctorCheckV1,
    PairingLocalIdentityV1, PairingPublicIdentityV1, PairingResultV1, ServerTlsIdentity,
    StoredLanPeerPathsV1, DEFAULT_LAN_BOOTSTRAP_PORT, DEFAULT_LAN_DISCOVERY_PORT,
    DEFAULT_LAN_NODE_PORT, DEFAULT_LAN_PAIRING_PORT, DEFAULT_MAX_CONNECTIONS, DEFAULT_NODE_BIND,
    DEFAULT_NODE_ID, LAN_BOOTSTRAP_SCHEMA_V1, LAN_SECURITY_MODE,
};
use o_lang::placement::{GenerationV1, StateQuotaLimitsV2};
use o_lang::runtime_exec::validate_native_runtime_binary;
use o_lang::shims::ExtractedShims;

const V2_NODE_EPOCH_HELP: &str = "Stable node-state/deployment epoch bound into durable V2 session identity. Reuse it across normal process restarts. To bump it, use a new state root or archive the old root first; changing this value never evicts or migrates existing sessions.";

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
    /// Serve frozen V1, optional durable Hosted V2, and scheduler/actor mesh requests over mTLS.
    Serve(ServeArgs),
}

#[derive(Debug, Clone, Args)]
struct StartArgs {
    /// Legacy compatibility: let any LAN-reachable client download a shared private key.
    #[arg(long)]
    lan_open: bool,
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
            PkiCommand::Init(args) => init_development_pki(args),
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

fn ensure_lan_open_material() -> Result<LanOpenNodeMaterial> {
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
        init_development_pki(PkiInitArgs {
            directory: Some(pki_dir.clone()),
            server_name: server_name.clone(),
            openssl: PathBuf::from("openssl"),
        })?;
        fs::write(&server_name_path, format!("{server_name}\n"))?;
    }

    // Pairing client authentication is intentionally separate from the
    // historical LAN-open CA. Generate it as an independent, stable migration
    // so adding these files can never rotate the existing node certificate.
    let (pairing_ca, pairing_ca_key) = ensure_pairing_ca(&pki_dir)?;
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

fn ensure_pairing_ca(pki_dir: &Path) -> Result<(PathBuf, PathBuf)> {
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
    run_openssl(
        Path::new("openssl"),
        temporary.path(),
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:3072",
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
        ],
    )?;
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

fn detached_node_paths() -> (PathBuf, PathBuf, PathBuf) {
    let directory = lan_node_process_dir();
    (
        directory.clone(),
        directory.join("o-node.pid"),
        directory.join("o-node.log"),
    )
}

fn start_detached_node(args: StartArgs) -> Result<()> {
    #[cfg(not(unix))]
    bail!("detached o-node start is currently supported on Unix-like systems");
    #[cfg(unix)]
    {
        if detached_node_status(false)? {
            println!("o-node is already running");
            return Ok(());
        }
        // Provision synchronously so configuration errors are shown in this
        // terminal instead of being buried in a detached log.
        let material = ensure_lan_open_material()?;
        let (directory, pid_path, log_path) = detached_node_paths();
        ensure_private_directory(&directory)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .with_context(|| format!("failed to open `{}`", log_path.display()))?;
        let current = env::current_exe().context("failed to locate o-node executable")?;
        let mut command = ProcessCommand::new(current);
        command
            .arg("serve")
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
        let mut child = command.spawn().context("failed to detach o-node serve")?;
        fs::write(&pid_path, format!("{}\n", child.id()))?;
        fs::set_permissions(&pid_path, fs::Permissions::from_mode(0o600))?;
        thread::sleep(Duration::from_millis(450));
        if let Some(status) = child.try_wait()? {
            let _ = fs::remove_file(&pid_path);
            bail!(
                "o-node exited during startup with {status}; inspect {}",
                log_path.display()
            );
        }
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
}

fn restart_detached_node(args: StartArgs) -> Result<()> {
    stop_detached_node()?;
    if detached_node_status(false)? {
        bail!("o-node is still running after stop; refusing to start a duplicate")
    }
    start_detached_node(args)
}

fn stop_detached_node() -> Result<()> {
    #[cfg(not(unix))]
    bail!("detached o-node stop is currently supported on Unix-like systems");
    #[cfg(unix)]
    {
        let (_, pid_path, _) = detached_node_paths();
        let Some(pid) = read_detached_pid(&pid_path)? else {
            println!("o-node is not running");
            return Ok(());
        };
        if !process_is_detached_node(pid) {
            let _ = fs::remove_file(&pid_path);
            println!("o-node was not running; removed stale PID file");
            return Ok(());
        }
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to signal o-node");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !process_is_alive(pid) {
                let _ = fs::remove_file(&pid_path);
                println!("o-node stopped");
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        bail!("o-node is still draining accepted work after 10 seconds")
    }
}

fn detached_node_status(print: bool) -> Result<bool> {
    let (_, pid_path, log_path) = detached_node_paths();
    let Some(pid) = read_detached_pid(&pid_path)? else {
        if print {
            println!("stopped");
        }
        return Ok(false);
    };
    let running = process_is_detached_node(pid);
    if !running {
        let _ = fs::remove_file(&pid_path);
    }
    if print {
        if running {
            println!("running pid={pid} log={}", log_path.display());
        } else {
            println!("stopped (stale PID removed)");
        }
    }
    Ok(running)
}

fn read_detached_pid(path: &Path) -> Result<Option<i32>> {
    if !path.is_file() {
        return Ok(None);
    }
    let value = fs::read_to_string(path)?;
    let pid = value
        .trim()
        .parse::<i32>()
        .with_context(|| format!("invalid o-node PID file `{}`", path.display()))?;
    if pid <= 0 {
        bail!("invalid o-node PID {pid}");
    }
    Ok(Some(pid))
}

fn process_is_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn process_is_detached_node(pid: i32) -> bool {
    if !process_is_alive(pid) {
        return false;
    }
    let expected = env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_owned()));

    #[cfg(target_os = "linux")]
    {
        if let Ok(bytes) = fs::read(format!("/proc/{pid}/cmdline")) {
            let arguments = bytes
                .split(|byte| *byte == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect::<Vec<_>>();
            return detached_command_matches(&arguments, expected.as_deref());
        }
    }

    #[cfg(unix)]
    {
        let output = ProcessCommand::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                let command = String::from_utf8_lossy(&output.stdout);
                let arguments = command
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                return detached_command_matches(&arguments, expected.as_deref());
            }
        }
    }
    false
}

fn detached_command_matches(arguments: &[String], expected: Option<&std::ffi::OsStr>) -> bool {
    let Some(executable) = arguments.first() else {
        return false;
    };
    let executable_name = Path::new(executable).file_name();
    let executable_matches = expected
        .zip(executable_name)
        .is_some_and(|(expected, actual)| expected == actual);
    executable_matches && arguments.iter().skip(1).any(|argument| argument == "serve")
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

fn init_development_pki(args: PkiInitArgs) -> Result<()> {
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
            "[req]\nprompt=no\ndistinguished_name=dn\nreq_extensions=node_ext\n[dn]\nCN={}\n[node_ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName={}\n",
            args.server_name, san
        ),
    )?;
    fs::write(
        temporary.path().join("client.cnf"),
        "[req]\nprompt=no\ndistinguished_name=dn\nreq_extensions=client_ext\n[dn]\nCN=ostadix-development-client\n[client_ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=clientAuth\n",
    )?;

    run_openssl(
        &args.openssl,
        temporary.path(),
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:3072",
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
        ],
    )?;
    run_openssl(
        &args.openssl,
        temporary.path(),
        &[
            "req",
            "-new",
            "-newkey",
            "rsa:3072",
            "-sha256",
            "-nodes",
            "-keyout",
            "node-key.pem",
            "-out",
            "node.csr",
            "-config",
            "node.cnf",
        ],
    )?;
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
    run_openssl(
        &args.openssl,
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
            "-config",
            "client.cnf",
        ],
    )?;
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

    secure_key(temporary.path().join("ca-key.pem"))?;
    secure_key(temporary.path().join("node-key.pem"))?;
    secure_key(temporary.path().join("client-key.pem"))?;
    verify_generated_pki(temporary.path(), &args.server_name)?;

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

fn serve(mut args: ServeArgs) -> Result<()> {
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

    let mut _discovery = None;
    let mut _bootstrap = None;
    if let Some(material) = automatic.as_ref() {
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
        if !args.no_discovery {
            let advertisement = if legacy_lan_open {
                LanNodeAdvertisementV1::new(
                    material.node_id.clone(),
                    material.server_name.clone(),
                    service_port,
                    DEFAULT_LAN_BOOTSTRAP_PORT,
                    args.v2_state_dir.is_some(),
                )?
            } else {
                LanNodeAdvertisementV1::pairing_required(
                    material.node_id.clone(),
                    material.server_name.clone(),
                    service_port,
                    DEFAULT_LAN_PAIRING_PORT,
                    args.v2_state_dir.is_some(),
                )?
            };
            _discovery = Some(spawn_lan_discovery_responder(
                advertisement,
                DEFAULT_LAN_DISCOVERY_PORT,
            )?);
        }
        if legacy_lan_open && !args.no_bootstrap {
            _bootstrap = Some(spawn_lan_bootstrap_server(
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_LAN_BOOTSTRAP_PORT)),
                lan_bootstrap_bundle(material, service_port)?,
            )?);
        }
    }

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
        let mesh_status = if let Some(mesh_state_dir) = mesh_state_dir.as_ref() {
            eprintln!(
                "o-node: scheduler/actor mesh enabled at `{}` with capacity for {} concurrent actors",
                mesh_state_dir.display(),
                v1_runtime.max_concurrent_connections
            );
            "scheduler/actor mesh enabled"
        } else {
            "scheduler/actor mesh disabled"
        };
        eprintln!(
            "o-node: serving {} on {} (TLS 1.3 mTLS; frozen V1 + durable V2; {}; max {} connections)",
            v1_runtime.node_id,
            bind_address,
            mesh_status,
            v1_runtime.max_concurrent_connections
        );
        return serve_owned_node_dual_until_shutdown(
            HostedOwnedDualNodeServerConfig {
                bind_address,
                v1_runtime,
                v2_runtime,
                mesh_runtime,
                tls_identity: tls_identity(args.tls),
            },
            shutdown,
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
    eprintln!(
        "o-node: serving {} on {} (TLS 1.3 mTLS, max {} connections)",
        config.runtime.node_id, config.bind_address, config.runtime.max_concurrent_connections
    );
    serve_node(config)
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
    let server = thread::spawn(move || -> Result<()> {
        let (tcp, _) = listener
            .accept()
            .context("generated-PKI verification accept failed")?;
        let _stream = accept_mutual_tls(
            tcp,
            server_config,
            Duration::from_secs(3),
            Duration::from_secs(3),
        )?;
        Ok(())
    });
    let client_result = connect_mutual_tls(
        &address.to_string(),
        &client_identity,
        Duration::from_secs(3),
        Duration::from_secs(3),
    );
    let server_result = server
        .join()
        .map_err(|_| anyhow::anyhow!("generated-PKI verification thread panicked"))?;
    client_result.context("generated PKI failed client-side loopback mTLS verification")?;
    server_result.context("generated PKI failed server-side loopback mTLS verification")?;
    Ok(())
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
    fn detached_pid_guard_only_matches_this_binary_in_serve_mode() {
        let expected = std::ffi::OsStr::new("o-node");
        assert!(detached_command_matches(
            &["/tmp/o-node".to_owned(), "serve".to_owned()],
            Some(expected)
        ));
        assert!(!detached_command_matches(
            &["/tmp/o-node".to_owned(), "profile".to_owned()],
            Some(expected)
        ));
        assert!(!detached_command_matches(
            &["/usr/bin/sleep".to_owned(), "serve".to_owned()],
            Some(expected)
        ));
    }

    #[test]
    fn development_pki_server_name_is_config_injection_safe() {
        assert_eq!(certificate_san("localhost").unwrap(), "DNS:localhost");
        assert_eq!(certificate_san("127.0.0.1").unwrap(), "IP:127.0.0.1");
        assert!(certificate_san("bad\n[evil]").is_err());
        assert!(certificate_san("*.example.test").is_err());
        assert!(certificate_san("-bad.example").is_err());
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
        let destination = root.path().join("hosted");
        init_development_pki(PkiInitArgs {
            directory: Some(destination.clone()),
            server_name: "localhost".to_string(),
            openssl: openssl.clone(),
        })
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
        let error = init_development_pki(PkiInitArgs {
            directory: Some(destination),
            server_name: "localhost".to_string(),
            openssl,
        })
        .unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));
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
