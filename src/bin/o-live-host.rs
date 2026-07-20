//! Hosted Live-World Reference command line.
//!
//! This executable is a semantic oracle for the future O-core live system. It
//! is deliberately named `-host`: its workers are local host child processes,
//! not dynamically loaded O-core tasks.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{ensure, Context, Result};
use clap::{Parser as ClapParser, Subcommand};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use o_lang::live_system::manifest::{
    payload_sha256, BuildManifest, CapabilityRequestManifest, HealthManifest, PackageDigest,
    PackageManifest, RuntimeManifest, ServiceManifest, VerifiedPackage, MAX_MANIFEST_BYTES,
    PACKAGE_SCHEMA_V1,
};
use o_lang::live_system::protocol::{self, RUNTIME_PROGRAM_SCHEMA, RUNTIME_PROTOCOL};
use o_lang::live_system::store::PackageStore;
use o_lang::live_system::supervisor::{
    ActivationPolicy, CompositionStep, HostedSupervisor, SupervisorConfig, HEALTH_PROTOCOL,
    SERVICE_RIGHT_INVOKE,
};
use o_lang::value::OValue;

const POLICY_SCHEMA: &str = "ocore.hosted-activation-policy/v1";
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const MAX_POLICY_GRANTS: usize = 256;
const MAX_VALUE_FILE_BYTES: u64 = 512 * 1024;
const MAX_COMPOSITION_STEPS: usize = 256;

#[derive(Debug, ClapParser)]
#[command(
    name = "o-live-host",
    about = "Hosted package-managed Live-World reference for Ostadix"
)]
struct Cli {
    /// Strict, exact-match activation-policy TOML. Omitted means default deny.
    #[arg(long, global = true, value_name = "FILE")]
    policy: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify a package and print its content identity without installing it.
    Pack {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        payload: PathBuf,
    },

    /// Verify and atomically publish a package into the local immutable CAS.
    Install {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        payload: PathBuf,
        #[arg(long)]
        alias: Option<String>,
    },

    /// Health-gate and atomically activate an installed package digest.
    Activate {
        #[arg(long)]
        state: PathBuf,
        digest: String,
    },

    /// Stage a new digest for a package and retain its healthy rollback root.
    Upgrade {
        #[arg(long)]
        state: PathBuf,
        digest: String,
    },

    /// Invoke one active service through a fresh generation-bound bearer.
    Invoke {
        #[arg(long)]
        state: PathBuf,
        service: String,
        protocol: String,
        operation: String,
        input: PathBuf,
    },

    /// Pipe one structural OValue through a bounded JSON composition plan.
    Compose {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },

    /// Atomically swap a package with its retained healthy generation.
    Rollback {
        #[arg(long)]
        state: PathBuf,
        package: String,
    },

    /// Restart one service without rotating unrelated service generations.
    Restart {
        #[arg(long)]
        state: PathBuf,
        service: String,
    },

    /// Reconstruct, health-check, and display the active service set.
    Status {
        #[arg(long)]
        state: PathBuf,
    },

    /// Run the self-contained transactional two-world acceptance scenario.
    Demo {
        #[arg(long)]
        state: PathBuf,
    },

