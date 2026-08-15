//! Local durable CLI for the signed Ostadix placement registry.
//!
//! This command reads and writes transport-independent snapshots. It is not a
//! network daemon and performs no automatic node discovery.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use o_lang::ir::{BackendAdapterKind, BackendRegistry};
use o_lang::placement::{
    BackendImplementationIdV1, CapabilityAtomV1, CapabilityKeyV1, EndiannessV1, GenerationV1,
    NodeProfileV1 as PlacementNodeProfileV1, PlatformDescriptorV1, SemanticDigestV1,
    TargetCapabilityModelV1, TargetDescriptorV1, UnixMillisV1,
};
use o_lang::registry::{
    append_profile_to_store, atomic_write_node_profile_json, atomic_write_registry_store,
    export_registry_store, import_registry_store, read_node_profile_json, read_registry_store,
    read_registry_trust, read_signing_key, registry_public_key_id, verify_registry_store,
    write_new_registry_state, ProfilePublicationV1, ProfileStalenessPolicyV1, RegistrySignerV1,
    RegistryStatePathsV1,
};
use o_lang::world::ArtifactId;
use sha2::{Digest, Sha256};

const DEFAULT_ROOT_LIFETIME_SECONDS: u64 = 10 * 365 * 24 * 60 * 60;

