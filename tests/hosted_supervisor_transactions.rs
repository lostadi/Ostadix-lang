//! Public-boundary transaction and compatibility tests for HostedSupervisor.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use o_lang::live_system::manifest::{
    payload_sha256, BuildManifest, HealthManifest, PackageDigest, PackageManifest, RuntimeManifest,
    ServiceManifest, PACKAGE_SCHEMA_V1,
};
use o_lang::live_system::protocol::{RUNTIME_PROGRAM_SCHEMA, RUNTIME_PROTOCOL};
use o_lang::live_system::store::PackageStore;
use o_lang::live_system::supervisor::{
    ActivationPolicy, HostedSupervisor, ServiceStatus, SupervisorConfig, HEALTH_PROTOCOL,
    HOSTED_RUNTIME_KIND, SERVICE_RIGHT_INVOKE,
};
use o_lang::value::OValue;
use serde_json::json;
use sha2::{Digest, Sha256};

struct WritableTempDir(tempfile::TempDir);

impl WritableTempDir {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create hosted-supervisor temporary root"))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for WritableTempDir {
    fn drop(&mut self) {
        restore_owner_permissions(self.0.path());
    }
}

struct InstalledPackage {
    digest: PackageDigest,
    name: String,
    services: Vec<String>,
}

struct PackageSpec<'a> {
    directory: &'a str,
    name: &'a str,
    architecture: &'a str,
    runtime_kind: &'a str,
    runtime_abi: &'a str,
    services: &'a [&'a str],
}

#[test]
fn reconstruction_is_read_only_preserves_generations_and_rotates_only_session() {
    let root = WritableTempDir::new();
    let state = root.path().join("state");
    let store = PackageStore::open(state.join("store")).unwrap();
    let package = install_package(
        &store,
        root.path(),
        PackageSpec {
            directory: "pair",
            name: "runtime.pair",
            architecture: std::env::consts::ARCH,
            runtime_kind: HOSTED_RUNTIME_KIND,
            runtime_abi: RUNTIME_PROTOCOL,
            services: &["world.alpha", "world.beta"],
        },
    );
    let active_set = state.join("active-set.json");
    let mut supervisor = open_supervisor(store.clone(), &active_set);
    supervisor.reconstruct().unwrap();
    let activated = supervisor.activate(&package.digest).unwrap();
    let activated_generations = generations(&activated);
    let stale_bearer = supervisor
        .service_capability("world.alpha", RUNTIME_PROTOCOL, [SERVICE_RIGHT_INVOKE])
        .unwrap();
    let durable_before = fs::read(&active_set).unwrap();
    #[cfg(unix)]
    let durable_identity_before = file_identity(&active_set);

    let reconstructed = supervisor.reconstruct().unwrap();
    assert_eq!(generations(&reconstructed), activated_generations);
    assert_eq!(fs::read(&active_set).unwrap(), durable_before);
    #[cfg(unix)]
    assert_eq!(file_identity(&active_set), durable_identity_before);
    assert!(supervisor
        .invoke(&stale_bearer, "echo", OValue::str_("stale"))
        .is_err());

    let restarted = supervisor.restart_service("world.alpha").unwrap();
    let after_restart = generations(&supervisor.services());
    assert_ne!(restarted.generation, activated_generations["world.alpha"]);
    assert_eq!(
        after_restart["world.beta"],
        activated_generations["world.beta"]
    );
    let durable_after_restart = fs::read(&active_set).unwrap();
    drop(supervisor);

    let mut recovered = open_supervisor(store, &active_set);
    assert_eq!(
        generations(&recovered.reconstruct().unwrap()),
        after_restart
    );
    assert_eq!(fs::read(&active_set).unwrap(), durable_after_restart);
}

#[test]
fn two_stale_supervisors_cannot_overwrite_one_another() {
    let root = WritableTempDir::new();
    let state = root.path().join("state");
    let store = PackageStore::open(state.join("store")).unwrap();
    let left_package = install_package(
        &store,
        root.path(),
        PackageSpec {
            directory: "left",
            name: "runtime.left",
            architecture: std::env::consts::ARCH,
            runtime_kind: HOSTED_RUNTIME_KIND,
            runtime_abi: RUNTIME_PROTOCOL,
            services: &["world.left"],
        },
    );
    let right_package = install_package(
        &store,
        root.path(),
        PackageSpec {
            directory: "right",
            name: "runtime.right",
            architecture: std::env::consts::ARCH,
            runtime_kind: HOSTED_RUNTIME_KIND,
            runtime_abi: RUNTIME_PROTOCOL,
            services: &["world.right"],
        },
    );
    let active_set = state.join("active-set.json");
    let mut left = open_supervisor(store.clone(), &active_set);
    let mut stale = open_supervisor(store.clone(), &active_set);
    left.reconstruct().unwrap();
    stale.reconstruct().unwrap();

    left.activate(&left_package.digest).unwrap();
    let error = stale.activate(&right_package.digest).unwrap_err();
    assert!(
        error.to_string().contains("active-set revision conflict"),
        "unexpected stale-writer error: {error:#}"
    );
    assert!(stale.active_digest(&right_package.name).is_none());

    let mut recovered = open_supervisor(store, &active_set);
    let services = recovered.reconstruct().unwrap();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].package, left_package.name);
    assert_eq!(services[0].service, left_package.services[0]);
}

