use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use o_lang::hosted_remote::{
    accept_mutual_tls, build_client_config, build_server_config, connect_mutual_tls,
    default_ca_path, default_node_cert_path, default_node_key_path, hosted_config_dir, serve_node,
    ClientTlsIdentity, HostedNodeRuntime, HostedNodeServerConfig, NodeDoctorCheckV1,
    ServerTlsIdentity, DEFAULT_MAX_CONNECTIONS, DEFAULT_NODE_BIND, DEFAULT_NODE_ID,
};
use o_lang::shims::ExtractedShims;

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
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Pki(args) => match args.command {
            PkiCommand::Init(args) => init_development_pki(args),
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

fn doctor(args: DoctorArgs) -> Result<()> {
    let (shim_dir, _shim_guard) = resolve_shim_dir(args.runtime.shim_dir.clone())?;
    let runtime = runtime_from_args(args.runtime, shim_dir);
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
    let config = HostedNodeServerConfig {
        bind_address: args.bind,
        runtime: runtime_from_args(args.runtime, shim_dir),
        tls_identity: tls_identity(args.tls),
    };
    eprintln!(
        "o-node: serving {} on {} (TLS 1.3 mTLS, max {} connections)",
        config.runtime.node_id, config.bind_address, config.runtime.max_concurrent_connections
    );
    serve_node(config)
}

fn runtime_from_args(args: RuntimeArgs, shim_dir: PathBuf) -> HostedNodeRuntime {
    HostedNodeRuntime {
        node_id: args.node_id,
        shim_dir,
        max_concurrent_connections: args.max_connections,
    }
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