#[derive(Debug, Parser)]
#[command(
    name = "o-registry",
    version,
    about = "Manage local signed Ostadix placement-registry snapshots"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a namespace root, private signing key, and pinned trust file.
    Init {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        trust: PathBuf,
        #[arg(long)]
        namespace: String,
        #[arg(long, default_value_t = DEFAULT_ROOT_LIFETIME_SECONDS)]
        root_lifetime_seconds: u64,
    },
    /// Discover a bounded local target descriptor and write publishable JSON.
    ProfileLocal {
        /// Registry signing key whose public identity is bound into the profile.
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        operating_system: Option<String>,
        #[arg(long)]
        architecture: Option<String>,
        #[arg(long)]
        abi: Option<String>,
        #[arg(long, default_value_t = 1)]
        node_generation: u64,
        /// Monotonic publication generation (default: current Unix milliseconds).
        #[arg(long)]
        profile_generation: Option<u64>,
        /// Canonical backend to fingerprint. Repeat for multiple backends.
        #[arg(long = "backend")]
        backends: Vec<String>,
        /// Exact O executable containing inline/native Rust adapters.
        #[arg(long)]
        runtime_binary: Option<PathBuf>,
        /// Directory containing compatibility shims selected by --backend.
        #[arg(long)]
        shim_dir: Option<PathBuf>,
        /// Downward-closed capability in namespace/name@level form.
        #[arg(long = "capability")]
        capabilities: Vec<String>,
        /// Descriptive raw CPU feature. Repeat to add multiple features.
        #[arg(long = "cpu-feature")]
        cpu_features: Vec<String>,
        #[arg(long, default_value_t = 45)]
        valid_for_seconds: u64,
    },
    /// Append one signed NodeProfileV1 JSON document.
    PublishProfile {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        trust: PathBuf,
        #[arg(long)]
        namespace: String,
        #[arg(long)]
        profile: PathBuf,
        /// Permit other latest profiles that have expired.
        #[arg(long)]
        allow_stale_profiles: bool,
    },
    /// Verify signatures, trust delegation, append-only chains, and freshness.
    Verify {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        trust: PathBuf,
        #[arg(long)]
        allow_stale_profiles: bool,
    },
    /// List the latest verified profile for every namespace/node pair.
    List {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        trust: PathBuf,
        #[arg(long)]
        allow_stale_profiles: bool,
    },
    /// Verify and atomically export the canonical snapshot store.
    Export {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        trust: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        allow_stale_profiles: bool,
    },
    /// Merge a canonical export after delegation and anti-rollback checks.
    Import {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        trust: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        allow_stale_profiles: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Init {
            state,
            key,
            trust,
            namespace,
            root_lifetime_seconds,
        } => {
            let now = unix_millis()?;
            let lifetime_ms = root_lifetime_seconds
                .checked_mul(1_000)
                .context("root lifetime overflowed milliseconds")?;
            let expires = now
                .checked_add(lifetime_ms)
                .context("root expiry overflowed Unix milliseconds")?;
            let signer = RegistrySignerV1::generate().context("could not generate registry key")?;
            let paths = RegistryStatePathsV1::new(&state, &key, &trust);
            write_new_registry_state(&paths, namespace, now, expires, &signer)
                .context("could not initialize registry")?;
            println!(
                "initialized registry state={} key={} trust={}",
                state.display(),
                key.display(),
                trust.display()
            );
        }
        Command::ProfileLocal {
            key,
            output,
            node_id,
            display_name,
            operating_system,
            architecture,
            abi,
            node_generation,
            profile_generation,
            backends,
            runtime_binary,
            shim_dir,
            capabilities,
            cpu_features,
            valid_for_seconds,
        } => {
            let now = unix_millis()?;
            let signer = read_signing_key(&key).context("could not read registry signing key")?;
            let expires = now
                .checked_add(
                    valid_for_seconds
                        .checked_mul(1_000)
                        .context("profile lifetime overflowed milliseconds")?,
                )
                .context("profile expiry overflowed Unix milliseconds")?;
            let capabilities = capabilities
                .iter()
                .map(|value| parse_capability(value))
                .collect::<Result<Vec<_>>>()?;
            let implementations = discover_backend_implementations(
                &backends,
                runtime_binary.as_deref(),
                shim_dir.as_deref(),
            )?;
            let target = TargetDescriptorV1::new(
                &node_id,
                display_name.unwrap_or_else(|| node_id.clone()),
                GenerationV1::new(node_generation).context("invalid node generation")?,
                TargetCapabilityModelV1::DownwardClosedIdeal,
                PlatformDescriptorV1::new(
                    operating_system.unwrap_or_else(|| std::env::consts::OS.to_owned()),
                    architecture.unwrap_or_else(|| std::env::consts::ARCH.to_owned()),
                    abi.unwrap_or_else(default_host_abi),
                    if cfg!(target_endian = "little") {
                        EndiannessV1::Little
                    } else {
                        EndiannessV1::Big
                    },
                    usize::BITS as u16,
                )
                .context("invalid local platform descriptor")?,
                capabilities,
                cpu_features,
                implementations,
            )
            .context("invalid local target descriptor")?;
            let issuer = SemanticDigestV1::from_sha256(hex::encode(registry_public_key_id(
                &signer.public_key(),
            )))
            .context("invalid registry key identity")?;
            let profile = PlacementNodeProfileV1::new(
                issuer,
                target,
                GenerationV1::new(profile_generation.unwrap_or(now))
                    .context("invalid profile generation")?,
                UnixMillisV1::new(now),
                UnixMillisV1::new(expires),
            )
            .context("local profile validity is invalid")?;
            atomic_write_node_profile_json(&output, &profile)
                .context("could not atomically write local profile")?;
            println!("wrote placement profile to {}", output.display());
        }
        Command::PublishProfile {
            state,
            key,
            trust,
            namespace,
            profile,
            allow_stale_profiles,
        } => {
            let now = unix_millis()?;
            let policy = staleness_policy(allow_stale_profiles);
            let signer = read_signing_key(&key).context("could not read registry signing key")?;
            let trust_record = read_registry_trust(&trust).context("could not read trust file")?;
            let mut store = read_registry_store(&state).context("could not read registry state")?;
            let profile_record =
                read_node_profile_json(&profile).context("could not read NodeProfileV1 JSON")?;
            let node_id = profile_record.descriptor().node_id().to_owned();
            let publication = ProfilePublicationV1::new(&namespace, node_id, profile_record)
                .context("profile publication is invalid")?;
            append_profile_to_store(&mut store, publication, now, &signer, &trust_record, policy)
                .context("registry key is not authorized to publish this profile")?;
            verify_registry_store(&store, &trust_record, now, policy)
                .context("updated registry did not verify")?;
            atomic_write_registry_store(&state, &store)
                .context("could not atomically update registry state")?;
            println!(
                "published profile namespace={namespace} state={}",
                state.display()
            );
        }
        Command::Verify {
            state,
            trust,
            allow_stale_profiles,
        } => {
            let verified = verify_paths(&state, &trust, allow_stale_profiles)?;
            println!(
                "verified snapshots={} profiles={}",
                verified.verified_snapshots(),
                verified.profiles().len()
            );
        }
        Command::List {
            state,
            trust,
            allow_stale_profiles,
        } => {
            let verified = verify_paths(&state, &trust, allow_stale_profiles)?;
            for (key, profile) in verified.profiles() {
                println!(
                    "{}\t{}\tgeneration={}\texpires_at_ms={}\tstale={}\tdescriptor={}",
                    key.namespace(),
                    key.node_id(),
                    profile.publication().profile().profile_generation().get(),
                    profile.expires_at_ms(),
                    profile.is_stale(),
                    profile
                        .publication()
                        .profile()
                        .descriptor_digest()
                        .context("could not digest target descriptor")?
                );
            }
        }
        Command::Export {
            state,
            trust,
            output,
            allow_stale_profiles,
        } => {
            verify_paths(&state, &trust, allow_stale_profiles)?;
            let store = read_registry_store(&state).context("could not read registry state")?;
            export_registry_store(&store, &output).context("could not export registry")?;
            println!("exported registry to {}", output.display());
        }
        Command::Import {
            state,
            trust,
            input,
            allow_stale_profiles,
        } => {
            let verified = import_registry_store(
                &state,
                &input,
                &trust,
                unix_millis()?,
                staleness_policy(allow_stale_profiles),
            )
            .context("registry import was rejected")?;
            println!(
                "imported registry snapshots={} profiles={}",
                verified.verified_snapshots(),
                verified.profiles().len()
            );
        }
    }
    Ok(())
}