#[test]
fn incompatible_hosted_runtimes_are_rejected_for_activation_and_reconstruction() {
    let root = WritableTempDir::new();
    let state = root.path().join("state");
    let store = PackageStore::open(state.join("store")).unwrap();
    let other_architecture = if std::env::consts::ARCH == "x86_64" {
        "aarch64"
    } else {
        "x86_64"
    };
    let wrong_arch = install_package(
        &store,
        root.path(),
        PackageSpec {
            directory: "wrong-arch",
            name: "runtime.wrong-arch",
            architecture: other_architecture,
            runtime_kind: HOSTED_RUNTIME_KIND,
            runtime_abi: RUNTIME_PROTOCOL,
            services: &["world.wrong-arch"],
        },
    );
    let wrong_kind = install_package(
        &store,
        root.path(),
        PackageSpec {
            directory: "wrong-kind",
            name: "runtime.wrong-kind",
            architecture: std::env::consts::ARCH,
            runtime_kind: "personality",
            runtime_abi: RUNTIME_PROTOCOL,
            services: &["world.wrong-kind"],
        },
    );
    let wrong_abi = install_package(
        &store,
        root.path(),
        PackageSpec {
            directory: "wrong-abi",
            name: "runtime.wrong-abi",
            architecture: std::env::consts::ARCH,
            runtime_kind: HOSTED_RUNTIME_KIND,
            runtime_abi: "ocore.runtime-service/v0",
            services: &["world.wrong-abi"],
        },
    );
    let active_set = state.join("active-set.json");
    let mut supervisor = open_supervisor(store.clone(), &active_set);
    supervisor.reconstruct().unwrap();

    assert!(supervisor
        .activate(&wrong_arch.digest)
        .unwrap_err()
        .to_string()
        .contains("incompatible with host architecture"));
    assert!(supervisor
        .activate(&wrong_kind.digest)
        .unwrap_err()
        .to_string()
        .contains("runtime kind"));
    assert!(supervisor
        .activate(&wrong_abi.digest)
        .unwrap_err()
        .to_string()
        .contains("runtime ABI"));

    let incompatible_record = json!({
        "schema": "ocore.hosted-active-set/v1",
        "revision": 7,
        "active": [{
            "package_name": wrong_arch.name,
            "digest": wrong_arch.digest.to_string(),
            "services": [{
                "name": wrong_arch.services[0],
                "protocol": RUNTIME_PROTOCOL,
                "generation": 11
            }]
        }],
        "rollback": []
    });
    fs::create_dir_all(active_set.parent().unwrap()).unwrap();
    fs::write(
        &active_set,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&incompatible_record).unwrap()
        ),
    )
    .unwrap();
    let durable_before = fs::read(&active_set).unwrap();
    let mut recovering = open_supervisor(store, &active_set);
    assert!(recovering
        .reconstruct()
        .unwrap_err()
        .to_string()
        .contains("incompatible with host architecture"));
    assert_eq!(fs::read(&active_set).unwrap(), durable_before);
}

fn install_package(
    store: &PackageStore,
    root: &Path,
    specification: PackageSpec<'_>,
) -> InstalledPackage {
    let package_root = root.join("packages").join(specification.directory);
    let payload = package_root.join("payload");
    let runtime_path = payload.join("bin/live.toml");
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime = format!(
        "schema = \"{RUNTIME_PROGRAM_SCHEMA}\"\nworld = \"{}\"\n\n\
         [health]\nstatus = \"healthy\"\n\n\
         [operations.echo]\nkind = \"identity\"\n",
        specification.name
    );
    fs::write(&runtime_path, runtime.as_bytes()).unwrap();
    let services = specification
        .services
        .iter()
        .map(|name| ServiceManifest {
            name: (*name).to_owned(),
            protocol: RUNTIME_PROTOCOL.to_owned(),
        })
        .collect::<Vec<_>>();
    let manifest = PackageManifest {
        schema: PACKAGE_SCHEMA_V1.to_owned(),
        name: specification.name.to_owned(),
        version: "1.0.0".to_owned(),
        architecture: specification.architecture.to_owned(),
        payload_sha256: payload_sha256(&payload).unwrap(),
        runtime: RuntimeManifest {
            kind: specification.runtime_kind.to_owned(),
            entry: "/bin/live.toml".to_owned(),
            abi: specification.runtime_abi.to_owned(),
        },
        services,
        capability_requests: Vec::new(),
        health: HealthManifest {
            protocol: HEALTH_PROTOCOL.to_owned(),
            timeout_ms: 1_000,
        },
        build: BuildManifest {
            source_sha256: hex::encode(Sha256::digest(runtime.as_bytes())),
            builder: "hosted-supervisor-transaction-test/v1".to_owned(),
        },
    };
    let stored = store
        .install(&manifest.canonical_toml().unwrap(), &payload)
        .unwrap();
    InstalledPackage {
        digest: stored.digest().clone(),
        name: specification.name.to_owned(),
        services: specification
            .services
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
    }
}

fn open_supervisor(store: PackageStore, active_set: &Path) -> HostedSupervisor {
    HostedSupervisor::new(
        store,
        active_set,
        PathBuf::from(env!("CARGO_BIN_EXE_o-live-host")),
        ActivationPolicy::new(),
        SupervisorConfig::default(),
    )
    .unwrap()
}

fn generations(statuses: &[ServiceStatus]) -> BTreeMap<String, u64> {
    statuses
        .iter()
        .map(|status| (status.service.clone(), status.generation))
        .collect()
}

#[cfg(unix)]
fn file_identity(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn restore_owner_permissions(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    make_owner_writable(path, &metadata);
    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            restore_owner_permissions(&entry.path());
        }
    }
}

#[cfg(unix)]
fn make_owner_writable(path: &Path, metadata: &fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;

    let required = if metadata.is_dir() { 0o700 } else { 0o600 };
    let mode = metadata.permissions().mode() | required;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn make_owner_writable(path: &Path, metadata: &fs::Metadata) {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(false);
    let _ = fs::set_permissions(path, permissions);
}
