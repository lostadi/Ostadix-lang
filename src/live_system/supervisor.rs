//! Hosted transactional supervisor for package-managed runtime worlds.
//!
//! This module is a semantic reference for the future native O-core control
//! plane. Each service is an ordinary local child process of a caller-supplied
//! worker executable. It does **not** provide O-core process/CSpace isolation,
//! kernel IPC, or enforcement of host syscall isolation. The useful contract
//! here is narrower: content-addressed packages, default-deny activation,
//! staged publication, live bearer capabilities, bounded calls, composition,
//! restart, rollback, and boot reconstruction. The directory containing the
//! active-set file is same-user trusted control-plane authority; immutable
//! package verification does not authenticate a hostile state directory.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::manifest::{PackageDigest, PackageManifest};
use super::protocol::{RuntimeRequest, RuntimeResponse, RUNTIME_MAX_FRAME_LEN, RUNTIME_PROTOCOL};
use super::store::{PackageStore, StoredPackage};
use crate::capability::fresh_bearer_identity;
use crate::value::{CapabilityKind, OValue, RuntimeBoundary};
use crate::wire;

/// Strict schema for the durable hosted active-set record.
pub const ACTIVE_SET_SCHEMA: &str = "ocore.hosted-active-set/v1";
/// The only authority currently issued by a hosted service endpoint.
pub const SERVICE_RIGHT_INVOKE: &str = "invoke";
/// Health protocol implemented by the fixed worker transport.
pub const HEALTH_PROTOCOL: &str = "ocore.health/v1";
/// The only runtime implementation understood by the hosted worker protocol.
pub const HOSTED_RUNTIME_KIND: &str = "native_test_runtime";

const MAX_ACTIVE_SET_BYTES: u64 = 1024 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CONFIGURED_VALUE_BYTES: usize = 512 * 1024;
pub const MAX_ACTIVE_PACKAGES: usize = 64;
pub const MAX_ACTIVE_SERVICES: usize = 256;
pub const MAX_ROLLBACK_PACKAGES: usize = 64;
pub const MAX_LIVE_BEARERS: usize = 4096;
pub const MAX_COMPOSITION_STEPS: usize = 256;
pub const MAX_OPERATION_BYTES: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PolicyRequestKey {
    package: String,
    kind: String,
    purpose: String,
}

/// Explicit activation grants. An empty policy denies every requested
/// capability; there are no wildcard or ambient grants.
#[derive(Debug, Clone, Default)]
pub struct ActivationPolicy {
    grants: BTreeMap<PolicyRequestKey, BTreeSet<String>>,
}

