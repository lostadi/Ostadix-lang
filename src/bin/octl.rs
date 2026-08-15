use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use o_lang::hosted_remote::{
    default_ca_path, default_client_cert_path, default_client_key_path, unix_time_ms,
    ClientTlsIdentity, HostedNodeClient, HostedOperationOutcomeV1, RemotePreparedOperationV1,
    DEFAULT_NODE_ADDRESS, DEFAULT_TLS_SERVER_NAME, MAX_HOSTED_OUTPUT_BYTES,
    MAX_HOSTED_SOURCE_BYTES,
};

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
    /// Fetch the node's descriptive backend catalog and transport limits.
    Profile(NodeQueryArgs),
    /// Fetch node-local readiness checks (not a placement warrant).
    Doctor(NodeQueryArgs),
    /// Run one exact O source document on the explicitly selected node.
    Run(NodeRunArgs),
}

#[derive(Debug, Clone, Args)]
struct NodeConnectionArgs {
    #[arg(long, default_value = DEFAULT_NODE_ADDRESS)]
    address: String,
    /// DNS name or IP SAN pinned by the node certificate.
    #[arg(long, default_value = DEFAULT_TLS_SERVER_NAME)]
    server_name: String,
    /// Server CA PEM (default: XDG config ostadix/hosted/ca.pem).
    #[arg(long)]
    ca: Option<PathBuf>,
    /// Client certificate chain PEM.
    #[arg(long)]
    cert: Option<PathBuf>,
    /// Client private key PEM.
    #[arg(long)]
    key: Option<PathBuf>,
    #[arg(long, default_value_t = 10)]
    connect_timeout_seconds: u64,
    #[arg(long, default_value_t = 60)]
    io_timeout_seconds: u64,
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

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Node(args) => match args.command {
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
        },
    }
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
    if args.connect_timeout_seconds == 0 || args.io_timeout_seconds == 0 {
        bail!("node connection timeouts must be positive");
    }
    if args.connect_timeout_seconds > 3600 || args.io_timeout_seconds > 3600 {
        bail!("node connection timeouts may not exceed 3600 seconds");
    }
    let mut client = HostedNodeClient::new(
        args.address,
        ClientTlsIdentity {
            ca_path: args.ca.unwrap_or_else(default_ca_path),
            cert_path: args.cert.unwrap_or_else(default_client_cert_path),
            key_path: args.key.unwrap_or_else(default_client_key_path),
            server_name: args.server_name,
        },
    );
    client.connect_timeout = Duration::from_secs(args.connect_timeout_seconds);
    client.io_timeout = Duration::from_secs(args.io_timeout_seconds);
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
