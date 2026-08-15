use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use o_lang::hosted_remote::v2::{
    default_hosted_v2_state_dir, read_node_signing_key_v2, read_placement_public_key_v2,
    serve_node_dual, write_new_node_public_key_v2, write_new_node_signing_key_v2,
    DurableSessionStoreV2, HostedDualNodeServerConfig, HostedNodeSignerV2, HostedV2Runtime,
    HostedV2RuntimeConfig, PinnedEd25519PlacementAuthorizerV2, DEFAULT_MAX_ACTORS_PER_SESSION_V2,
    DEFAULT_MAX_OPEN_SESSIONS_V2, DEFAULT_MAX_SNAPSHOT_BYTES_PER_ACTOR_V2,
    DEFAULT_MAX_STATE_BYTES_PER_SESSION_V2, DEFAULT_MAX_STATE_BYTES_TOTAL_V2,
};
use o_lang::hosted_remote::{
    accept_mutual_tls, build_client_config, build_server_config, connect_mutual_tls,
    default_ca_path, default_node_cert_path, default_node_key_path, hosted_config_dir, serve_node,
    ClientTlsIdentity, HostedNodeRuntime, HostedNodeServerConfig, NodeDoctorCheckV1,
    ServerTlsIdentity, DEFAULT_MAX_CONNECTIONS, DEFAULT_NODE_BIND, DEFAULT_NODE_ID,
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
    /// Listen for one-operation canonical-CBOR requests over mTLS.
    Serve(ServeArgs),
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
    #[arg(long, default_value = DEFAULT_NODE_ID)]
    node_id: String,
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
    #[arg(long, default_value = DEFAULT_NODE_ID)]
    node_id: String,
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
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[command(flatten)]
    runtime: RuntimeArgs,
    #[command(flatten)]
    tls: ServerTlsArgs,
    #[arg(long, default_value = DEFAULT_NODE_BIND)]
    bind: String,
    /// Enable durable session protocol V2 using this capability-first state root.
    #[arg(long)]
    v2_state_dir: Option<PathBuf>,
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
            let profile =
                o_lang::hosted_remote::NodeProfileV1::local(args.node_id, args.max_connections)?;
            println!("{}", serde_json::to_string_pretty(&profile)?);
            Ok(())
        }
        Command::Doctor(args) => doctor(args),
        Command::Serve(args) => serve(args),
    }
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

fn doctor(args: DoctorArgs) -> Result<()> {
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

fn serve(args: ServeArgs) -> Result<()> {
    let (shim_dir, _shim_guard) = resolve_shim_dir(args.runtime.shim_dir.clone())?;
    let v1_runtime = runtime_from_args(args.runtime, shim_dir)?;
    if let Some(state_dir) = args.v2_state_dir {
        let node_key_path = args
            .v2_node_signing_key
            .unwrap_or_else(|| state_dir.join("node-signing-key.v2"));
        let authority_path = args
            .v2_authority_public_key
            .context("--v2-authority-public-key is required when --v2-state-dir enables V2")?;
        let signer = read_node_signing_key_v2(&node_key_path)?;
        let authority = read_placement_public_key_v2(&authority_path)?;
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
        let v2_runtime = HostedV2Runtime::open(
            HostedV2RuntimeConfig {
                node_id: v1_runtime.node_id.clone(),
                node_generation: node_state_epoch,
                shim_dir: v1_runtime.shim_dir.clone(),
                runtime_executable: v1_runtime.runtime_executable.clone(),
                state_quota_generation,
                state_quotas,
            },
            store,
            Arc::new(PinnedEd25519PlacementAuthorizerV2::new(authority)),
        )
        .with_context(|| {
            format!(
                "failed to open durable V2 state at `{}` with stable node-state/deployment epoch {}; reuse the epoch across normal restarts, and use a new state root or archive the old root before bumping it; epoch changes never evict existing sessions",
                state_dir.display(),
                node_state_epoch.get()
            )
        })?;
        eprintln!(
            "o-node: durable V2 node-state/deployment epoch {} is stable across normal restarts; bump only with a new state root or after archiving `{}`; changing the epoch never evicts or migrates existing sessions",
            node_state_epoch.get(),
            state_dir.display()
        );
        let unreadable_sessions = v2_runtime.unreadable_sessions()?;
        if !unreadable_sessions.is_empty() {
            eprintln!(
                "o-node: retained {} unreadable durable V2 session(s); no session was evicted. A node-state epoch change requires a new state root or an archived old root",
                unreadable_sessions.len()
            );
            for diagnostic in unreadable_sessions {
                eprintln!("o-node: retained unreadable V2 session: {diagnostic}");
            }
        }
        eprintln!(
            "o-node: serving {} on {} (TLS 1.3 mTLS; frozen V1 + durable V2; max {} connections)",
            v1_runtime.node_id, args.bind, v1_runtime.max_concurrent_connections
        );
        return serve_node_dual(HostedDualNodeServerConfig {
            bind_address: args.bind,
            v1_runtime,
            v2_runtime,
            tls_identity: tls_identity(args.tls),
        });
    }
    if args.v2_node_signing_key.is_some() || args.v2_authority_public_key.is_some() {
        bail!("--v2-state-dir is required when any V2 key option is supplied");
    }
    let config = HostedNodeServerConfig {
        bind_address: args.bind,
        runtime: v1_runtime,
        tls_identity: tls_identity(args.tls),
    };
    eprintln!(
        "o-node: serving {} on {} (TLS 1.3 mTLS, max {} connections)",
        config.runtime.node_id, config.bind_address, config.runtime.max_concurrent_connections
    );
    serve_node(config)
}

fn runtime_from_args(args: RuntimeArgs, shim_dir: PathBuf) -> Result<HostedNodeRuntime> {
    let runtime_executable = resolve_runtime_binary(args.runtime_binary)?;
    Ok(HostedNodeRuntime {
        node_id: args.node_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    use clap::CommandFactory;

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
    fn development_pki_server_name_is_config_injection_safe() {
        assert_eq!(certificate_san("localhost").unwrap(), "DNS:localhost");
        assert_eq!(certificate_san("127.0.0.1").unwrap(), "IP:127.0.0.1");
        assert!(certificate_san("bad\n[evil]").is_err());
        assert!(certificate_san("*.example.test").is_err());
        assert!(certificate_san("-bad.example").is_err());
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