impl ActivationPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant an exact package/request tuple a bounded set of rights.
    pub fn allow_request<I, S>(
        &mut self,
        package: impl Into<String>,
        kind: impl Into<String>,
        purpose: impl Into<String>,
        rights: I,
    ) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let key = PolicyRequestKey {
            package: package.into(),
            kind: kind.into(),
            purpose: purpose.into(),
        };
        if key.package.is_empty() || key.kind.is_empty() || key.purpose.is_empty() {
            bail!("activation policy keys must be non-empty");
        }
        let rights = rights
            .into_iter()
            .map(|right| right.as_ref().to_owned())
            .collect::<BTreeSet<_>>();
        if rights.is_empty() || rights.iter().any(String::is_empty) {
            bail!("activation policy grants require non-empty rights");
        }
        self.grants.insert(key, rights);
        Ok(())
    }

    /// Fail closed unless every manifest request is covered by an exact grant.
    pub fn authorize(&self, manifest: &PackageManifest) -> Result<()> {
        manifest.validate().context("invalid package manifest")?;
        for request in &manifest.capability_requests {
            let key = PolicyRequestKey {
                package: manifest.name.clone(),
                kind: request.kind.clone(),
                purpose: request.purpose.clone(),
            };
            let allowed = self.grants.get(&key).ok_or_else(|| {
                anyhow!(
                    "activation policy denies package `{}` request `{}` for `{}`",
                    manifest.name,
                    request.kind,
                    request.purpose
                )
            })?;
            for right in &request.rights {
                if !allowed.contains(right) {
                    bail!(
                        "activation policy denies `{right}` for package `{}` request `{}` for `{}`",
                        manifest.name,
                        request.kind,
                        request.purpose
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub health_timeout_ceiling: Duration,
    pub invoke_timeout: Duration,
    pub max_value_bytes: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            health_timeout_ceiling: Duration::from_secs(5),
            invoke_timeout: Duration::from_secs(5),
            max_value_bytes: 64 * 1024,
        }
    }
}

impl SupervisorConfig {
    fn validate(&self) -> Result<()> {
        if self.health_timeout_ceiling.is_zero() || self.health_timeout_ceiling > MAX_TIMEOUT {
            bail!("health timeout ceiling must be between 1ns and 60s");
        }
        if self.invoke_timeout.is_zero() || self.invoke_timeout > MAX_TIMEOUT {
            bail!("invoke timeout must be between 1ns and 60s");
        }
        if self.max_value_bytes == 0 || self.max_value_bytes > MAX_CONFIGURED_VALUE_BYTES {
            bail!("max value bytes must be between 1 and {MAX_CONFIGURED_VALUE_BYTES}");
        }
        Ok(())
    }
}

/// One persistable structural-data step in a cross-world computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionStep {
    pub service: String,
    pub protocol: String,
    pub operation: String,
}

impl CompositionStep {
    pub fn new(
        service: impl Into<String>,
        protocol: impl Into<String>,
        operation: impl Into<String>,
    ) -> Self {
        Self {
            service: service.into(),
            protocol: protocol.into(),
            operation: operation.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceStatus {
    pub package: String,
    pub digest: String,
    pub service: String,
    pub protocol: String,
    pub generation: u64,
    pub world: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedActiveSet {
    schema: String,
    /// Monotonic compare-and-swap token for durable mutations. Revision zero
    /// also admits active sets written before this field was introduced.
    #[serde(default)]
    revision: u64,
    active: Vec<PersistedPackage>,
    rollback: Vec<PersistedPackage>,
}

impl PersistedActiveSet {
    fn empty() -> Self {
        Self {
            schema: ACTIVE_SET_SCHEMA.to_owned(),
            revision: 0,
            active: Vec::new(),
            rollback: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPackage {
    package_name: String,
    digest: String,
    services: Vec<PersistedService>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedService {
    name: String,
    protocol: String,
    generation: u64,
}

struct WorkerExchange {
    request: RuntimeRequest,
    reply: Sender<std::result::Result<RuntimeResponse, String>>,
}

struct ChildRuntime {
    child: Child,
    requests: Sender<WorkerExchange>,
    terminated: bool,
}

impl ChildRuntime {
    fn spawn(
        worker_executable: &Path,
        package_root: &Path,
        entry: &str,
        service: &str,
    ) -> Result<Self> {
        let mut command = Command::new(worker_executable);
        command
            .arg("__worker")
            .arg("--package-root")
            .arg(package_root)
            .arg("--entry")
            .arg(entry)
            .arg("--service")
            .arg(service)
            .current_dir(package_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to start hosted worker {} for service `{service}`",
                worker_executable.display()
            )
        })?;

        let stdin = child.stdin.take().context("worker stdin was not piped")?;
        let stdout = child.stdout.take().context("worker stdout was not piped")?;
        let (requests, receiver) = mpsc::channel();
        if let Err(error) = thread::Builder::new()
            .name(format!("o-live-{service}"))
            .spawn(move || worker_exchange_loop(stdin, stdout, receiver))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("failed to start hosted worker I/O thread");
        }
        Ok(Self {
            child,
            requests,
            terminated: false,
        })
    }

    fn request(&mut self, request: RuntimeRequest, timeout: Duration) -> Result<RuntimeResponse> {
        if self.terminated {
            bail!("hosted worker has already been terminated");
        }
        if self
            .exited_without_reaping()
            .context("failed to inspect hosted worker")?
        {
            self.terminate();
            bail!("hosted worker exited before request");
        }
        let (reply, response) = mpsc::channel();
        if self
            .requests
            .send(WorkerExchange { request, reply })
            .is_err()
        {
            self.terminate();
            bail!("hosted worker I/O channel is closed");
        }
        match response.recv_timeout(timeout) {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(message)) => {
                self.terminate();
                bail!("hosted worker protocol failed: {message}")
            }
            Err(RecvTimeoutError::Timeout) => {
                self.terminate();
                bail!("hosted worker exceeded its bounded timeout of {timeout:?}")
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.terminate();
                bail!("hosted worker disconnected before replying")
            }
        }
    }

    fn has_exited(&mut self) -> Result<bool> {
        if self.terminated {
            return Ok(true);
        }
        let exited = self
            .exited_without_reaping()
            .context("failed to inspect hosted worker")?;
        if exited {
            // Observe with WNOWAIT, then finalize the process group while the
            // unreaped leader still pins its pid/pgid identity against reuse.
            self.terminate();
        }
        Ok(exited)
    }

    #[cfg(unix)]
    fn exited_without_reaping(&mut self) -> io::Result<bool> {
        loop {
            // SAFETY: waitid initializes siginfo for this exact child. WNOWAIT
            // observes an exited leader without reaping it, so terminate can
            // still signal the original process group without a pid-reuse
            // window.
            let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let status = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    &mut information,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if status == 0 {
                // SAFETY: a successful waitid initialized the siginfo object;
                // si_pid is zero when WNOHANG found no waitable child state.
                return Ok(unsafe { information.si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    #[cfg(not(unix))]
    fn exited_without_reaping(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        // Set this before issuing any signal or wait so Drop and error paths
        // cannot target a subsequently reused pid/process-group id.
        self.terminated = true;
        #[cfg(unix)]
        {
            let process_group = self.child.id();
            if process_group <= i32::MAX as u32 {
                // SAFETY: a successful Unix spawn above placed the worker in a
                // fresh process group whose id is its child pid. A negative
                // pid targets that group, not the supervisor's group.
                unsafe {
                    libc::kill(-(process_group as i32), libc::SIGKILL);
                }
            }
        }
        // Also target the direct, still-unreaped child. This is the non-Unix
        // fallback and covers a Unix worker that deliberately escaped its
        // original group. Do not call try_wait before either signal: reaping
        // first would permit the numeric pid/pgid to be reused.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildRuntime {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn worker_exchange_loop(
    mut stdin: ChildStdin,
    mut stdout: ChildStdout,
    receiver: Receiver<WorkerExchange>,
) {
    while let Ok(exchange) = receiver.recv() {
        let result = (|| -> Result<RuntimeResponse> {
            wire::write_frame_with_max(&mut stdin, &exchange.request, RUNTIME_MAX_FRAME_LEN)?;
            wire::read_frame_with_max(&mut stdout, RUNTIME_MAX_FRAME_LEN)?
                .context("hosted worker closed stdout")
        })()
        .map_err(|error| error.to_string());
        let failed = result.is_err();
        let _ = exchange.reply.send(result);
        if failed {
            return;
        }
    }
}

struct ActiveService {
    protocol: String,
    generation: u64,
    world: String,
    runtime: ChildRuntime,
}

struct ActivePackage {
    record: PersistedPackage,
    services: BTreeMap<String, ActiveService>,
}

struct RollbackRoot {
    record: PersistedPackage,
    runtime: Option<ActivePackage>,
}

#[derive(Default)]
struct SupervisorState {
    active: BTreeMap<String, ActivePackage>,
    rollback: BTreeMap<String, RollbackRoot>,
    service_owners: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct BearerBinding {
    session: String,
    service: String,
    protocol: String,
    generation: u64,
    rights: BTreeSet<String>,
}

/// Hosted reference control plane. All mutation requires exclusive access, so
/// a staged package becomes observable only at the final in-process commit.
pub struct HostedSupervisor {
    store: PackageStore,
    active_set_path: PathBuf,
    worker_executable: PathBuf,
    policy: ActivationPolicy,
    config: SupervisorConfig,
    session: String,
    next_health_nonce: u64,
    observed_revision: Option<u64>,
    state: SupervisorState,
    bearers: HashMap<String, BearerBinding>,
}

impl HostedSupervisor {
    pub fn new(
        store: PackageStore,
        active_set_path: impl Into<PathBuf>,
        worker_executable: impl Into<PathBuf>,
        policy: ActivationPolicy,
        config: SupervisorConfig,
    ) -> Result<Self> {
        config.validate()?;
        let worker_executable = worker_executable.into();
        if worker_executable.as_os_str().is_empty() {
            bail!("worker executable path must not be empty");
        }
        Ok(Self {
            store,
            active_set_path: active_set_path.into(),
            worker_executable,
            policy,
            config,
            session: fresh_bearer_identity("o-live-host-session")?,
            next_health_nonce: 1,
            observed_revision: None,
            state: SupervisorState::default(),
            bearers: HashMap::new(),
        })
    }

    /// Rebuild the complete published set from immutable package digests.
    /// Every active worker is staged and health-checked before one in-memory
    /// commit. Durable generations are preserved exactly; only the fresh
    /// process-local session invalidates capabilities from the prior world.
    /// Reconstruction never rewrites the active-set file.
    pub fn reconstruct(&mut self) -> Result<Vec<ServiceStatus>> {
        let persisted =
            read_active_set(&self.active_set_path)?.unwrap_or_else(PersistedActiveSet::empty);
        validate_active_set_shape(&persisted)?;

        let fresh_session = fresh_bearer_identity("o-live-host-session")?;
        let mut staged = BTreeMap::new();
        let mut owners = BTreeMap::new();

        for record in &persisted.active {
            let stored = self.verify_record(record)?;
            validate_hosted_compatibility(stored.manifest())?;
            self.policy.authorize(stored.manifest())?;
            self.validate_health_manifest(stored.manifest())?;
            let generations = record
                .services
                .iter()
                .map(|service| (service.name.clone(), service.generation))
                .collect::<BTreeMap<_, _>>();
            let package = self.stage_package(&stored, &generations)?;
            for service in package.services.keys() {
                if let Some(owner) = owners.insert(service.clone(), record.package_name.clone()) {
                    bail!(
                        "service `{service}` is declared by both `{owner}` and `{}`",
                        record.package_name
                    );
                }
            }
            staged.insert(record.package_name.clone(), package);
        }

        let mut rollback = BTreeMap::new();
        for record in &persisted.rollback {
            let stored = self.verify_record(record)?;
            validate_hosted_compatibility(stored.manifest())?;
            self.policy.authorize(stored.manifest())?;
            self.validate_health_manifest(stored.manifest())?;
            rollback.insert(
                record.package_name.clone(),
                RollbackRoot {
                    record: record.clone(),
                    runtime: None,
                },
            );
        }

        self.state = SupervisorState {
            active: staged,
            rollback,
            service_owners: owners,
        };
        self.observed_revision = Some(persisted.revision);
        self.session = fresh_session;
        self.bearers.clear();
        Ok(self.services())
    }

    /// Verify, authorize, start, and health-check every service before
    /// publishing the package as one activation transaction.
    pub fn activate(&mut self, digest: &PackageDigest) -> Result<Vec<ServiceStatus>> {
        let observed_revision = self.mutation_revision()?;
        let stored = self
            .store
            .verify(digest)
            .with_context(|| format!("failed to verify package {digest}"))?;
        validate_hosted_compatibility(stored.manifest())?;
        self.policy.authorize(stored.manifest())?;
        self.validate_health_manifest(stored.manifest())?;
        let package_name = stored.manifest().name.clone();

        if self
            .state
            .active
            .get(&package_name)
            .is_some_and(|active| active.record.digest == digest.to_string())
        {
            ensure_active_set_revision(&self.active_set_path, observed_revision)?;
            return Ok(self.package_statuses(&package_name));
        }

        let replacing = self.state.active.contains_key(&package_name);
        if !replacing && self.state.active.len() >= MAX_ACTIVE_PACKAGES {
            bail!("active package limit of {MAX_ACTIVE_PACKAGES} has been reached");
        }
        let retained_service_count = self
            .state
            .active
            .iter()
            .filter(|(name, _)| *name != &package_name)
            .map(|(_, package)| package.services.len())
            .sum::<usize>();
        let next_service_count = retained_service_count
            .checked_add(stored.manifest().services.len())
            .context("active service count overflowed")?;
        if next_service_count > MAX_ACTIVE_SERVICES {
            bail!("activation would exceed active service limit of {MAX_ACTIVE_SERVICES}");
        }

        for service in &stored.manifest().services {
            if let Some(owner) = self.state.service_owners.get(&service.name) {
                if owner != &package_name {
                    bail!(
                        "service `{}` is already published by package `{owner}`",
                        service.name
                    );
                }
            }
        }

        let mut generation = self.max_generation();
        let generations = stored
            .manifest()
            .services
            .iter()
            .map(|service| {
                generation = checked_next_generation(generation)?;
                Ok((service.name.clone(), generation))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let staged = self.stage_package(&stored, &generations)?;

        let mut active_records = self.active_records();
        active_records.insert(package_name.clone(), staged.record.clone());
        let mut rollback_records = self.rollback_records();
        if let Some(current) = self.state.active.get(&package_name) {
            rollback_records.insert(package_name.clone(), current.record.clone());
        }
        self.observed_revision = Some(compare_exchange_active_set(
            &self.active_set_path,
            observed_revision,
            &persisted_from_records(&active_records, &rollback_records),
        )?);

        let prior = self.state.active.insert(package_name.clone(), staged);
        if let Some(prior) = prior {
            self.state.rollback.insert(
                package_name.clone(),
                RollbackRoot {
                    record: prior.record.clone(),
                    runtime: Some(prior),
                },
            );
        }
        self.state.service_owners = owners_for_active(&self.state.active)?;
        let affected = package_service_union(
            self.state.active.get(&package_name),
            self.state.rollback.get(&package_name),
        );
        self.revoke_services(&affected);
        Ok(self.package_statuses(&package_name))
    }

    /// Swap an active package with its retained prior healthy generation.
    pub fn rollback(&mut self, package_name: &str) -> Result<Vec<ServiceStatus>> {
        let observed_revision = self.mutation_revision()?;
        let mut root = self
            .state
            .rollback
            .remove(package_name)
            .with_context(|| format!("package `{package_name}` has no rollback root"))?;
        let retained_service_count = self
            .state
            .active
            .iter()
            .filter(|(name, _)| name.as_str() != package_name)
            .map(|(_, package)| package.services.len())
            .sum::<usize>();
        let next_service_count = retained_service_count.saturating_add(root.record.services.len());
        if next_service_count > MAX_ACTIVE_SERVICES {
            self.state.rollback.insert(package_name.to_owned(), root);
            bail!("rollback would exceed active service limit of {MAX_ACTIVE_SERVICES}");
        }
        if let Some((service, owner)) = root.record.services.iter().find_map(|service| {
            self.state
                .service_owners
                .get(&service.name)
                .filter(|owner| owner.as_str() != package_name)
                .map(|owner| (service.name.clone(), owner.clone()))
        }) {
            self.state.rollback.insert(package_name.to_owned(), root);
            bail!("rollback service `{service}` is already published by package `{owner}`");
        }
        let result = self.prepare_rollback(&mut root);
        let replacement = match result {
            Ok(replacement) => replacement,
            Err(error) => {
                self.state.rollback.insert(package_name.to_owned(), root);
                return Err(error);
            }
        };

        let conflict = replacement.services.keys().find_map(|service| {
            self.state
                .service_owners
                .get(service)
                .filter(|owner| owner.as_str() != package_name)
                .map(|owner| (service.clone(), owner.clone()))
        });
        if let Some((service, owner)) = conflict {
            root.record = replacement.record.clone();
            root.runtime = Some(replacement);
            self.state.rollback.insert(package_name.to_owned(), root);
            bail!("rollback service `{service}` is already published by package `{owner}`");
        }

        let current = self
            .state
            .active
            .get(package_name)
            .with_context(|| format!("package `{package_name}` is not active"))?;
        let mut active_records = self.active_records();
        active_records.insert(package_name.to_owned(), replacement.record.clone());
        let mut rollback_records = self.rollback_records();
        rollback_records.insert(package_name.to_owned(), current.record.clone());
        let committed_revision = match compare_exchange_active_set(
            &self.active_set_path,
            observed_revision,
            &persisted_from_records(&active_records, &rollback_records),
        ) {
            Ok(revision) => revision,
            Err(error) => {
                root.record = replacement.record.clone();
                root.runtime = Some(replacement);
                self.state.rollback.insert(package_name.to_owned(), root);
                return Err(error);
            }
        };
        self.observed_revision = Some(committed_revision);

        let current = self
            .state
            .active
            .insert(package_name.to_owned(), replacement)
            .expect("active package was checked above");
        self.state.rollback.insert(
            package_name.to_owned(),
            RollbackRoot {
                record: current.record.clone(),
                runtime: Some(current),
            },
        );
        self.state.service_owners = owners_for_active(&self.state.active)?;
        let affected = package_service_union(
            self.state.active.get(package_name),
            self.state.rollback.get(package_name),
        );
        self.revoke_services(&affected);
        Ok(self.package_statuses(package_name))
    }

    /// Restart one service with a fresh process and generation. Other services
    /// and all capabilities bound to them remain live.
    pub fn restart_service(&mut self, service: &str) -> Result<ServiceStatus> {
        let observed_revision = self.mutation_revision()?;
        let package_name = self
            .state
            .service_owners
            .get(service)
            .cloned()
            .with_context(|| format!("service `{service}` is not active"))?;
        let digest = parse_persisted_digest(
            &self
                .state
                .active
                .get(&package_name)
                .expect("service owner refers to active package")
                .record
                .digest,
        )?;
        let stored = self.store.verify(&digest)?;
        validate_hosted_compatibility(stored.manifest())?;
        self.policy.authorize(stored.manifest())?;
        self.validate_health_manifest(stored.manifest())?;
        let declaration = stored
            .manifest()
            .services
            .iter()
            .find(|candidate| candidate.name == service)
            .with_context(|| format!("verified package no longer declares service `{service}`"))?;
        let generation = checked_next_generation(self.max_generation())?;
        let runtime = self.spawn_healthy_service(
            &stored,
            &declaration.name,
            &declaration.protocol,
            generation,
        )?;

        let mut next_record = self
            .state
            .active
            .get(&package_name)
            .expect("service owner refers to active package")
            .record
            .clone();
        let persisted = next_record
            .services
            .iter_mut()
            .find(|candidate| candidate.name == service)
            .expect("verified record contains declared service");
        persisted.generation = generation;
        let mut active_records = self.active_records();
        active_records.insert(package_name.clone(), next_record.clone());
        self.observed_revision = Some(compare_exchange_active_set(
            &self.active_set_path,
            observed_revision,
            &persisted_from_records(&active_records, &self.rollback_records()),
        )?);

        let package = self
            .state
            .active
            .get_mut(&package_name)
            .expect("service owner refers to active package");
        package.record = next_record;
        package.services.insert(service.to_owned(), runtime);
        self.revoke_services(&BTreeSet::from([service.to_owned()]));
        self.status_for(service)
    }

    /// Detect dead children and restart only those services.
    pub fn restart_crashed(&mut self) -> Result<Vec<String>> {
        self.mutation_revision()?;
        let mut crashed = Vec::new();
        for package in self.state.active.values_mut() {
            for (name, service) in &mut package.services {
                if service.runtime.has_exited()? {
                    crashed.push(name.clone());
                }
            }
        }
        for service in &crashed {
            self.restart_service(service)?;
        }
        Ok(crashed)
    }

    /// Resolve a service name to a private, process-local bearer authority.
    pub fn service_capability<I, S>(
        &mut self,
        service: &str,
        protocol: &str,
        rights: I,
    ) -> Result<OValue>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let status = self.status_for(service)?;
        if status.protocol != protocol {
            bail!(
                "service `{service}` speaks `{}`, not `{protocol}`",
                status.protocol
            );
        }
        let rights = rights
            .into_iter()
            .map(|right| right.as_ref().to_owned())
            .collect::<BTreeSet<_>>();
        if rights.is_empty() {
            bail!("service capability requires at least one right");
        }
        for right in &rights {
            if right != SERVICE_RIGHT_INVOKE {
                bail!("service `{service}` does not expose right `{right}`");
            }
        }
        ensure_bearer_capacity(self.bearers.len())?;
        let identity = loop {
            let candidate = fresh_bearer_identity("o-live-host-service")?;
            if !self.bearers.contains_key(&candidate) {
                break candidate;
            }
        };
        self.bearers.insert(
            identity.clone(),
            BearerBinding {
                session: self.session.clone(),
                service: service.to_owned(),
                protocol: protocol.to_owned(),
                generation: status.generation,
                rights: rights.clone(),
            },
        );
        let metadata = HashMap::from([
            ("live".into(), OValue::bool_(true)),
            ("service".into(), OValue::str_(service)),
            ("protocol".into(), OValue::str_(protocol)),
            (
                "generation".into(),
                OValue::str_(status.generation.to_string()),
            ),
            ("package".into(), OValue::str_(status.package)),
            ("digest".into(), OValue::str_(status.digest)),
            (
                "rights".into(),
                OValue::list(rights.iter().cloned().map(OValue::str_).collect()),
            ),
            ("session_bound".into(), OValue::bool_(true)),
        ]);
        Ok(OValue::capability(
            CapabilityKind::Service,
            identity,
            metadata,
        ))
    }

    /// Invoke through an exact live binding. Serialized metadata is ignored;
    /// only the private bearer table grants authority.
    pub fn invoke(
        &mut self,
        capability: &OValue,
        operation: &str,
        input: OValue,
    ) -> Result<OValue> {
        validate_structural_value(&input, self.config.max_value_bytes, "service input")?;
        validate_operation(operation)?;
        let OValue::Capability { kind, identity, .. } = capability else {
            bail!(
                "expected service capability, got {}",
                capability.type_name()
            );
        };
        if *kind != CapabilityKind::Service {
            bail!("expected service capability, got {}", kind.name());
        }
        let binding = self.bearers.get(identity).cloned().ok_or_else(|| {
            anyhow!("service capability is forged, revoked, stale, or from another supervisor")
        })?;
        if binding.session != self.session {
            bail!("service capability belongs to another supervisor session");
        }
        if !binding.rights.contains(SERVICE_RIGHT_INVOKE) {
            bail!("service capability lacks `{SERVICE_RIGHT_INVOKE}` right");
        }
        let current = self.status_for(&binding.service)?;
        if current.protocol != binding.protocol || current.generation != binding.generation {
            self.bearers.remove(identity);
            bail!("service capability generation or protocol is stale");
        }
        let service_name = binding.service.clone();
        let package_name = self
            .state
            .service_owners
            .get(&service_name)
            .cloned()
            .with_context(|| format!("service `{service_name}` is no longer active"))?;
        let response = {
            let active = self
                .state
                .active
                .get_mut(&package_name)
                .expect("service owner refers to active package");
            let service = active
                .services
                .get_mut(&service_name)
                .expect("service owner refers to active service");
            service.runtime.request(
                RuntimeRequest::Invoke {
                    service: service_name.clone(),
                    operation: operation.to_owned(),
                    input,
                },
                self.config.invoke_timeout,
            )
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                self.revoke_services(&BTreeSet::from([service_name.clone()]));
                return Err(error);
            }
        };
        let output = match response {
            RuntimeResponse::Result { value } => value,
            RuntimeResponse::Error { message } => bail!("service rejected request: {message}"),
            other => {
                self.invalidate_service(&service_name);
                bail!("service returned unexpected invocation response: {other:?}")
            }
        };
        if let Err(error) =
            validate_structural_value(&output, self.config.max_value_bytes, "service output")
        {
            self.invalidate_service(&service_name);
            return Err(error);
        }
        Ok(output)
    }

    /// Pipe persistable structural data through named services. Temporary
    /// capabilities are revoked after every step, including failed steps.
    pub fn compose(&mut self, steps: &[CompositionStep], mut value: OValue) -> Result<OValue> {
        validate_structural_value(&value, self.config.max_value_bytes, "composition input")?;
        validate_composition_count(steps.len())?;
        for step in steps {
            if step.service.is_empty() || step.protocol.is_empty() || step.operation.is_empty() {
                bail!("composition steps require service, protocol, and operation");
            }
            validate_operation(&step.operation)?;
            let capability =
                self.service_capability(&step.service, &step.protocol, [SERVICE_RIGHT_INVOKE])?;
            let identity = match &capability {
                OValue::Capability { identity, .. } => identity.clone(),
                _ => unreachable!("service_capability always returns a capability"),
            };
            let result = self.invoke(&capability, &step.operation, value);
            self.bearers.remove(&identity);
            value = result?;
        }
        Ok(value)
    }

    pub fn services(&self) -> Vec<ServiceStatus> {
        self.state
            .active
            .iter()
            .flat_map(|(package_name, package)| {
                package
                    .services
                    .iter()
                    .map(move |(name, service)| ServiceStatus {
                        package: package_name.clone(),
                        digest: package.record.digest.clone(),
                        service: name.clone(),
                        protocol: service.protocol.clone(),
                        generation: service.generation,
                        world: service.world.clone(),
                    })
            })
            .collect()
    }

    pub fn active_digest(&self, package_name: &str) -> Option<&str> {
        self.state
            .active
            .get(package_name)
            .map(|package| package.record.digest.as_str())
    }

    pub fn rollback_digest(&self, package_name: &str) -> Option<&str> {
        self.state
            .rollback
            .get(package_name)
            .map(|package| package.record.digest.as_str())
    }

    pub fn active_set_path(&self) -> &Path {
        &self.active_set_path
    }

    fn mutation_revision(&self) -> Result<u64> {
        self.observed_revision.context(
            "supervisor must reconstruct the durable active set before attempting a mutation",
        )
    }

    fn validate_health_manifest(&self, manifest: &PackageManifest) -> Result<()> {
        if manifest.health.protocol != HEALTH_PROTOCOL {
            bail!(
                "package `{}` health protocol `{}` is unsupported; expected `{HEALTH_PROTOCOL}`",
                manifest.name,
                manifest.health.protocol
            );
        }
        let timeout = Duration::from_millis(manifest.health.timeout_ms);
        if timeout > self.config.health_timeout_ceiling {
            bail!(
                "package `{}` health timeout {timeout:?} exceeds supervisor ceiling {:?}",
                manifest.name,
                self.config.health_timeout_ceiling
            );
        }
        Ok(())
    }

    fn stage_package(
        &mut self,
        stored: &StoredPackage,
        generations: &BTreeMap<String, u64>,
    ) -> Result<ActivePackage> {
        let manifest = stored.manifest();
        if generations.len() != manifest.services.len() {
            bail!("internal generation set does not match package services");
        }
        let mut services = BTreeMap::new();
        let mut persisted = Vec::new();
        for declaration in &manifest.services {
            let generation = *generations
                .get(&declaration.name)
                .context("missing staged service generation")?;
            let service = self.spawn_healthy_service(
                stored,
                &declaration.name,
                &declaration.protocol,
                generation,
            )?;
            persisted.push(PersistedService {
                name: declaration.name.clone(),
                protocol: declaration.protocol.clone(),
                generation,
            });
            services.insert(declaration.name.clone(), service);
        }
        persisted.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(ActivePackage {
            record: PersistedPackage {
                package_name: manifest.name.clone(),
                digest: stored.digest().to_string(),
                services: persisted,
            },
            services,
        })
    }

    fn spawn_healthy_service(
        &mut self,
        stored: &StoredPackage,
        service_name: &str,
        protocol: &str,
        generation: u64,
    ) -> Result<ActiveService> {
        let manifest = stored.manifest();
        let timeout = Duration::from_millis(manifest.health.timeout_ms);
        let mut runtime = ChildRuntime::spawn(
            &self.worker_executable,
            stored.payload_path(),
            &manifest.runtime.entry,
            service_name,
        )?;
        let nonce = self.next_nonce()?;
        let response = runtime
            .request(RuntimeRequest::Health { nonce }, timeout)
            .with_context(|| format!("health check failed for service `{service_name}`"))?;
        let world = match response {
            RuntimeResponse::Healthy {
                nonce: returned,
                world,
            } if returned == nonce => world,
            RuntimeResponse::Healthy {
                nonce: returned, ..
            } => {
                bail!("service `{service_name}` returned health nonce {returned}, expected {nonce}")
            }
            RuntimeResponse::Unhealthy {
                nonce: returned,
                message,
            } => bail!("service `{service_name}` was unhealthy at nonce {returned}: {message}"),
            other => bail!("service `{service_name}` returned invalid health response: {other:?}"),
        };
        Ok(ActiveService {
            protocol: protocol.to_owned(),
            generation,
            world,
            runtime,
        })
    }

    fn prepare_rollback(&mut self, root: &mut RollbackRoot) -> Result<ActivePackage> {
        let digest = parse_persisted_digest(&root.record.digest)?;
        let stored = self.store.verify(&digest)?;
        verify_record_matches(&root.record, &stored)?;
        validate_hosted_compatibility(stored.manifest())?;
        self.policy.authorize(stored.manifest())?;
        self.validate_health_manifest(stored.manifest())?;

        let mut candidate = root.runtime.take();
        let retained_is_healthy = if let Some(package) = candidate.as_mut() {
            self.health_existing_package(package, stored.manifest().health.timeout_ms)
                .is_ok()
        } else {
            false
        };
        if !retained_is_healthy {
            candidate = None;
        }

        let mut generation = self.max_generation();
        let generations = stored
            .manifest()
            .services
            .iter()
            .map(|service| {
                generation = checked_next_generation(generation)?;
                Ok((service.name.clone(), generation))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        if let Some(mut package) = candidate {
            for persisted in &mut package.record.services {
                let next = *generations
                    .get(&persisted.name)
                    .context("rollback generation is missing a service")?;
                persisted.generation = next;
                package
                    .services
                    .get_mut(&persisted.name)
                    .context("rollback runtime is missing a service")?
                    .generation = next;
            }
            root.record = package.record.clone();
            Ok(package)
        } else {
            let package = self.stage_package(&stored, &generations)?;
            root.record = package.record.clone();
            Ok(package)
        }
    }

    fn health_existing_package(
        &mut self,
        package: &mut ActivePackage,
        timeout_ms: u64,
    ) -> Result<()> {
        let timeout = Duration::from_millis(timeout_ms);
        for (name, service) in &mut package.services {
            let nonce = self.next_nonce()?;
            match service
                .runtime
                .request(RuntimeRequest::Health { nonce }, timeout)?
            {
                RuntimeResponse::Healthy {
                    nonce: returned,
                    world,
                } if returned == nonce => service.world = world,
                RuntimeResponse::Unhealthy { message, .. } => {
                    bail!("rollback service `{name}` is unhealthy: {message}")
                }
                response => bail!("rollback service `{name}` failed health: {response:?}"),
            }
        }
        Ok(())
    }

    fn next_nonce(&mut self) -> Result<u64> {
        let nonce = self.next_health_nonce;
        self.next_health_nonce = self
            .next_health_nonce
            .checked_add(1)
            .context("health nonce space exhausted")?;
        Ok(nonce)
    }

    fn verify_record(&self, record: &PersistedPackage) -> Result<StoredPackage> {
        let digest = parse_persisted_digest(&record.digest)?;
        let stored = self
            .store
            .verify(&digest)
            .with_context(|| format!("failed to verify active-set digest {}", record.digest))?;
        verify_record_matches(record, &stored)?;
        Ok(stored)
    }

    fn max_generation(&self) -> u64 {
        self.state
            .active
            .values()
            .flat_map(|package| package.record.services.iter())
            .chain(
                self.state
                    .rollback
                    .values()
                    .flat_map(|package| package.record.services.iter()),
            )
            .map(|service| service.generation)
            .max()
            .unwrap_or(0)
    }

    fn active_records(&self) -> BTreeMap<String, PersistedPackage> {
        self.state
            .active
            .iter()
            .map(|(name, package)| (name.clone(), package.record.clone()))
            .collect()
    }

    fn rollback_records(&self) -> BTreeMap<String, PersistedPackage> {
        self.state
            .rollback
            .iter()
            .map(|(name, package)| (name.clone(), package.record.clone()))
            .collect()
    }

    fn status_for(&self, service_name: &str) -> Result<ServiceStatus> {
        let package_name = self
            .state
            .service_owners
            .get(service_name)
            .with_context(|| format!("service `{service_name}` is not active"))?;
        let package = self
            .state
            .active
            .get(package_name)
            .expect("service owner refers to active package");
        let service = package
            .services
            .get(service_name)
            .expect("service owner refers to active service");
        Ok(ServiceStatus {
            package: package_name.clone(),
            digest: package.record.digest.clone(),
            service: service_name.to_owned(),
            protocol: service.protocol.clone(),
            generation: service.generation,
            world: service.world.clone(),
        })
    }

    fn package_statuses(&self, package_name: &str) -> Vec<ServiceStatus> {
        self.state
            .active
            .get(package_name)
            .into_iter()
            .flat_map(|package| package.services.keys())
            .filter_map(|service| self.status_for(service).ok())
            .collect()
    }

    fn revoke_services(&mut self, services: &BTreeSet<String>) {
        self.bearers
            .retain(|_, binding| !services.contains(&binding.service));
    }

    fn invalidate_service(&mut self, service_name: &str) {
        if let Some(package_name) = self.state.service_owners.get(service_name).cloned() {
            if let Some(service) = self
                .state
                .active
                .get_mut(&package_name)
                .and_then(|package| package.services.get_mut(service_name))
            {
                service.runtime.terminate();
            }
        }
        self.revoke_services(&BTreeSet::from([service_name.to_owned()]));
    }
}

fn validate_structural_value(value: &OValue, max_bytes: usize, label: &str) -> Result<()> {
    if value.runtime_boundary() != RuntimeBoundary::Pure || !value.is_boot_persistable() {
        bail!("{label} must be pure, persistable structural OValue data");
    }
    // Stream into a counting sink so rejecting a caller-supplied oversized
    // tree never requires first allocating another complete encoding of it.
    let mut sink = BoundedValueSink::new(max_bytes);
    serde_json::to_writer(&mut sink, value)
        .map_err(|_| anyhow!("{label} exceeds or cannot satisfy its {max_bytes}-byte bound"))?;
    Ok(())
}

struct BoundedValueSink {
    remaining: usize,
}

impl BoundedValueSink {
    fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }
}

impl Write for BoundedValueSink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "structural value exceeds configured bound",
            ));
        }
        self.remaining -= buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn validate_operation(operation: &str) -> Result<()> {
    if operation.is_empty()
        || operation.len() > MAX_OPERATION_BYTES
        || !operation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!(
            "service operation must be a portable identifier of at most {MAX_OPERATION_BYTES} bytes"
        );
    }
    Ok(())
}

fn ensure_bearer_capacity(current: usize) -> Result<()> {
    if current >= MAX_LIVE_BEARERS {
        bail!("live service bearer limit of {MAX_LIVE_BEARERS} has been reached");
    }
    Ok(())
}

fn validate_composition_count(count: usize) -> Result<()> {
    if count > MAX_COMPOSITION_STEPS {
        bail!("composition exceeds step limit of {MAX_COMPOSITION_STEPS}");
    }
    Ok(())
}

fn checked_next_generation(generation: u64) -> Result<u64> {
    generation
        .checked_add(1)
        .context("service generation space exhausted")
}

fn parse_persisted_digest(value: &str) -> Result<PackageDigest> {
    let hex = value
        .strip_prefix("sha256:")
        .context("active-set digest must use explicit sha256:<hex> form")?;
    PackageDigest::from_hex(hex).context("invalid active-set package digest")
}

fn verify_record_matches(record: &PersistedPackage, stored: &StoredPackage) -> Result<()> {
    let manifest = stored.manifest();
    if record.package_name != manifest.name {
        bail!(
            "active-set package name `{}` does not match verified manifest `{}`",
            record.package_name,
            manifest.name
        );
    }
    if record.digest != stored.digest().to_string() {
        bail!("active-set digest does not match verified package identity");
    }
    let declared = manifest
        .services
        .iter()
        .map(|service| (service.name.as_str(), service.protocol.as_str()))
        .collect::<BTreeMap<_, _>>();
    let persisted = record
        .services
        .iter()
        .map(|service| (service.name.as_str(), service.protocol.as_str()))
        .collect::<BTreeMap<_, _>>();
    if declared != persisted {
        bail!(
            "active-set service metadata for `{}` does not match its verified manifest",
            record.package_name
        );
    }
    Ok(())
}

fn validate_hosted_compatibility(manifest: &PackageManifest) -> Result<()> {
    if manifest.architecture != std::env::consts::ARCH {
        bail!(
            "package `{}` architecture `{}` is incompatible with host architecture `{}`",
            manifest.name,
            manifest.architecture,
            std::env::consts::ARCH
        );
    }
    if manifest.runtime.kind != HOSTED_RUNTIME_KIND {
        bail!(
            "package `{}` runtime kind `{}` is unsupported; expected `{HOSTED_RUNTIME_KIND}`",
            manifest.name,
            manifest.runtime.kind
        );
    }
    if manifest.runtime.abi != RUNTIME_PROTOCOL {
        bail!(
            "package `{}` runtime ABI `{}` is unsupported; expected `{RUNTIME_PROTOCOL}`",
            manifest.name,
            manifest.runtime.abi
        );
    }
    Ok(())
}

fn validate_active_set_shape(active_set: &PersistedActiveSet) -> Result<()> {
    if active_set.schema != ACTIVE_SET_SCHEMA {
        bail!(
            "unsupported active-set schema `{}`; expected `{ACTIVE_SET_SCHEMA}`",
            active_set.schema
        );
    }
    if active_set.active.len() > MAX_ACTIVE_PACKAGES {
        bail!("active set exceeds package limit of {MAX_ACTIVE_PACKAGES}");
    }
    if active_set.rollback.len() > MAX_ROLLBACK_PACKAGES {
        bail!("active set exceeds rollback-root limit of {MAX_ROLLBACK_PACKAGES}");
    }
    let active_service_count = active_set
        .active
        .iter()
        .try_fold(0usize, |count, package| {
            count.checked_add(package.services.len())
        })
        .context("active service count overflowed")?;
    if active_service_count > MAX_ACTIVE_SERVICES {
        bail!("active set exceeds service limit of {MAX_ACTIVE_SERVICES}");
    }
    let mut active_names = BTreeSet::new();
    for record in &active_set.active {
        validate_persisted_record(record)?;
        if !active_names.insert(record.package_name.as_str()) {
            bail!("duplicate active package `{}`", record.package_name);
        }
    }
    let mut rollback_names = BTreeSet::new();
    for record in &active_set.rollback {
        validate_persisted_record(record)?;
        if !rollback_names.insert(record.package_name.as_str()) {
            bail!("duplicate rollback package `{}`", record.package_name);
        }
        if !active_names.contains(record.package_name.as_str()) {
            bail!(
                "rollback package `{}` has no active counterpart",
                record.package_name
            );
        }
    }
    Ok(())
}

fn validate_persisted_record(record: &PersistedPackage) -> Result<()> {
    if record.package_name.is_empty() {
        bail!("active-set package name must not be empty");
    }
    parse_persisted_digest(&record.digest)?;
    let mut services = BTreeSet::new();
    for service in &record.services {
        if service.name.is_empty() || service.protocol.is_empty() || service.generation == 0 {
            bail!("active-set contains invalid service metadata");
        }
        if !services.insert(service.name.as_str()) {
            bail!("duplicate active-set service `{}`", service.name);
        }
    }
    Ok(())
}

fn owners_for_active(active: &BTreeMap<String, ActivePackage>) -> Result<BTreeMap<String, String>> {
    let mut owners = BTreeMap::new();
    for (package_name, package) in active {
        for service in package.services.keys() {
            if let Some(prior) = owners.insert(service.clone(), package_name.clone()) {
                bail!("service `{service}` is published by both `{prior}` and `{package_name}`");
            }
        }
    }
    Ok(owners)
}

fn package_service_union(
    active: Option<&ActivePackage>,
    rollback: Option<&RollbackRoot>,
) -> BTreeSet<String> {
    active
        .into_iter()
        .flat_map(|package| package.record.services.iter())
        .chain(
            rollback
                .into_iter()
                .flat_map(|package| package.record.services.iter()),
        )
        .map(|service| service.name.clone())
        .collect()
}

fn persisted_from_records(
    active: &BTreeMap<String, PersistedPackage>,
    rollback: &BTreeMap<String, PersistedPackage>,
) -> PersistedActiveSet {
    PersistedActiveSet {
        schema: ACTIVE_SET_SCHEMA.to_owned(),
        revision: 0,
        active: active.values().cloned().collect(),
        rollback: rollback.values().cloned().collect(),
    }
}

fn read_active_set(path: &Path) -> Result<Option<PersistedActiveSet>> {
    let file = match open_active_set_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to open active set {}", path.display()))
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect active set {}", path.display()))?;
    if !metadata.is_file() {
        bail!(
            "active set must be a regular non-symlink file: {}",
            path.display()
        );
    }
    if metadata.len() > MAX_ACTIVE_SET_BYTES {
        bail!("active set exceeds {MAX_ACTIVE_SET_BYTES} bytes");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_ACTIVE_SET_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read active set {}", path.display()))?;
    if bytes.len() as u64 > MAX_ACTIVE_SET_BYTES {
        bail!("active set exceeds {MAX_ACTIVE_SET_BYTES} bytes");
    }
    let active_set = serde_json::from_slice(&bytes).context("invalid active-set JSON")?;
    Ok(Some(active_set))
}

fn open_active_set_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

/// Lock only the compare-and-swap window. A supervisor deliberately does not
/// retain this lock across reconstruction and later mutation: the persisted
/// revision detects that stale interval without forcing all live worlds into
/// one process-wide critical section.
struct ActiveSetLock {
    file: File,
}

impl ActiveSetLock {
    fn acquire(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create active-set directory {}", parent.display())
        })?;
        let mut lock_name = OsString::from(".");
        lock_name.push(path.file_name().unwrap_or_else(|| OsStr::new("active-set")));
        lock_name.push(".lock");
        let lock_path = parent.join(lock_name);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options
            .open(&lock_path)
            .with_context(|| format!("failed to open active-set lock {}", lock_path.display()))?;
        let metadata = file.metadata().with_context(|| {
            format!("failed to inspect active-set lock {}", lock_path.display())
        })?;
        if !metadata.is_file() {
            bail!(
                "active-set lock must be a regular non-symlink file: {}",
                lock_path.display()
            );
        }
        #[cfg(unix)]
        loop {
            // SAFETY: `file` owns this descriptor for the dedicated lock inode
            // until ActiveSetLock is dropped.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).with_context(|| {
                    format!("failed to acquire active-set lock {}", lock_path.display())
                });
            }
        }
        #[cfg(not(unix))]
        bail!("hosted active-set mutation requires Unix process-shared file locking");
        #[cfg(unix)]
        Ok(Self { file })
    }
}

impl Drop for ActiveSetLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: this descriptor was locked by acquire and remains live.
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn durable_active_set_revision(path: &Path) -> Result<u64> {
    let active_set = read_active_set(path)?.unwrap_or_else(PersistedActiveSet::empty);
    validate_active_set_shape(&active_set)?;
    Ok(active_set.revision)
}

fn ensure_active_set_revision(path: &Path, expected_revision: u64) -> Result<()> {
    let _lock = ActiveSetLock::acquire(path)?;
    let actual_revision = durable_active_set_revision(path)?;
    if actual_revision != expected_revision {
        bail!(
            "active-set revision conflict: supervisor observed revision {expected_revision}, durable revision is {actual_revision}; reconstruct before retrying"
        );
    }
    Ok(())
}

fn compare_exchange_active_set(
    path: &Path,
    expected_revision: u64,
    next: &PersistedActiveSet,
) -> Result<u64> {
    let _lock = ActiveSetLock::acquire(path)?;
    let actual_revision = durable_active_set_revision(path)?;
    if actual_revision != expected_revision {
        bail!(
            "active-set revision conflict: supervisor observed revision {expected_revision}, durable revision is {actual_revision}; reconstruct before retrying"
        );
    }
    let committed_revision = checked_next_generation(expected_revision)
        .context("active-set revision space exhausted")?;
    let mut committed = next.clone();
    committed.revision = committed_revision;
    write_active_set_unlocked(path, &committed)?;
    Ok(committed_revision)
}

#[cfg(test)]
fn write_active_set(path: &Path, active_set: &PersistedActiveSet) -> Result<()> {
    let _lock = ActiveSetLock::acquire(path)?;
    write_active_set_unlocked(path, active_set)
}

fn write_active_set_unlocked(path: &Path, active_set: &PersistedActiveSet) -> Result<()> {
    validate_active_set_shape(active_set)?;
    let mut encoded = serde_json::to_vec_pretty(active_set)?;
    encoded.push(b'\n');
    if encoded.len() as u64 > MAX_ACTIVE_SET_BYTES {
        bail!("active set exceeds {MAX_ACTIVE_SET_BYTES} bytes");
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create active-set directory {}", parent.display()))?;
    // The state directory is the same-user authority root. Open it before the
    // transaction so no fallible setup remains after publication.
    let directory = File::open(parent).with_context(|| {
        format!(
            "failed to open active-set authority directory {}",
            parent.display()
        )
    })?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "active set must be a regular non-symlink file: {}",
                path.display()
            );
        }
    }

    let nonce = fresh_bearer_identity("active-set-temp")?;
    let temp = parent.join(format!(
        ".{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("active-set"),
        nonce.replace(':', "-")
    ));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("failed to create active-set temp file {}", temp.display()))?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temp, path).with_context(|| {
            format!("failed to atomically publish active set {}", path.display())
        })?;
        // A successful command acknowledges the active set only after both
        // file contents and the renamed directory entry have crossed their
        // durability barriers. If this post-rename barrier fails, return an
        // error and require reconstruction; never report a durable success.
        directory.sync_all().with_context(|| {
            format!(
                "active set was renamed but its directory durability barrier failed for {}; state must be reconstructed",
                path.display()
            )
        })?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{
        BuildManifest, CapabilityRequestManifest, HealthManifest, RuntimeManifest,
    };
    use super::*;

    fn manifest_with_request() -> PackageManifest {
        PackageManifest {
            schema: super::super::manifest::PACKAGE_SCHEMA_V1.to_owned(),
            name: "personality.test".to_owned(),
            version: "1.0.0".to_owned(),
            architecture: "host".to_owned(),
            payload_sha256: "0".repeat(64),
            runtime: RuntimeManifest {
                kind: "personality".to_owned(),
                entry: "/bin/runtime".to_owned(),
                abi: "ocore.personality/test-v1".to_owned(),
            },
            services: Vec::new(),
            capability_requests: vec![CapabilityRequestManifest {
                kind: "endpoint".to_owned(),
                rights: vec!["send".to_owned(), "receive".to_owned()],
                purpose: "personality channel".to_owned(),
            }],
            health: HealthManifest {
                protocol: HEALTH_PROTOCOL.to_owned(),
                timeout_ms: 10,
            },
            build: BuildManifest {
                source_sha256: "1".repeat(64),
                builder: "test-builder/v1".to_owned(),
            },
        }
    }

    #[test]
    fn activation_policy_is_default_deny_and_exact_rights_only() {
        let manifest = manifest_with_request();
        assert!(ActivationPolicy::default().authorize(&manifest).is_err());

        let mut policy = ActivationPolicy::new();
        policy
            .allow_request(
                "personality.test",
                "endpoint",
                "personality channel",
                ["send"],
            )
            .unwrap();
        assert!(policy.authorize(&manifest).is_err());
        policy
            .allow_request(
                "personality.test",
                "endpoint",
                "personality channel",
                ["send", "receive"],
            )
            .unwrap();
        assert!(policy.authorize(&manifest).is_ok());
    }

    #[test]
    fn capability_bearing_values_are_rejected_recursively() {
        let nested = OValue::Object {
            fields: BTreeMap::from([(
                "authority".into(),
                OValue::capability(CapabilityKind::Service, "forged", HashMap::new()),
            )]),
        };
        assert!(validate_structural_value(&nested, 4096, "test").is_err());
        let pure = OValue::Object {
            fields: BTreeMap::from([("message".into(), OValue::str_("hello"))]),
        };
        assert!(validate_structural_value(&pure, 4096, "test").is_ok());
        let oversized = OValue::str_("x".repeat(2048));
        assert!(validate_structural_value(&oversized, 128, "test").is_err());
    }

    #[test]
    fn durable_state_contains_metadata_but_no_live_authority() {
        let record = PersistedPackage {
            package_name: "personality.test".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            services: vec![PersistedService {
                name: "personality.test".into(),
                protocol: "test/v1".into(),
                generation: 7,
            }],
        };
        let active_set = persisted_from_records(
            &BTreeMap::from([("personality.test".into(), record)]),
            &BTreeMap::new(),
        );
        let json = serde_json::to_string(&active_set).unwrap();
        assert!(json.contains("sha256:"));
        for forbidden in ["token", "bearer", "session", "pid", "path"] {
            assert!(
                !json.contains(forbidden),
                "persisted live field `{forbidden}`"
            );
        }
    }

    #[test]
    fn active_set_limits_are_checked_before_reconstruction() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let too_many_packages = PersistedActiveSet {
            schema: ACTIVE_SET_SCHEMA.into(),
            revision: 0,
            active: (0..=MAX_ACTIVE_PACKAGES)
                .map(|index| PersistedPackage {
                    package_name: format!("package.{index}"),
                    digest: digest.clone(),
                    services: Vec::new(),
                })
                .collect(),
            rollback: Vec::new(),
        };
        assert!(validate_active_set_shape(&too_many_packages).is_err());

        let too_many_services = PersistedActiveSet {
            schema: ACTIVE_SET_SCHEMA.into(),
            revision: 0,
            active: vec![PersistedPackage {
                package_name: "package.services".into(),
                digest,
                services: (0..=MAX_ACTIVE_SERVICES)
                    .map(|index| PersistedService {
                        name: format!("service.{index}"),
                        protocol: "test/v1".into(),
                        generation: index as u64 + 1,
                    })
                    .collect(),
            }],
            rollback: Vec::new(),
        };
        assert!(validate_active_set_shape(&too_many_services).is_err());
    }

    #[test]
    fn request_side_control_plane_limits_fail_closed() {
        assert!(ensure_bearer_capacity(MAX_LIVE_BEARERS - 1).is_ok());
        assert!(ensure_bearer_capacity(MAX_LIVE_BEARERS).is_err());
        assert!(validate_composition_count(MAX_COMPOSITION_STEPS).is_ok());
        assert!(validate_composition_count(MAX_COMPOSITION_STEPS + 1).is_err());
        assert!(validate_operation(&"a".repeat(MAX_OPERATION_BYTES)).is_ok());
        assert!(validate_operation(&"a".repeat(MAX_OPERATION_BYTES + 1)).is_err());
        assert!(validate_operation("world/escape").is_err());
    }

    #[test]
    fn hosted_runtime_compatibility_is_exact() {
        let mut manifest = manifest_with_request();
        manifest.architecture = std::env::consts::ARCH.to_owned();
        manifest.runtime.kind = HOSTED_RUNTIME_KIND.to_owned();
        manifest.runtime.abi = RUNTIME_PROTOCOL.to_owned();
        assert!(validate_hosted_compatibility(&manifest).is_ok());

        manifest.architecture = if std::env::consts::ARCH == "x86_64" {
            "aarch64".to_owned()
        } else {
            "x86_64".to_owned()
        };
        assert!(validate_hosted_compatibility(&manifest)
            .unwrap_err()
            .to_string()
            .contains("incompatible with host architecture"));

        manifest.architecture = std::env::consts::ARCH.to_owned();
        manifest.runtime.kind = "personality".to_owned();
        assert!(validate_hosted_compatibility(&manifest)
            .unwrap_err()
            .to_string()
            .contains("runtime kind"));

        manifest.runtime.kind = HOSTED_RUNTIME_KIND.to_owned();
        manifest.runtime.abi = "ocore.runtime-service/v0".to_owned();
        assert!(validate_hosted_compatibility(&manifest)
            .unwrap_err()
            .to_string()
            .contains("runtime ABI"));
    }

    #[cfg(unix)]
    #[test]
    fn active_set_reader_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real.json");
        write_active_set(&real, &PersistedActiveSet::empty()).unwrap();
        let link = directory.path().join("active.json");
        symlink(&real, &link).unwrap();
        assert!(read_active_set(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observed_child_exit_finalizes_termination_only_once() {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("exit 0").process_group(0);
        let child = command.spawn().unwrap();
        let (requests, _receiver) = mpsc::channel();
        let mut runtime = ChildRuntime {
            child,
            requests,
            terminated: false,
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !runtime.has_exited().unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "child did not exit before test deadline"
            );
            thread::yield_now();
        }
        assert!(runtime.terminated);
        runtime.terminate();
        assert!(runtime.terminated);
    }
}