fn verify_paths(
    state: &PathBuf,
    trust: &PathBuf,
    allow_stale_profiles: bool,
) -> Result<o_lang::registry::VerifiedRegistryV1> {
    let store = read_registry_store(state).context("could not read registry state")?;
    let trust = read_registry_trust(trust).context("could not read trust file")?;
    verify_registry_store(
        &store,
        &trust,
        unix_millis()?,
        staleness_policy(allow_stale_profiles),
    )
    .context("registry verification failed")
}

fn staleness_policy(allow: bool) -> ProfileStalenessPolicyV1 {
    if allow {
        ProfileStalenessPolicyV1::AllowExpired
    } else {
        ProfileStalenessPolicyV1::Reject
    }
}

fn unix_millis() -> Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    u64::try_from(duration.as_millis()).context("Unix time does not fit u64 milliseconds")
}

fn default_host_abi() -> String {
    if cfg!(target_os = "macos") {
        "darwin".to_owned()
    } else if cfg!(target_env = "musl") {
        "musl".to_owned()
    } else if cfg!(target_env = "msvc") {
        "msvc".to_owned()
    } else {
        "gnu".to_owned()
    }
}

fn parse_capability(value: &str) -> Result<CapabilityAtomV1> {
    let (coordinate, level) = value
        .rsplit_once('@')
        .with_context(|| format!("capability `{value}` must use namespace/name@level"))?;
    let (namespace, name) = coordinate
        .split_once('/')
        .with_context(|| format!("capability `{value}` must use namespace/name@level"))?;
    let level = level
        .parse::<u32>()
        .with_context(|| format!("capability `{value}` has an invalid level"))?;
    CapabilityAtomV1::new(CapabilityKeyV1::new(namespace, name)?, level)
        .context("invalid capability")
}