    /// Internal fixed-protocol runtime worker entrypoint.
    #[command(name = "__worker", hide = true)]
    Worker {
        #[arg(long)]
        package_root: PathBuf,
        #[arg(long)]
        entry: String,
        #[arg(long)]
        service: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyDocument {
    schema: String,
    grants: Vec<PolicyGrant>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyGrant {
    package: String,
    kind: String,
    purpose: String,
    rights: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Pack { manifest, payload } => pack(&manifest, &payload),
        Command::Install {
            state,
            manifest,
            payload,
            alias,
        } => with_state_lock(&state, || {
            install(&state, &manifest, &payload, alias.as_deref())
        }),
        Command::Activate { state, digest } => with_state_lock(&state, || {
            activate(&state, cli.policy.as_deref(), &digest, "activated")
        }),
        Command::Upgrade { state, digest } => with_state_lock(&state, || {
            activate(&state, cli.policy.as_deref(), &digest, "upgraded")
        }),
        Command::Invoke {
            state,
            service,
            protocol,
            operation,
            input,
        } => with_state_lock(&state, || {
            invoke(
                &state,
                cli.policy.as_deref(),
                &service,
                &protocol,
                &operation,
                &input,
            )
        }),
        Command::Compose { state, plan, input } => with_state_lock(&state, || {
            compose(&state, cli.policy.as_deref(), &plan, &input)
        }),
        Command::Rollback { state, package } => {
            with_state_lock(&state, || rollback(&state, cli.policy.as_deref(), &package))
        }
        Command::Restart { state, service } => {
            with_state_lock(&state, || restart(&state, cli.policy.as_deref(), &service))
        }
        Command::Status { state } => {
            with_state_lock(&state, || status(&state, cli.policy.as_deref()))
        }
        Command::Demo { state } => with_state_lock(&state, || demo(&state)),
        Command::Worker {
            package_root,
            entry,
            service,
        } => protocol::worker_main(&package_root, &entry, &service),
    }
}

/// Serialize each complete reconstruct-and-operate CLI transaction for one
/// authority directory. Without this process-shared lock, two independently
/// healthy activations can both commit stale snapshots and silently discard
/// one another's package from the active set.
struct StateLock {
    file: File,
}

impl StateLock {
    fn acquire(state: &Path) -> Result<Self> {
        fs::create_dir_all(state)
            .with_context(|| format!("failed to create state directory {}", state.display()))?;
        let path = state.join(".o-live-host.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open state lock {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect state lock {}", path.display()))?;
        ensure!(
            metadata.is_file(),
            "state lock must be a regular non-symlink file: {}",
            path.display()
        );
        #[cfg(unix)]
        {
            // SAFETY: `file` owns a live descriptor for the dedicated lock
            // inode. flock is held until StateLock drops after the complete
            // reconstruct-and-command closure.
            let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if status != 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("failed to acquire state lock {}", path.display()));
            }
        }
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: this unlocks only the descriptor locked by acquire.
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn with_state_lock<T>(state: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = StateLock::acquire(state)?;
    operation()
}

fn pack(manifest_path: &Path, payload: &Path) -> Result<()> {
    let manifest = read_bounded_text(manifest_path, MAX_MANIFEST_BYTES as u64, "package manifest")?;
    let package = VerifiedPackage::load(&manifest, payload)?;
    println!("{}", package.digest());
    Ok(())
}

fn install(state: &Path, manifest_path: &Path, payload: &Path, alias: Option<&str>) -> Result<()> {
    let manifest = read_bounded_text(manifest_path, MAX_MANIFEST_BYTES as u64, "package manifest")?;
    let store = PackageStore::open(store_path(state))?;
    let stored = store.install(&manifest, payload)?;
    if let Some(alias) = alias {
        store.set_alias(alias, stored.digest())?;
    }
    println!("{}", stored.digest());
    Ok(())
}

fn activate(state: &Path, policy: Option<&Path>, digest: &str, verb: &str) -> Result<()> {
    let mut supervisor = open_supervisor(state, policy)?;
    supervisor.reconstruct()?;
    let digest = parse_digest(digest)?;
    let services = supervisor.activate(&digest)?;
    println!("{verb} {}", digest);
    print_json(&services)
}

fn invoke(
    state: &Path,
    policy: Option<&Path>,
    service: &str,
    protocol: &str,
    operation: &str,
    input_path: &Path,
) -> Result<()> {
    let input = read_ovalue(input_path)?;
    let mut supervisor = open_supervisor(state, policy)?;
    supervisor.reconstruct()?;
    let capability = supervisor.service_capability(service, protocol, [SERVICE_RIGHT_INVOKE])?;
    let output = supervisor.invoke(&capability, operation, input)?;
    print_json(&output)
}

fn compose(state: &Path, policy: Option<&Path>, plan: &Path, input: &Path) -> Result<()> {
    let plan_bytes = read_bounded_regular(plan, MAX_VALUE_FILE_BYTES, "composition plan")?;
    let steps: Vec<CompositionStep> =
        serde_json::from_slice(&plan_bytes).context("invalid composition-plan JSON")?;
    ensure!(
        !steps.is_empty() && steps.len() <= MAX_COMPOSITION_STEPS,
        "composition plan must contain between 1 and {MAX_COMPOSITION_STEPS} steps"
    );
    let input = read_ovalue(input)?;
    let mut supervisor = open_supervisor(state, policy)?;
    supervisor.reconstruct()?;
    let output = supervisor.compose(&steps, input)?;
    print_json(&output)
}

fn rollback(state: &Path, policy: Option<&Path>, package: &str) -> Result<()> {
    let mut supervisor = open_supervisor(state, policy)?;
    supervisor.reconstruct()?;
    let services = supervisor.rollback(package)?;
    println!("rolled back {package}");
    print_json(&services)
}

fn restart(state: &Path, policy: Option<&Path>, service: &str) -> Result<()> {
    let mut supervisor = open_supervisor(state, policy)?;
    supervisor.reconstruct()?;
    let status = supervisor.restart_service(service)?;
    println!("restarted {service}");
    print_json(&status)
}

fn status(state: &Path, policy: Option<&Path>) -> Result<()> {
    let mut supervisor = open_supervisor(state, policy)?;
    let services = supervisor.reconstruct()?;
    print_json(&services)
}

fn open_supervisor(state: &Path, policy_path: Option<&Path>) -> Result<HostedSupervisor> {
    let store = PackageStore::open(store_path(state))?;
    let policy = load_policy(policy_path)?;
    let executable = std::env::current_exe().context("failed to locate o-live-host executable")?;
    HostedSupervisor::new(
        store,
        active_set_path(state),
        executable,
        policy,
        SupervisorConfig::default(),
    )
}

fn store_path(state: &Path) -> PathBuf {
    state.join("store")
}

fn active_set_path(state: &Path) -> PathBuf {
    state.join("active-set.json")
}

fn load_policy(path: Option<&Path>) -> Result<ActivationPolicy> {
    let Some(path) = path else {
        return Ok(ActivationPolicy::new());
    };
    let bytes = read_bounded_regular(path, MAX_POLICY_BYTES, "activation policy")?;
    let text = std::str::from_utf8(&bytes).context("activation policy is not UTF-8")?;
    let document: PolicyDocument =
        toml::from_str(text).context("invalid activation-policy TOML")?;
    ensure!(
        document.schema == POLICY_SCHEMA,
        "unsupported activation-policy schema `{}`; expected `{POLICY_SCHEMA}`",
        document.schema
    );
    ensure!(
        document.grants.len() <= MAX_POLICY_GRANTS,
        "activation policy exceeds {MAX_POLICY_GRANTS} grants"
    );
    let mut policy = ActivationPolicy::new();
    let mut exact_grants = BTreeSet::new();
    for grant in document.grants {
        ensure!(
            exact_grants.insert((
                grant.package.clone(),
                grant.kind.clone(),
                grant.purpose.clone(),
            )),
            "activation policy contains a duplicate exact grant"
        );
        policy.allow_request(grant.package, grant.kind, grant.purpose, grant.rights)?;
    }
    Ok(policy)
}

fn parse_digest(value: &str) -> Result<PackageDigest> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    PackageDigest::from_hex(value).context("invalid package digest")
}