fn discover_backend_implementations(
    names: &[String],
    runtime_binary: Option<&Path>,
    shim_dir: Option<&Path>,
) -> Result<Vec<BackendImplementationIdV1>> {
    let registry = BackendRegistry::global();
    let mut output = Vec::with_capacity(names.len());
    for requested in names {
        let spec = registry
            .get(requested)
            .with_context(|| format!("unknown backend `{requested}`"))?;
        let adapter_path = match spec.adapter {
            BackendAdapterKind::Inline | BackendAdapterKind::NativeRust => {
                resolve_runtime_binary(runtime_binary)?
            }
            BackendAdapterKind::LegacyPythonShim => {
                let directory = shim_dir.context(
                    "--shim-dir is required when profiling a legacy Python shim backend",
                )?;
                let path = registry.resolve_shim_path(directory, spec.name);
                if !path.is_file() {
                    bail!(
                        "backend `{}` resolved missing shim `{}`",
                        spec.name,
                        path.display()
                    );
                }
                path
            }
        };
        let adapter_path = adapter_path
            .canonicalize()
            .with_context(|| format!("could not canonicalize `{}`", adapter_path.display()))?;
        let adapter_sha256 = sha256_file(&adapter_path)?;
        let adapter_artifact =
            ArtifactId::from_sha256(&adapter_sha256).context("invalid adapter artifact digest")?;
        let executable_set = discover_executable_set(registry, spec.name, &adapter_path)?;
        let backend_specification = SemanticDigestV1::from_sha256(
            registry
                .specification_sha256(spec.name)
                .context("backend specification digest is unavailable")?,
        )
        .context("invalid backend specification digest")?;
        let realization_material = serde_json::json!({
            "schema": "ostadix.local-realization/v1",
            "backend_specification": backend_specification.as_sha256(),
            "adapter_kind": spec.adapter.name(),
            "adapter_artifact": adapter_sha256,
            "executable_set": executable_set.as_sha256(),
            "protocol": "o-backend-cbor-v1",
        });
        let realization_pipeline = SemanticDigestV1::hash_bytes(
            "ostadix/registry/local-realization/v1",
            &serde_json::to_vec(&realization_material)?,
        );
        output.push(
            BackendImplementationIdV1::new(
                backend_specification,
                adapter_artifact,
                executable_set,
                "o-backend-cbor-v1",
                realization_pipeline,
            )
            .context("invalid discovered backend implementation")?,
        );
    }
    Ok(output)
}

fn resolve_runtime_binary(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_owned());
        }
        bail!("runtime binary `{}` is not a file", path.display());
    }
    which::which("O").context(
        "could not find O on PATH; pass --runtime-binary for inline/native backend profiling",
    )
}

fn discover_executable_set(
    registry: &BackendRegistry,
    backend: &str,
    adapter_path: &Path,
) -> Result<SemanticDigestV1> {
    let requirement = registry.runtime_requirements_for(backend);
    let mut artifacts = vec![(
        "adapter".to_owned(),
        adapter_path.display().to_string(),
        sha256_file(adapter_path)?,
    )];
    if !requirement.builtin {
        let selected = requirement
            .alternatives
            .iter()
            .find_map(|commands| {
                let paths = commands
                    .iter()
                    .map(|command| which::which(command).ok())
                    .collect::<Option<Vec<_>>>()?;
                Some(commands.iter().copied().zip(paths).collect::<Vec<_>>())
            })
            .with_context(|| {
                format!("backend `{backend}` has no complete installed executable alternative")
            })?;
        for (command, path) in selected {
            let path = path
                .canonicalize()
                .with_context(|| format!("could not canonicalize executable `{command}`"))?;
            artifacts.push((
                command.to_owned(),
                path.display().to_string(),
                sha256_file(&path)?,
            ));
        }
    }
    artifacts.sort();
    Ok(SemanticDigestV1::hash_bytes(
        "ostadix/registry/executable-set/v1",
        &serde_json::to_vec(&artifacts)?,
    ))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("could not open artifact `{}`", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("could not read artifact `{}`", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}