fn read_ovalue(path: &Path) -> Result<OValue> {
    let bytes = read_bounded_regular(path, MAX_VALUE_FILE_BYTES, "OValue input")?;
    serde_json::from_slice(&bytes).context("invalid tagged OValue JSON")
}

fn read_bounded_text(path: &Path, max: u64, label: &str) -> Result<String> {
    let bytes = read_bounded_regular(path, max, label)?;
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))
}

fn read_bounded_regular(path: &Path, max: u64, label: &str) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {label} {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "{label} must be a regular non-symlink file: {}",
        path.display()
    );
    ensure!(
        metadata.len() <= max,
        "{label} exceeds {max} bytes (got {})",
        metadata.len()
    );
    let limit = max
        .checked_add(1)
        .context("bounded-read limit overflowed")?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label} {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= max,
        "{label} exceeded {max} bytes while it was being read"
    );
    Ok(bytes)
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

struct DemoPackage {
    manifest: String,
    payload: PathBuf,
}

fn demo(state: &Path) -> Result<()> {
    fs::create_dir_all(state)
        .with_context(|| format!("failed to create demo state root {}", state.display()))?;
    let inputs = state.join(format!("demo-inputs-{}", std::process::id()));
    fs::create_dir(&inputs).with_context(|| {
        format!(
            "demo input directory must be new and private: {}",
            inputs.display()
        )
    })?;

    let source_v1 = create_demo_package(
        &inputs,
        "source-v1",
        "runtime.source",
        "1.0.0",
        "world.source",
        &runtime_program("native.source.v1", "healthy", SOURCE_V1_OPERATIONS),
        Vec::new(),
    )?;
    let source_unhealthy_v2 = create_demo_package(
        &inputs,
        "source-v2-unhealthy",
        "runtime.source",
        "2.0.0",
        "world.source",
        &runtime_program("native.source.v2", "unhealthy", SOURCE_V2_OPERATIONS),
        Vec::new(),
    )?;
    let source_v2 = create_demo_package(
        &inputs,
        "source-v2",
        "runtime.source",
        "2.0.0",
        "world.source",
        &runtime_program("native.source.v2", "healthy", SOURCE_V2_OPERATIONS),
        Vec::new(),
    )?;
    let sum = create_demo_package(
        &inputs,
        "sum-v1",
        "runtime.sum",
        "1.0.0",
        "world.sum",
        &runtime_program("native.sum.v1", "healthy", SUM_OPERATIONS),
        Vec::new(),
    )?;
    let denied = create_demo_package(
        &inputs,
        "denied-v1",
        "runtime.denied",
        "1.0.0",
        "world.denied",
        &runtime_program("native.denied.v1", "healthy", IDENTITY_OPERATIONS),
        vec![CapabilityRequestManifest {
            kind: "network_endpoint".into(),
            rights: vec!["connect".into()],
            purpose: "ambient outbound network".into(),
        }],
    )?;

    let store = PackageStore::open(store_path(state))?;
    let source_v1 = store.install(&source_v1.manifest, &source_v1.payload)?;
    let source_v1_again = store.install(
        &read_bounded_text(
            &inputs.join("source-v1").join("manifest.toml"),
            MAX_MANIFEST_BYTES as u64,
            "demo manifest",
        )?,
        &inputs.join("source-v1").join("payload"),
    )?;
    ensure!(source_v1.digest() == source_v1_again.digest());
    let source_unhealthy_v2 =
        store.install(&source_unhealthy_v2.manifest, &source_unhealthy_v2.payload)?;
    let source_v2 = store.install(&source_v2.manifest, &source_v2.payload)?;
    let sum = store.install(&sum.manifest, &sum.payload)?;
    let denied = store.install(&denied.manifest, &denied.payload)?;
    for digest in [
        source_v1.digest(),
        source_unhealthy_v2.digest(),
        source_v2.digest(),
        sum.digest(),
        denied.digest(),
    ] {
        ensure!(store.contains(digest)?);
        store.verify(digest)?;
    }
    fs::remove_dir_all(&inputs)
        .with_context(|| format!("failed to remove demo inputs {}", inputs.display()))?;
    println!("HOSTED live reference: immutable package CAS PASS");

    let executable = std::env::current_exe().context("failed to locate demo worker")?;
    let mut supervisor = HostedSupervisor::new(
        store.clone(),
        active_set_path(state),
        executable.clone(),
        ActivationPolicy::new(),
        SupervisorConfig::default(),
    )?;
    supervisor.reconstruct()?;

    let denied_error = supervisor
        .activate(denied.digest())
        .expect_err("over-broad package must be denied");
    ensure!(
        denied_error
            .to_string()
            .contains("activation policy denies"),
        "over-broad package failed for the wrong reason: {denied_error:#}"
    );
    ensure!(supervisor.active_digest("runtime.denied").is_none());
    println!("HOSTED live reference: over-broad capability denied");

    supervisor.activate(source_v1.digest())?;
    supervisor.activate(sum.digest())?;
    ensure!(supervisor.services().len() == 2);
    println!("HOSTED live reference: health-gated activation PASS");

    let steps = demo_composition_steps();
    let result = supervisor.compose(&steps, OValue::Null)?;
    ensure!(result.as_int()? == 42);
    println!("HOSTED live reference: cross-world OValue composition PASS");

    let source_v1_bearer =
        supervisor.service_capability("world.source", RUNTIME_PROTOCOL, [SERVICE_RIGHT_INVOKE])?;
    let failed_upgrade = supervisor
        .activate(source_unhealthy_v2.digest())
        .expect_err("unhealthy upgrade must fail before publication");
    ensure!(failed_upgrade.to_string().contains("unhealthy"));
    let source_v1_identity = source_v1.digest().to_string();
    ensure!(supervisor.active_digest("runtime.source") == Some(source_v1_identity.as_str()));
    supervisor.invoke(&source_v1_bearer, "produce", OValue::Null)?;
    println!("HOSTED live reference: failed upgrade rollback PASS");

    supervisor.activate(source_v2.digest())?;
    ensure!(supervisor
        .invoke(&source_v1_bearer, "produce", OValue::Null)
        .is_err());
    let source_v2_bearer =
        supervisor.service_capability("world.source", RUNTIME_PROTOCOL, [SERVICE_RIGHT_INVOKE])?;
    supervisor.rollback("runtime.source")?;
    ensure!(supervisor
        .invoke(&source_v2_bearer, "produce", OValue::Null)
        .is_err());
    let source_after_rollback =
        supervisor.service_capability("world.source", RUNTIME_PROTOCOL, [SERVICE_RIGHT_INVOKE])?;
    supervisor.invoke(&source_after_rollback, "produce", OValue::Null)?;
    println!("HOSTED live reference: stale service bearer denied");

    let sum_before_crash =
        supervisor.service_capability("world.sum", RUNTIME_PROTOCOL, [SERVICE_RIGHT_INVOKE])?;
    ensure!(supervisor
        .invoke(&sum_before_crash, "crash", OValue::Null)
        .is_err());
    supervisor.invoke(&source_after_rollback, "produce", OValue::Null)?;
    let restarted = supervisor.restart_crashed()?;
    ensure!(restarted == vec!["world.sum".to_owned()]);
    ensure!(supervisor
        .invoke(&sum_before_crash, "compute", demo_pair())
        .is_err());
    let sum_after_restart =
        supervisor.service_capability("world.sum", RUNTIME_PROTOCOL, [SERVICE_RIGHT_INVOKE])?;
    ensure!(
        supervisor
            .invoke(&sum_after_restart, "compute", demo_pair())?
            .as_int()?
            == 42
    );
    supervisor.invoke(&source_after_rollback, "produce", OValue::Null)?;
    println!("HOSTED live reference: crash isolation and restart PASS");

    let stale_session_bearer = source_after_rollback;
    drop(supervisor);
    let mut reconstructed = HostedSupervisor::new(
        store,
        active_set_path(state),
        executable,
        ActivationPolicy::new(),
        SupervisorConfig::default(),
    )?;
    let reconstructed_services = reconstructed.reconstruct()?;
    ensure!(reconstructed_services.len() == 2);
    ensure!(reconstructed
        .invoke(&stale_session_bearer, "produce", OValue::Null)
        .is_err());
    ensure!(reconstructed.compose(&steps, OValue::Null)?.as_int()? == 42);
    println!("HOSTED live reference: active-set reconstruction PASS");
    println!("HOSTED live reference: PASS");
    Ok(())
}

fn runtime_program(world: &str, health: &str, operations: &str) -> String {
    format!(
        "schema = \"{RUNTIME_PROGRAM_SCHEMA}\"\nworld = \"{world}\"\n\n[health]\nstatus = \"{health}\"\n\n{operations}\n"
    )
}

fn create_demo_package(
    inputs: &Path,
    directory: &str,
    package_name: &str,
    version: &str,
    service: &str,
    runtime_text: &str,
    capability_requests: Vec<CapabilityRequestManifest>,
) -> Result<DemoPackage> {
    let root = inputs.join(directory);
    let payload = root.join("payload");
    let bin = payload.join("bin");
    fs::create_dir_all(&bin)
        .with_context(|| format!("failed to create demo package {}", root.display()))?;
    fs::write(bin.join("live.toml"), runtime_text.as_bytes())?;

    let manifest = PackageManifest {
        schema: PACKAGE_SCHEMA_V1.into(),
        name: package_name.into(),
        version: version.into(),
        architecture: std::env::consts::ARCH.into(),
        payload_sha256: payload_sha256(&payload)?,
        runtime: RuntimeManifest {
            kind: "native_test_runtime".into(),
            entry: "/bin/live.toml".into(),
            abi: RUNTIME_PROTOCOL.into(),
        },
        services: vec![ServiceManifest {
            name: service.into(),
            protocol: RUNTIME_PROTOCOL.into(),
        }],
        capability_requests,
        health: HealthManifest {
            protocol: HEALTH_PROTOCOL.into(),
            timeout_ms: 1_000,
        },
        build: BuildManifest {
            source_sha256: hex::encode(Sha256::digest(runtime_text.as_bytes())),
            builder: "o-live-host-demo/v1".into(),
        },
    };
    let manifest = manifest.canonical_toml()?;
    fs::write(root.join("manifest.toml"), manifest.as_bytes())?;
    Ok(DemoPackage { manifest, payload })
}

fn demo_composition_steps() -> Vec<CompositionStep> {
    vec![
        CompositionStep::new("world.source", RUNTIME_PROTOCOL, "produce"),
        CompositionStep::new("world.sum", RUNTIME_PROTOCOL, "compute"),
    ]
}

fn demo_pair() -> OValue {
    use std::collections::BTreeMap;
    OValue::Object {
        fields: BTreeMap::from([
            ("lhs".into(), OValue::int(20)),
            ("rhs".into(), OValue::int(22)),
        ]),
    }
}

const SOURCE_V1_OPERATIONS: &str = r#"[operations.produce]
kind = "int_pair"
lhs = 20
rhs = 22"#;

const SOURCE_V2_OPERATIONS: &str = r#"[operations.produce]
kind = "int_pair"
lhs = 40
rhs = 2"#;

const SUM_OPERATIONS: &str = r#"[operations.compute]
kind = "sum_fields"
lhs = "lhs"
rhs = "rhs"

[operations.crash]
kind = "crash""#;

const IDENTITY_OPERATIONS: &str = r#"[operations.identity]
kind = "identity""#;
