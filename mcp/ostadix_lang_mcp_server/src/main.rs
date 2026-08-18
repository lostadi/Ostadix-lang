//! Ostadix-lang / O-lang MCP server (Rust-only, stdio).
//!
//! Exposes toolchain helpers so agents can run `O` / `olangc` with a correct
//! backends path — avoiding the relative-`backends` and `$VAR` splice traps.
//!
//! Logging goes to **stderr** only (stdout is MCP JSON-RPC).

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
#[cfg(unix)]
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const INTENT_SCHEMA_V1: &str = "oexec.execution-intent/v1";
const DEFAULT_INTENT_TTL_SECS: u64 = 120;
const MAX_INTENT_TTL_SECS: u64 = 900;
const MAX_INTENT_OPERATION_TIMEOUT_SECS: u64 = 900;
const MAX_LIVE_INTENTS: usize = 64;
const MAX_INFORMATION_STATE_PATH_BYTES: usize = 4096;
const MAX_INFORMATION_HEAD_NAME_BYTES: usize = 128;
const MAX_INFORMATION_INSPECTION_STDOUT_BYTES: usize = 256 * 1024;
const MAX_INFORMATION_INSPECTION_STDERR_BYTES: usize = 16 * 1024;
const DEFAULT_INFORMATION_INSPECTION_TIMEOUT_SECS: u64 = 10;
const MAX_INFORMATION_INSPECTION_TIMEOUT_SECS: u64 = 30;
const INFORMATION_NON_AUTHORITY_NOTICE: &str =
    "information presence and signatures grant no execution authority";

#[derive(Clone)]
struct OstadixMcp {
    tool_router: ToolRouter<Self>,
    runtime_search: RuntimeSearchPath,
    intents: Arc<Mutex<IntentStore>>,
}

impl OstadixMcp {
    fn new(runtime_search: RuntimeSearchPath) -> Self {
        Self {
            tool_router: Self::tool_router(),
            runtime_search,
            intents: Arc::new(Mutex::new(IntentStore::default())),
        }
    }
}

#[derive(Clone, Debug)]
struct IntentLease {
    program: PathBuf,
    cwd: PathBuf,
    root: PathBuf,
    backends: PathBuf,
    source_sha256: String,
    execution_intent_sha256: String,
    expires_at: Instant,
}

#[derive(Default)]
struct IntentStore {
    leases: BTreeMap<String, IntentLease>,
    reservations: BTreeSet<String>,
}

impl IntentStore {
    fn prune_expired(&mut self, now: Instant) {
        self.leases.retain(|_, lease| lease.expires_at > now);
    }

    fn reserve(&mut self, handle: String, now: Instant) -> Result<(), String> {
        self.prune_expired(now);
        if self.leases.len() + self.reservations.len() >= MAX_LIVE_INTENTS {
            return Err(format!(
                "execution-intent store is full (maximum {MAX_LIVE_INTENTS} live or in-progress handles)"
            ));
        }
        if self.leases.contains_key(&handle) || self.reservations.contains(&handle) {
            return Err("execution-intent handle collision".to_string());
        }
        self.reservations.insert(handle);
        Ok(())
    }

    fn cancel_reservation(&mut self, handle: &str) {
        self.reservations.remove(handle);
    }

    fn insert_reserved(&mut self, handle: String, lease: IntentLease) -> Result<(), String> {
        if !self.reservations.remove(&handle) {
            return Err("execution-intent reservation expired or was not established".to_string());
        }
        if self.leases.contains_key(&handle) {
            return Err("execution-intent handle collision".to_string());
        }
        self.leases.insert(handle, lease);
        Ok(())
    }

    /// Atomically consume a handle before target validation or dispatch. A
    /// failed execution attempt cannot replay the same bearer handle.
    fn take(&mut self, handle: &str, now: Instant) -> Result<IntentLease, String> {
        let lease = self
            .leases
            .remove(handle)
            .ok_or_else(|| "unknown or already-consumed execution-intent handle".to_string())?;
        if lease.expires_at <= now {
            return Err("execution-intent handle expired".to_string());
        }
        Ok(lease)
    }
}

/// Cancellation-safe reservation for one expensive intent analysis. The store
/// uses a standard mutex because every critical section is in-memory and
/// nonblocking; this lets Drop release a reservation even if an async MCP call
/// is cancelled while `olangc` is running.
struct IntentReservation {
    store: Arc<Mutex<IntentStore>>,
    handle: String,
    active: bool,
}

impl IntentReservation {
    fn acquire(
        store: Arc<Mutex<IntentStore>>,
        handle: String,
        now: Instant,
    ) -> Result<Self, String> {
        store
            .lock()
            .map_err(|_| "execution-intent store lock is poisoned".to_string())?
            .reserve(handle.clone(), now)?;
        Ok(Self {
            store,
            handle,
            active: true,
        })
    }

    fn commit(mut self, lease: IntentLease) -> Result<(), String> {
        self.store
            .lock()
            .map_err(|_| "execution-intent store lock is poisoned".to_string())?
            .insert_reserved(self.handle.clone(), lease)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for IntentReservation {
    fn drop(&mut self) {
        if self.active {
            if let Ok(mut store) = self.store.lock() {
                store.cancel_reservation(&self.handle);
            }
        }
    }
}

fn random_intent_handle() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    fill_handle_entropy(&mut bytes)?;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(encoded)
}

#[cfg(unix)]
fn fill_handle_entropy(bytes: &mut [u8]) -> Result<(), String> {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(bytes))
        .map_err(|error| format!("obtain handle entropy from /dev/urandom: {error}"))
}

#[cfg(not(unix))]
fn fill_handle_entropy(_bytes: &mut [u8]) -> Result<(), String> {
    Err(
        "execution-intent handles require an operating-system entropy source on this platform"
            .to_string(),
    )
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Deserialize)]
struct ExecutionIntentDocument {
    schema: String,
    source_sha256: String,
    execution_intent_sha256: String,
}

fn parse_execution_intent(stdout: &str) -> Result<ExecutionIntentDocument, String> {
    let document: ExecutionIntentDocument = serde_json::from_str(stdout)
        .map_err(|error| format!("olangc returned invalid execution-intent JSON: {error}"))?;
    if document.schema != INTENT_SCHEMA_V1 {
        return Err(format!(
            "olangc returned unsupported execution-intent schema {:?}",
            document.schema
        ));
    }
    if !is_sha256(&document.source_sha256) || !is_sha256(&document.execution_intent_sha256) {
        return Err("olangc returned a malformed execution-intent digest".to_string());
    }
    Ok(document)
}

fn validate_intent_target(
    lease: &IntentLease,
    program: &Path,
    cwd: &Path,
    root: &Path,
    backends: &Path,
) -> Result<(), String> {
    if lease.program != program {
        return Err(format!(
            "execution-intent program mismatch: analyzed={} requested={}",
            lease.program.display(),
            program.display()
        ));
    }
    if lease.cwd != cwd {
        return Err(format!(
            "execution-intent cwd mismatch: analyzed={} requested={}",
            lease.cwd.display(),
            cwd.display()
        ));
    }
    if lease.root != root {
        return Err(format!(
            "execution-intent root mismatch: analyzed={} current={}",
            lease.root.display(),
            root.display()
        ));
    }
    if lease.backends != backends {
        return Err(format!(
            "execution-intent backends mismatch: analyzed={} current={}",
            lease.backends.display(),
            backends.display()
        ));
    }
    Ok(())
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn canonical_directory(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        path.canonicalize().ok()
    } else {
        None
    }
}

fn is_lang_root(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
        && path.join("backends/python_shim.py").is_file()
        && path.join("examples/hello.O").is_file()
}

fn resolve_lang_root() -> PathBuf {
    if let Some(path) = std::env::var_os("O_LANG_ROOT").map(PathBuf::from) {
        if is_lang_root(&path) {
            return canonical_directory(&path).unwrap_or(path);
        }
    }

    if let Ok(current) = std::env::current_dir() {
        for candidate in current.ancestors() {
            if is_lang_root(candidate) {
                return canonical_directory(candidate).unwrap_or_else(|| candidate.to_path_buf());
            }
        }
    }

    for candidate in [home_dir().join("Ostadix-lang"), home_dir().join("O-lang")] {
        if is_lang_root(&candidate) {
            return canonical_directory(&candidate).unwrap_or(candidate);
        }
    }

    std::env::current_dir().unwrap_or_else(|_| home_dir().join("Ostadix-lang"))
}

fn resolve_backends(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("O_BACKENDS_DIR") {
        let pb = PathBuf::from(p);
        if let Some(canonical) = canonical_directory(&pb) {
            return canonical;
        }
    }
    let backends = root.join("backends");
    canonical_directory(&backends).unwrap_or(backends)
}

fn resolve_o_bin(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("OLANG") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return pb;
        }
    }
    let release = root.join("target/release/O");
    if release.is_file() {
        return release;
    }
    which::which("O").unwrap_or_else(|_| PathBuf::from("O"))
}

fn resolve_olangc(root: &Path) -> PathBuf {
    let release = root.join("target/release/olangc");
    if release.is_file() {
        return release;
    }
    which::which("olangc").unwrap_or_else(|_| PathBuf::from("olangc"))
}

fn resolve_o_info(root: &Path) -> Result<PathBuf, String> {
    let candidate = root.join("target/release/o-info");
    let metadata = std::fs::symlink_metadata(&candidate).map_err(|_| {
        format!(
            "local o-info is not built at the fixed repository path {}",
            candidate.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err("fixed repository o-info path must not be a symlink".to_string());
    }
    if !metadata.is_file() {
        return Err(format!(
            "local o-info is not built at the fixed repository path {}",
            candidate.display()
        ));
    }
    candidate
        .canonicalize()
        .map_err(|error| format!("resolve local o-info {}: {error}", candidate.display()))
}

const RUNTIME_PATH_MODE_ENV: &str = "OSTADIX_RUNTIME_PATH_MODE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimePathMode {
    InheritedOnly,
    InheritedPlusExplicit,
    DiscoverLocal,
}

impl RuntimePathMode {
    fn name(self) -> &'static str {
        match self {
            Self::InheritedOnly => "inherited-only",
            Self::InheritedPlusExplicit => "inherited-plus-explicit",
            Self::DiscoverLocal => "discover-local",
        }
    }
}

impl FromStr for RuntimePathMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "inherited-only" => Ok(Self::InheritedOnly),
            "inherited-plus-explicit" => Ok(Self::InheritedPlusExplicit),
            "discover-local" => Ok(Self::DiscoverLocal),
            _ => anyhow::bail!(
                "invalid {RUNTIME_PATH_MODE_ENV}={value:?}; expected inherited-only, inherited-plus-explicit, or discover-local"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimePathEntry {
    directory: PathBuf,
    source: String,
}

#[derive(Clone, Debug)]
struct RuntimeSearchPath {
    mode: RuntimePathMode,
    entries: Vec<RuntimePathEntry>,
    encoded: OsString,
}

impl RuntimeSearchPath {
    fn new(mode: RuntimePathMode, entries: Vec<RuntimePathEntry>) -> anyhow::Result<Self> {
        let encoded = std::env::join_paths(entries.iter().map(|entry| &entry.directory))
            .map_err(|error| anyhow::anyhow!("cannot encode runtime search PATH: {error}"))?;
        Ok(Self {
            mode,
            entries,
            encoded,
        })
    }

    fn source_for_executable(&self, executable: &Path) -> &str {
        let parent = executable.parent();
        self.entries
            .iter()
            .find(|entry| {
                parent == Some(entry.directory.as_path())
                    || parent
                        .and_then(|path| path.canonicalize().ok())
                        .zip(entry.directory.canonicalize().ok())
                        .is_some_and(|(left, right)| left == right)
            })
            .map_or("untracked", |entry| entry.source.as_str())
    }
}

fn append_runtime_path(
    entries: &mut Vec<RuntimePathEntry>,
    candidate: PathBuf,
    source: impl Into<String>,
) {
    if !entries.iter().any(|entry| entry.directory == candidate) {
        entries.push(RuntimePathEntry {
            directory: candidate,
            source: source.into(),
        });
    }
}

fn append_existing_runtime_path(
    entries: &mut Vec<RuntimePathEntry>,
    candidate: PathBuf,
    source: impl Into<String>,
) {
    if candidate.is_dir() {
        append_runtime_path(entries, candidate, source);
    }
}

fn append_path_list(entries: &mut Vec<RuntimePathEntry>, encoded: Option<&OsStr>, source: &str) {
    let Some(encoded) = encoded else {
        return;
    };
    for (ordinal, candidate) in std::env::split_paths(encoded).enumerate() {
        append_runtime_path(entries, candidate, format!("{source}:{ordinal}"));
    }
}

/// Preserve the client's PATH, then append local runtime locations commonly
/// omitted by GUI/MCP launchers. Appending rather than prepending ensures this
/// discovery fallback never silently replaces an explicitly selected runtime.
#[cfg(test)]
fn runtime_search_path_with_mode(
    root: &Path,
    home: &Path,
    inherited: Option<&OsStr>,
    explicit_extra: Option<&OsStr>,
    mode: RuntimePathMode,
) -> anyhow::Result<RuntimeSearchPath> {
    runtime_search_path_with_mode_and_manager_environment(
        root,
        home,
        inherited,
        explicit_extra,
        mode,
        &[],
    )
}

fn runtime_search_path_with_mode_and_manager_environment(
    root: &Path,
    home: &Path,
    inherited: Option<&OsStr>,
    explicit_extra: Option<&OsStr>,
    mode: RuntimePathMode,
    manager_environment: &[(OsString, OsString)],
) -> anyhow::Result<RuntimeSearchPath> {
    let mut entries = Vec::new();
    append_path_list(&mut entries, inherited, "inherited");
    if mode != RuntimePathMode::InheritedOnly {
        append_path_list(&mut entries, explicit_extra, "explicit");
    }

    if mode == RuntimePathMode::DiscoverLocal {
        append_existing_runtime_path(
            &mut entries,
            root.join("target/release"),
            "repository-release",
        );
        for (relative, label) in [
            (".local/bin", "home-local-bin"),
            (".cargo/bin", "home-cargo-bin"),
            (".nix-profile/bin", "home-nix-profile"),
            (".volta/bin", "home-volta"),
            (".pyenv/shims", "home-pyenv"),
            (".rbenv/shims", "home-rbenv"),
            (".asdf/shims", "home-asdf"),
            (".local/share/mise/shims", "home-mise"),
            (".local/share/fnm/aliases/default/bin", "home-fnm"),
            (".ghcup/bin", "home-ghcup"),
            (".cabal/bin", "home-cabal"),
            (".opam/default/bin", "home-opam"),
            (".dotnet", "home-dotnet"),
            (".dotnet/tools", "home-dotnet-tools"),
            (".wasmtime/bin", "home-wasmtime"),
            (".wasmer/bin", "home-wasmer"),
            (".bun/bin", "home-bun"),
            (".sdkman/candidates/java/current/bin", "home-sdkman-java"),
            ("miniforge3/bin", "home-miniforge"),
            ("miniconda3/bin", "home-miniconda"),
            ("anaconda3/bin", "home-anaconda"),
        ] {
            append_existing_runtime_path(&mut entries, home.join(relative), label);
        }
        // Explicit runtime-manager roots are more specific than generic
        // machine fallbacks and therefore retain precedence over them.
        append_environment_runtime_paths(&mut entries, manager_environment);
        for candidate in [
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
            "/opt/homebrew/opt/openjdk/bin",
            "/usr/local/bin",
            "/usr/local/sbin",
            "/usr/local/opt/openjdk/bin",
            "/nix/var/nix/profiles/default/bin",
            "/run/current-system/sw/bin",
            "/Library/Frameworks/Mono.framework/Versions/Current/Commands",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ] {
            append_existing_runtime_path(
                &mut entries,
                PathBuf::from(candidate),
                format!("system-fallback:{candidate}"),
            );
        }
    }

    RuntimeSearchPath::new(mode, entries)
}

fn append_environment_runtime_paths(
    entries: &mut Vec<RuntimePathEntry>,
    environment: &[(OsString, OsString)],
) {
    let value = |name: &str| {
        environment
            .iter()
            .find(|(key, _)| key == OsStr::new(name))
            .map(|(_, value)| value.as_os_str())
    };
    for variable in ["NVM_BIN", "PNPM_HOME"] {
        if let Some(path) = value(variable) {
            append_existing_runtime_path(
                entries,
                PathBuf::from(path),
                format!("manager-env:{variable}"),
            );
        }
    }
    for variable in [
        "CONDA_PREFIX",
        "VIRTUAL_ENV",
        "JAVA_HOME",
        "OPAM_SWITCH_PREFIX",
        "CARGO_HOME",
        "VOLTA_HOME",
        "GEM_HOME",
    ] {
        if let Some(prefix) = value(variable) {
            append_existing_runtime_path(
                entries,
                PathBuf::from(prefix).join("bin"),
                format!("manager-env:{variable}"),
            );
        }
    }
    for variable in ["PYENV_ROOT", "RBENV_ROOT"] {
        if let Some(prefix) = value(variable) {
            append_existing_runtime_path(
                entries,
                PathBuf::from(prefix).join("shims"),
                format!("manager-env:{variable}"),
            );
        }
    }
    if let Some(root) = value("DOTNET_ROOT") {
        append_existing_runtime_path(entries, PathBuf::from(root), "manager-env:DOTNET_ROOT");
    }
}

fn runtime_search_path(root: &Path) -> anyhow::Result<RuntimeSearchPath> {
    let mode = std::env::var_os(RUNTIME_PATH_MODE_ENV)
        .map(|value| value.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(RuntimePathMode::DiscoverLocal);
    let inherited = std::env::var_os("PATH");
    let explicit_extra = std::env::var_os("OSTADIX_RUNTIME_PATH");
    let manager_environment = runtime_manager_environment();
    runtime_search_path_with_mode_and_manager_environment(
        root,
        &home_dir(),
        inherited.as_deref(),
        explicit_extra.as_deref(),
        mode,
        &manager_environment,
    )
}

fn runtime_manager_environment() -> Vec<(OsString, OsString)> {
    [
        "NVM_BIN",
        "PNPM_HOME",
        "CONDA_PREFIX",
        "VIRTUAL_ENV",
        "JAVA_HOME",
        "OPAM_SWITCH_PREFIX",
        "CARGO_HOME",
        "VOLTA_HOME",
        "GEM_HOME",
        "PYENV_ROOT",
        "RBENV_ROOT",
        "DOTNET_ROOT",
    ]
    .into_iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
    .collect()
}

#[derive(Clone, Copy, Debug)]
struct CatalogRuntimeRequirement {
    key: &'static str,
    builtin: bool,
    precision: &'static str,
    alternatives: &'static [&'static [&'static str]],
}

#[derive(Clone, Copy, Debug)]
struct CatalogBackendRuntime {
    name: &'static str,
    requirement_key: &'static str,
    integer_exactness: &'static str,
    integer_exactness_bits: Option<u16>,
    integer_exactness_min: Option<&'static str>,
    integer_exactness_max: Option<&'static str>,
    rich_numbers: &'static str,
    state_support: &'static str,
    state_codec: Option<&'static str>,
    state_compatibility: Option<&'static str>,
    state_manifest_schema: Option<&'static str>,
    morphism_profile: Option<&'static str>,
}

macro_rules! backend_catalog_metadata {
    (
        current_schema: $current_schema:literal,
        legacy_schema_v4: $legacy_schema_v4:literal,
        legacy_schema_v3: $legacy_schema_v3:literal $(,)?
    ) => {
        const CATALOG_SCHEMA: &str = $current_schema;
        const CATALOG_LEGACY_SCHEMA_V4: &str = $legacy_schema_v4;
        const CATALOG_LEGACY_SCHEMA_V3: &str = $legacy_schema_v3;
    };
}

macro_rules! catalog_integer_exactness {
    (Unknown) => {
        (
            "unknown",
            None::<u16>,
            None::<&'static str>,
            None::<&'static str>,
        )
    };
    (ExactMagnitudeBits($bits:literal)) => {
        (
            "exact-magnitude-bits",
            Some($bits),
            None::<&'static str>,
            None::<&'static str>,
        )
    };
    (TwosComplementBits($bits:literal)) => {
        (
            "twos-complement-bits",
            Some($bits),
            None::<&'static str>,
            None::<&'static str>,
        )
    };
    (ExactRange { min: $min:literal, max: $max:literal }) => {
        ("exact-range", None::<u16>, Some($min), Some($max))
    };
    (Arbitrary) => {
        (
            "arbitrary",
            None::<u16>,
            None::<&'static str>,
            None::<&'static str>,
        )
    };
}

macro_rules! catalog_rich_numbers {
    (Unknown) => {
        "unknown"
    };
    (Preserved) => {
        "preserved"
    };
    (Collapsed) => {
        "collapsed"
    };
}

macro_rules! catalog_state_support {
    (Stateless) => {
        (
            "stateless",
            None::<&'static str>,
            None::<&'static str>,
            None::<&'static str>,
        )
    };
    (SemanticSnapshot { codec: $codec:literal, compatibility: ExactImplementation }) => {
        (
            "semantic-snapshot",
            Some($codec),
            Some("exact-implementation"),
            None::<&'static str>,
        )
    };
    (
        SemanticSnapshot {
            codec: $codec:literal,
            compatibility: CompatibilityClass($class:literal)
        }
    ) => {
        (
            "semantic-snapshot",
            Some($codec),
            Some($class),
            None::<&'static str>,
        )
    };
    (ExternalPinned { manifest_schema: $manifest_schema:literal }) => {
        (
            "external-pinned",
            None::<&'static str>,
            None::<&'static str>,
            Some($manifest_schema),
        )
    };
}

// Dependency-isolated projection of the catalog-owned profile labels. The MCP
// intentionally does not link the runtime's nominal morphism type.
macro_rules! catalog_morphism_profile {
    (None) => {
        None::<&'static str>
    };
    (PythonPlainData) => {
        Some("python-plain-data")
    };
    (JavascriptBindingStdout) => {
        Some("javascript-binding-stdout")
    };
    (RustSourceConstantStdout) => {
        Some("rust-source-constant-stdout")
    };
}

macro_rules! runtime_requirement_precision_name {
    (Exact) => {
        "exact"
    };
    (ConservativeAllSources) => {
        "conservative-all-sources"
    };
}

macro_rules! runtime_requirement_catalog {
    (
        $(
            {
                key: $key:literal,
                builtin: $builtin:literal,
                precision: $precision:ident,
                alternatives: [$([$($command:literal),* $(,)?]),* $(,)?],
            }
        ),* $(,)?
    ) => {
        const CATALOG_RUNTIME_REQUIREMENTS: &[CatalogRuntimeRequirement] = &[
            $(
                CatalogRuntimeRequirement {
                    key: $key,
                    builtin: $builtin,
                    precision: runtime_requirement_precision_name!($precision),
                    alternatives: &[$(&[$($command),*]),*],
                },
            )*
        ];
    };
}

macro_rules! backend_catalog {
    (
        $(
            {
                name: $name:literal,
                aliases: [$($alias:literal),* $(,)?],
                pure: $pure:literal,
                renderer: $renderer:ident,
                execution: $execution:ident,
                authorities: [$($authority:ident),* $(,)?],
                adapter: $adapter:ident,
                runtime: $runtime:literal,
                integer_exactness: $integer_exactness:ident
                    $(($($integer_arguments:literal),* $(,)?))?
                    $({ min: $integer_min:literal, max: $integer_max:literal })?,
                rich_numbers: $rich_numbers:ident,
                state_support: $state_support:ident
                    $({
                        $($state_key:ident: $state_value:tt),* $(,)?
                    })?,
                morphism_profile: $morphism_profile:ident,
            }
        ),* $(,)?
    ) => {
        const CATALOG_BACKEND_RUNTIMES: &[CatalogBackendRuntime] = &[
            $(
                CatalogBackendRuntime {
                    name: $name,
                    requirement_key: $runtime,
                    integer_exactness: catalog_integer_exactness!(
                        $integer_exactness
                        $(($($integer_arguments),*))?
                        $({ min: $integer_min, max: $integer_max })?
                    ).0,
                    integer_exactness_bits: catalog_integer_exactness!(
                        $integer_exactness
                        $(($($integer_arguments),*))?
                        $({ min: $integer_min, max: $integer_max })?
                    ).1,
                    integer_exactness_min: catalog_integer_exactness!(
                        $integer_exactness
                        $(($($integer_arguments),*))?
                        $({ min: $integer_min, max: $integer_max })?
                    ).2,
                    integer_exactness_max: catalog_integer_exactness!(
                        $integer_exactness
                        $(($($integer_arguments),*))?
                        $({ min: $integer_min, max: $integer_max })?
                    ).3,
                    rich_numbers: catalog_rich_numbers!($rich_numbers),
                    state_support: catalog_state_support!(
                        $state_support
                        $({ $($state_key: $state_value),* })?
                    ).0,
                    state_codec: catalog_state_support!(
                        $state_support
                        $({ $($state_key: $state_value),* })?
                    ).1,
                    state_compatibility: catalog_state_support!(
                        $state_support
                        $({ $($state_key: $state_value),* })?
                    ).2,
                    state_manifest_schema: catalog_state_support!(
                        $state_support
                        $({ $($state_key: $state_value),* })?
                    ).3,
                    morphism_profile: catalog_morphism_profile!($morphism_profile),
                },
            )*
        ];
    };
}

// Compile-time projection of the root catalog. The MCP crate remains
// dependency-isolated while consuming the identical backend declarations.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../src/backend_catalog.inc.rs"
));

fn catalog_backends_for(requirement_key: &str) -> Vec<&'static str> {
    CATALOG_BACKEND_RUNTIMES
        .iter()
        .filter(|backend| backend.requirement_key == requirement_key)
        .map(|backend| backend.name)
        .collect()
}

struct RuntimeDiscovery {
    search: RuntimeSearchPath,
    lines: Vec<String>,
    backend_count: usize,
    builtin_backends: usize,
    located_backends: usize,
    missing_backends: usize,
}

impl RuntimeDiscovery {
    fn summary(&self) -> String {
        format!(
            "runtime-summary backend-count={} builtin-backends={} located-backends={} missing-backends={}",
            self.backend_count,
            self.builtin_backends,
            self.located_backends,
            self.missing_backends
        )
    }

    fn to_text(&self) -> String {
        let mut out = format!(
            "runtime-catalog-schema={CATALOG_SCHEMA}\nruntime-catalog-legacy-schema-v4={CATALOG_LEGACY_SCHEMA_V4}\nruntime-catalog-legacy-schema-v3={CATALOG_LEGACY_SCHEMA_V3}\nruntime-catalog-projection=compiled-mcp-snapshot\nruntime-search-mode={}\nruntime-search-path={}\n{}\n",
            self.search.mode.name(),
            self.search.encoded.to_string_lossy(),
            self.summary()
        );
        for (index, entry) in self.search.entries.iter().enumerate() {
            writeln!(
                out,
                "runtime-search-entry index={index} source={} path={}",
                entry.source,
                entry.directory.display()
            )
            .expect("writing to a String cannot fail");
        }
        for line in &self.lines {
            writeln!(out, "{line}").expect("writing to a String cannot fail");
        }
        out.push_str(
            "runtime-note discovery is descriptive; missing optional runtimes do not block unrelated backends\n",
        );
        out.push_str(
            "runtime-note only declared and located are established here; invocability, compatibility, authorization, health, and per-operation admission require separate evidence\n",
        );
        out.push_str(
            "runtime-note morphism profiles are bounded shadow descriptions; they do not authorize execution or claim generic backend crossings\n",
        );
        out
    }
}

fn discover_runtimes(search: &RuntimeSearchPath, root: &Path) -> RuntimeDiscovery {
    let cwd = std::env::current_dir().unwrap_or_else(|_| root.to_path_buf());
    let builtin_backends = CATALOG_RUNTIME_REQUIREMENTS
        .iter()
        .filter(|requirement| requirement.builtin)
        .flat_map(|requirement| catalog_backends_for(requirement.key))
        .collect::<Vec<_>>();
    let builtin_precision = CATALOG_RUNTIME_REQUIREMENTS
        .iter()
        .find(|requirement| requirement.builtin)
        .map_or("unknown", |requirement| requirement.precision);
    let mut lines = vec![format!(
        "runtime backends={} status=builtin precision={builtin_precision} declared=catalog located=not-required invocable=not-probed compatible=not-probed authorized=operation-scoped-deferred healthy=not-probed admitted=operation-scoped-not-evaluated selected=ostadix-runtime",
        builtin_backends.join(",")
    )];
    let mut located_backends = 0;
    let mut missing_backends = 0;

    for requirement in CATALOG_RUNTIME_REQUIREMENTS
        .iter()
        .filter(|requirement| !requirement.builtin)
    {
        let backends = catalog_backends_for(requirement.key);
        let selected = requirement.alternatives.iter().find_map(|alternative| {
            let resolved = alternative
                .iter()
                .map(|command| {
                    which::which_in(command, Some(&search.encoded), &cwd)
                        .ok()
                        .map(|path| (*command, path))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(resolved)
        });
        let backend_names = backends.join(",");
        match selected {
            Some(resolved) => {
                located_backends += backends.len();
                let commands = resolved
                    .iter()
                    .map(|(command, _)| *command)
                    .collect::<Vec<_>>()
                    .join("+");
                let paths = resolved
                    .iter()
                    .map(|(command, path)| format!("{command}={}", path.display()))
                    .collect::<Vec<_>>()
                    .join(",");
                let path_sources = resolved
                    .iter()
                    .map(|(command, path)| {
                        format!("{command}={}", search.source_for_executable(path))
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                lines.push(format!(
                    "runtime backends={backend_names} status=located precision={} declared=catalog located=satisfied invocable=not-probed compatible=not-probed authorized=operation-scoped-deferred healthy=not-probed admitted=operation-scoped-not-evaluated selected={commands} paths=[{paths}] path-sources=[{path_sources}]",
                    requirement.precision
                ));
            }
            None => {
                missing_backends += backends.len();
                let alternatives = requirement
                    .alternatives
                    .iter()
                    .map(|alternative| alternative.join("+"))
                    .collect::<Vec<_>>()
                    .join("|");
                lines.push(format!(
                    "runtime backends={backend_names} status=missing precision={} declared=catalog located=unsatisfied invocable=not-probed compatible=not-probed authorized=operation-scoped-deferred healthy=not-probed admitted=operation-scoped-not-evaluated alternatives=[{alternatives}]",
                    requirement.precision
                ));
            }
        }
    }

    for backend in CATALOG_BACKEND_RUNTIMES {
        let integer_exactness = match (
            backend.integer_exactness_bits,
            backend.integer_exactness_min,
            backend.integer_exactness_max,
        ) {
            (Some(bits), None, None) => format!("{}:{bits}", backend.integer_exactness),
            (None, Some(min), Some(max)) => {
                format!("{}:[{min},{max}]", backend.integer_exactness)
            }
            (None, None, None) => backend.integer_exactness.to_string(),
            _ => unreachable!("catalog exactness projection has inconsistent parameters"),
        };
        let state_detail = match (
            backend.state_codec,
            backend.state_compatibility,
            backend.state_manifest_schema,
        ) {
            (Some(codec), Some(compatibility), None) => {
                format!(" codec={codec} compatibility={compatibility}")
            }
            (None, None, Some(manifest_schema)) => {
                format!(" manifest-schema={manifest_schema}")
            }
            (None, None, None) => String::new(),
            _ => unreachable!("catalog state projection has inconsistent parameters"),
        };
        lines.push(format!(
            "runtime-capability backend={} integer-exactness={} rich-numbers={} state-support={}{} morphism-profile={} provenance=catalog",
            backend.name,
            integer_exactness,
            backend.rich_numbers,
            backend.state_support,
            state_detail,
            backend.morphism_profile.unwrap_or("none"),
        ));
    }

    RuntimeDiscovery {
        search: search.clone(),
        lines,
        backend_count: CATALOG_BACKEND_RUNTIMES.len(),
        builtin_backends: builtin_backends.len(),
        located_backends,
        missing_backends,
    }
}

fn text_ok(s: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(s.into())]))
}

fn text_err(s: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![Content::text(s.into())]))
}

async fn run_cmd(
    program: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(&str, String)],
    timeout_secs: u64,
) -> Result<(i32, String, String), String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Keep abnormal future cancellation from orphaning the group leader;
        // the explicit timeout path below kills and reaps the whole group.
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", program.display()))?;

    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout was not piped".to_string())?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "child stderr was not piped".to_string())?;
    #[cfg(unix)]
    let process_group_id = child.id();
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let completed = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            stdout_pipe.read_to_end(&mut stdout_bytes),
            stderr_pipe.read_to_end(&mut stderr_bytes),
        );
        let status = status.map_err(|e| format!("wait: {e}"))?;
        stdout.map_err(|e| format!("read stdout: {e}"))?;
        stderr.map_err(|e| format!("read stderr: {e}"))?;
        Ok::<_, String>(status)
    })
    .await;

    let status = match completed {
        Ok(result) => result?,
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = process_group_id {
                if let Ok(group_id) = i32::try_from(pid) {
                    // SAFETY: the child was placed in a new process group whose
                    // id is the leader pid. Keep that id before waiting so the
                    // group can still be killed after the leader has exited and
                    // descendants are retaining its stdout/stderr pipes.
                    unsafe {
                        libc::kill(-group_id, libc::SIGKILL);
                    }
                }
            }
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill().await;
            }
            let _ = child.wait().await;
            return Err(format!("timeout after {timeout_secs}s"));
        }
    };
    let code = status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    Ok((code, stdout, stderr))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InformationInspectRunError {
    Spawn,
    Timeout,
    StdoutLimit,
    StderrLimit,
    Io,
    InvalidUtf8,
}

impl InformationInspectRunError {
    fn public_message(self) -> &'static str {
        match self {
            Self::Spawn => "could not start the fixed local o-info inspector",
            Self::Timeout => "local o-info read-only inspection timed out",
            Self::StdoutLimit => "local o-info exceeded the stdout inspection bound",
            Self::StderrLimit => "local o-info exceeded the stderr inspection bound",
            Self::Io => "local o-info inspection failed while collecting bounded output",
            Self::InvalidUtf8 => "local o-info returned non-UTF-8 inspection output",
        }
    }
}

async fn read_information_pipe_bounded<R: AsyncRead + Unpin>(
    reader: R,
    maximum: usize,
) -> Result<Vec<u8>, ()> {
    let limit = u64::try_from(maximum)
        .map_err(|_| ())?
        .checked_add(1)
        .ok_or(())?;
    let mut bytes = Vec::with_capacity(maximum.min(16 * 1024));
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ())?;
    if bytes.len() > maximum {
        Err(())
    } else {
        Ok(bytes)
    }
}

async fn terminate_information_child(
    child: &mut tokio::process::Child,
    #[cfg(unix)] process_group_id: Option<u32>,
) {
    #[cfg(unix)]
    if let Some(pid) = process_group_id {
        if let Ok(group_id) = i32::try_from(pid) {
            // SAFETY: this inspector child is placed in a fresh process group.
            unsafe {
                libc::kill(-group_id, libc::SIGKILL);
            }
        }
    }
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill().await;
    }
    let _ = child.wait().await;
}

async fn run_information_inspect_bounded(
    program: &Path,
    args: &[&str],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<(i32, String, String), InformationInspectRunError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| InformationInspectRunError::Spawn)?;
    let stdout = child.stdout.take().ok_or(InformationInspectRunError::Io)?;
    let stderr = child.stderr.take().ok_or(InformationInspectRunError::Io)?;
    #[cfg(unix)]
    let process_group_id = child.id();

    let mut stdout_task = tokio::spawn(read_information_pipe_bounded(
        stdout,
        MAX_INFORMATION_INSPECTION_STDOUT_BYTES,
    ));
    let mut stderr_task = tokio::spawn(read_information_pipe_bounded(
        stderr,
        MAX_INFORMATION_INSPECTION_STDERR_BYTES,
    ));
    let timeout = tokio::time::sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(timeout);
    let mut status = None;
    let mut stdout_bytes = None;
    let mut stderr_bytes = None;

    while status.is_none() || stdout_bytes.is_none() || stderr_bytes.is_none() {
        tokio::select! {
            waited = child.wait(), if status.is_none() => {
                match waited {
                    Ok(value) => status = Some(value),
                    Err(_) => {
                        terminate_information_child(&mut child, #[cfg(unix)] process_group_id).await;
                        stdout_task.abort();
                        stderr_task.abort();
                        return Err(InformationInspectRunError::Io);
                    }
                }
            }
            read = &mut stdout_task, if stdout_bytes.is_none() => {
                match read {
                    Ok(Ok(bytes)) => stdout_bytes = Some(bytes),
                    Ok(Err(())) => {
                        terminate_information_child(&mut child, #[cfg(unix)] process_group_id).await;
                        stderr_task.abort();
                        return Err(InformationInspectRunError::StdoutLimit);
                    }
                    Err(_) => {
                        terminate_information_child(&mut child, #[cfg(unix)] process_group_id).await;
                        stderr_task.abort();
                        return Err(InformationInspectRunError::Io);
                    }
                }
            }
            read = &mut stderr_task, if stderr_bytes.is_none() => {
                match read {
                    Ok(Ok(bytes)) => stderr_bytes = Some(bytes),
                    Ok(Err(())) => {
                        terminate_information_child(&mut child, #[cfg(unix)] process_group_id).await;
                        stdout_task.abort();
                        return Err(InformationInspectRunError::StderrLimit);
                    }
                    Err(_) => {
                        terminate_information_child(&mut child, #[cfg(unix)] process_group_id).await;
                        stdout_task.abort();
                        return Err(InformationInspectRunError::Io);
                    }
                }
            }
            _ = &mut timeout => {
                terminate_information_child(&mut child, #[cfg(unix)] process_group_id).await;
                stdout_task.abort();
                stderr_task.abort();
                return Err(InformationInspectRunError::Timeout);
            }
        }
    }

    let code = status
        .expect("inspection status completed")
        .code()
        .unwrap_or(-1);
    let stdout = String::from_utf8(stdout_bytes.expect("inspection stdout completed"))
        .map_err(|_| InformationInspectRunError::InvalidUtf8)?;
    let stderr = String::from_utf8(stderr_bytes.expect("inspection stderr completed"))
        .map_err(|_| InformationInspectRunError::InvalidUtf8)?;
    Ok((code, stdout, stderr))
}

fn resolve_directory(base: &Path, requested: Option<&str>, label: &str) -> Result<PathBuf, String> {
    let candidate = requested
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        })
        .unwrap_or_else(|| base.to_path_buf());
    if !candidate.is_dir() {
        return Err(format!(
            "{label} is not a directory: {}",
            candidate.display()
        ));
    }
    candidate
        .canonicalize()
        .map_err(|error| format!("resolve {label} {}: {error}", candidate.display()))
}

fn resolve_information_state(root: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(requested);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        root.join(requested)
    };
    let metadata = std::fs::symlink_metadata(&candidate)
        .map_err(|_| "information state is not an existing directory".to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("information state root must not be a symlink".to_string());
    }
    if !metadata.is_dir() {
        return Err("information state is not an existing directory".to_string());
    }
    candidate
        .canonicalize()
        .map_err(|_| "could not resolve information state directory".to_string())
}

fn resolve_file(base: &Path, requested: &str, label: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(requested);
    let candidate = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    if !candidate.is_file() {
        return Err(format!("{label} is not a file: {}", candidate.display()));
    }
    candidate
        .canonicalize()
        .map_err(|error| format!("resolve {label} {}: {error}", candidate.display()))
}

fn resolve_run_target(
    root: &Path,
    requested_path: &str,
    requested_cwd: Option<&str>,
) -> Result<(PathBuf, PathBuf), String> {
    let input_path = Path::new(requested_path);
    let cwd = match requested_cwd {
        Some(requested) => resolve_directory(root, Some(requested), "cwd")?,
        None if input_path.is_absolute() => {
            let parent = input_path.parent().ok_or_else(|| {
                format!(
                    "absolute program has no parent directory: {}",
                    input_path.display()
                )
            })?;
            resolve_directory(parent, None, "program cwd")?
        }
        None => resolve_directory(root, None, "cwd")?,
    };
    let program = resolve_file(&cwd, requested_path, "program")?;
    Ok((program, cwd))
}

fn format_run(code: i32, stdout: &str, stderr: &str) -> String {
    let mut s = format!("exit={code}\n");
    if !stdout.is_empty() {
        s.push_str("--- stdout ---\n");
        s.push_str(stdout);
        if !stdout.ends_with('\n') {
            s.push('\n');
        }
    }
    if !stderr.is_empty() {
        s.push_str("--- stderr ---\n");
        s.push_str(stderr);
        if !stderr.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

// Empty args for zero-parameter tools — emits `type: object` so strict MCP
// clients (OpenCode, TS SDK) accept tools/list instead of rejecting `{}`.
#[derive(Debug, Default, Deserialize)]
struct EmptyArgs {}

impl schemars::JsonSchema for EmptyArgs {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EmptyArgs".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Schemars omits an empty object `properties` map. Some
        // strict MCP clients require the keyword even for zero-argument tools,
        // so construct the exact zero-argument object contract explicitly.
        schemars::json_schema!({
            "type": "object",
            "description": "No parameters",
            "properties": {}
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunOArgs {
    #[schemars(
        description = "Path to a .O program (absolute paths default cwd to their parent; relative paths use cwd/O_LANG_ROOT)"
    )]
    path: String,
    #[serde(default)]
    #[schemars(description = "Optional working directory (relative paths use O_LANG_ROOT)")]
    cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "Timeout seconds (default 120)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AnalyzeIntentArgs {
    #[schemars(
        description = "Path to a .O program (absolute paths default cwd to their parent; relative paths use cwd/O_LANG_ROOT)"
    )]
    path: String,
    #[serde(default)]
    #[schemars(description = "Optional working directory (relative paths use O_LANG_ROOT)")]
    cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "One-use handle lifetime in seconds (default 120; maximum 900)")]
    ttl_secs: Option<u64>,
    #[serde(default)]
    #[schemars(description = "Analysis timeout seconds (default 120)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecuteIntentArgs {
    #[schemars(description = "Opaque one-use handle returned by o_analyze_intent")]
    handle: String,
    #[schemars(description = "Path resolving to the same canonical .O program analyzed earlier")]
    path: String,
    #[serde(default)]
    #[schemars(
        description = "Working directory resolving to the same canonical cwd analyzed earlier"
    )]
    cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "Execution timeout seconds (default 120)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OlangcArgs {
    #[schemars(description = "Path to a .O program (relative paths use O_LANG_ROOT)")]
    path: String,
    #[serde(default)]
    #[schemars(
        description = "olangc target: ir | dot | script | wasm | or omit for default AOT analysis"
    )]
    target: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional -o output path (relative paths use O_LANG_ROOT)")]
    output: Option<String>,
    #[serde(default)]
    #[schemars(description = "Timeout seconds (default 180)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchRunArgs {
    #[schemars(
        description = "Search tool name without .O, e.g. sptm_retype_catalog, nscramble_mine, lab_pipeline"
    )]
    name: String,
    #[serde(default)]
    #[schemars(description = "a18re work root (default A18_WORK or ~/a18re)")]
    work: Option<String>,
    #[serde(default)]
    #[schemars(description = "Timeout seconds (default 300)")]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InformationInspectArgs {
    #[schemars(
        description = "Existing local Information V1 state directory (absolute or relative to O_LANG_ROOT; maximum 4096 UTF-8 bytes)"
    )]
    state: String,
    #[serde(default)]
    #[schemars(
        description = "Fixed local head name (default main; maximum 128 ASCII token bytes)"
    )]
    head: Option<String>,
    #[serde(default)]
    #[schemars(description = "Read-only inspection timeout seconds (default 10; maximum 30)")]
    timeout_secs: Option<u64>,
}

fn validate_information_head_name(name: &str) -> Result<(), String> {
    if name.len() > MAX_INFORMATION_HEAD_NAME_BYTES
        || name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("invalid bounded information head name".to_string());
    }
    Ok(())
}

fn lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sanitize_information_head_output(stdout: &str, expected_head: &str) -> Result<String, String> {
    let mut lines = stdout.lines();
    let summary = lines
        .next()
        .ok_or_else(|| "o-info head returned no summary".to_string())?;
    let prefix = "head state=";
    let summary = summary
        .strip_prefix(prefix)
        .ok_or_else(|| "o-info head returned an unexpected summary prefix".to_string())?;
    let marker = format!(" name={expected_head} revision=");
    let marker_at = summary
        .rfind(&marker)
        .ok_or_else(|| "o-info head returned an unexpected head name".to_string())?;
    let state = &summary[..marker_at];
    if state.is_empty() || state.chars().any(char::is_control) {
        return Err("o-info head returned an invalid state token".to_string());
    }
    let remainder = &summary[marker_at + marker.len()..];

    let mut sanitized = format!("head={expected_head}\n");
    let expected_fact_count;
    if remainder == "none" {
        sanitized.push_str("revision=none\nfacts=0\n");
        expected_fact_count = 0_usize;
    } else {
        let (revision, rest) = remainder
            .split_once(" snapshot=")
            .ok_or_else(|| "o-info head omitted snapshot identity".to_string())?;
        let (snapshot, facts) = rest
            .split_once(" facts=")
            .ok_or_else(|| "o-info head omitted fact count".to_string())?;
        if !lowercase_sha256(revision) || !lowercase_sha256(snapshot) {
            return Err("o-info head returned a malformed object identity".to_string());
        }
        expected_fact_count = facts
            .parse::<usize>()
            .map_err(|_| "o-info head returned a malformed fact count".to_string())?;
        sanitized.push_str(&format!(
            "revision={revision}\nsnapshot={snapshot}\nfacts={expected_fact_count}\n"
        ));
    }

    let mut facts = BTreeSet::new();
    let mut authority_seen = false;
    for line in lines {
        if line == format!("authority={INFORMATION_NON_AUTHORITY_NOTICE}") {
            if authority_seen {
                return Err("o-info head repeated its authority notice".to_string());
            }
            authority_seen = true;
            continue;
        }
        if authority_seen {
            return Err("o-info head emitted content after its authority notice".to_string());
        }
        let fact = line
            .strip_prefix("fact=")
            .ok_or_else(|| "o-info head returned an unexpected output key".to_string())?;
        if !lowercase_sha256(fact) || !facts.insert(fact.to_string()) {
            return Err("o-info head returned a malformed or duplicate fact identity".to_string());
        }
    }
    if !authority_seen || facts.len() != expected_fact_count {
        return Err("o-info head fact count or authority notice mismatch".to_string());
    }
    for fact in facts {
        sanitized.push_str(&format!("fact={fact}\n"));
    }
    sanitized.push_str(&format!(
        "authority={INFORMATION_NON_AUTHORITY_NOTICE}\nsource=local-o-info-read-only\n"
    ));
    if sanitized.len() > MAX_INFORMATION_INSPECTION_STDOUT_BYTES {
        return Err("sanitized information inspection exceeds its output bound".to_string());
    }
    Ok(sanitized)
}

#[tool_router]
impl OstadixMcp {
    #[tool(
        description = "Report O-lang / Ostadix-lang environment: roots, tools, shim presence, and all-runtime summary"
    )]
    async fn o_env(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let olangc = resolve_olangc(&root);
        let shim = backends.join("python_shim.py");
        let runtimes = discover_runtimes(&self.runtime_search, &root);
        let msg = format!(
            "O_LANG_ROOT={}\nO_BACKENDS_DIR={}\nO_bin={}\nolangc={}\npython_shim={} ({})\n{}\nnote=always pass absolute backends dir to O; never bare \"backends\" from unrelated cwd; never put $VAR inside .O sources\n",
            root.display(),
            backends.display(),
            o_bin.display(),
            olangc.display(),
            shim.display(),
            if shim.is_file() { "ok" } else { "MISSING" },
            runtimes.summary()
        );
        text_ok(msg)
    }

    #[tool(
        description = "Discover executable requirements for every canonical Ostadix backend without blocking on missing optional runtimes"
    )]
    async fn o_runtimes(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        text_ok(discover_runtimes(&self.runtime_search, &root).to_text())
    }

    #[tool(
        description = "Inspect exactly one existing local Information V1 head through the fixed read-only o-info path; no cloud, network, mutation, dispatch, or arbitrary subcommands"
    )]
    async fn o_information_inspect(
        &self,
        Parameters(args): Parameters<InformationInspectArgs>,
    ) -> Result<CallToolResult, McpError> {
        if args.state.len() > MAX_INFORMATION_STATE_PATH_BYTES
            || args.state.is_empty()
            || args.state.chars().any(char::is_control)
        {
            return text_err("invalid bounded information state path");
        }
        let head = args.head.as_deref().unwrap_or("main");
        if let Err(error) = validate_information_head_name(head) {
            return text_err(error);
        }
        let timeout = args
            .timeout_secs
            .unwrap_or(DEFAULT_INFORMATION_INSPECTION_TIMEOUT_SECS);
        if timeout == 0 || timeout > MAX_INFORMATION_INSPECTION_TIMEOUT_SECS {
            return text_err(format!(
                "information inspection timeout must be between 1 and {MAX_INFORMATION_INSPECTION_TIMEOUT_SECS} seconds"
            ));
        }

        let root = resolve_lang_root();
        let state = match resolve_information_state(&root, &args.state) {
            Ok(state) => state,
            Err(error) => return text_err(error),
        };
        let o_info = match resolve_o_info(&root) {
            Ok(path) => path,
            Err(error) => return text_err(error),
        };
        let state_text = match state.to_str() {
            Some(path) => path.to_string(),
            None => return text_err("resolved information state path is not UTF-8"),
        };
        match run_information_inspect_bounded(
            &o_info,
            &["head", "--state", &state_text, "--head", head],
            &root,
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                if stdout.len() > MAX_INFORMATION_INSPECTION_STDOUT_BYTES
                    || stderr.len() > MAX_INFORMATION_INSPECTION_STDERR_BYTES
                {
                    return text_err("local o-info exceeded the MCP inspection output bound");
                }
                if code != 0 {
                    return text_err("local o-info read-only inspection rejected the state/head");
                }
                if !stderr.is_empty() {
                    return text_err("local o-info emitted unexpected stderr during inspection");
                }
                match sanitize_information_head_output(&stdout, head) {
                    Ok(output) => text_ok(output),
                    Err(error) => text_err(error),
                }
            }
            Err(error) => text_err(error.public_message()),
        }
    }

    #[tool(
        description = "Smoke-test O toolchain: run examples/hello.O (expect 2) with correct backends path"
    )]
    async fn o_smoke(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let hello = root.join("examples/hello.O");
        if !hello.is_file() {
            return text_err(format!("missing {}", hello.display()));
        }
        if !backends.join("python_shim.py").is_file() {
            return text_err(format!(
                "backends invalid: {} (no python_shim.py)",
                backends.display()
            ));
        }
        match run_cmd(
            &o_bin,
            &[
                hello.to_str().unwrap_or(""),
                backends.to_str().unwrap_or(""),
            ],
            Some(&root),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
            ],
            60,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format_run(code, &stdout, &stderr);
                if code == 0 && stdout.contains('2') {
                    text_ok(format!("SMOKE_OK\n{body}"))
                } else {
                    text_err(format!("SMOKE_FAIL\n{body}"))
                }
            }
            Err(e) => text_err(e),
        }
    }

    #[tool(
        description = "Run a .O program with the O interpreter using absolute O_BACKENDS_DIR (fixes relative backends failures)"
    )]
    async fn o_run(
        &self,
        Parameters(args): Parameters<RunOArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let (path, cwd) = match resolve_run_target(&root, &args.path, args.cwd.as_deref()) {
            Ok(target) => target,
            Err(error) => return text_err(error),
        };
        if !backends.is_dir() {
            return text_err(format!("backends missing: {}", backends.display()));
        }
        let timeout = args.timeout_secs.unwrap_or(120);
        match run_cmd(
            &o_bin,
            &[path.to_str().unwrap_or(""), backends.to_str().unwrap_or("")],
            Some(&cwd),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
                ("A18_WORK", cwd.display().to_string()),
            ],
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format_run(code, &stdout, &stderr);
                if code == 0 {
                    text_ok(body)
                } else {
                    text_err(body)
                }
            }
            Err(e) => text_err(e),
        }
    }

    #[tool(
        description = "Analyze a .O program without executing it and issue a bounded, expiring, one-use same-intent handle (not authorization or a capability)"
    )]
    async fn o_analyze_intent(
        &self,
        Parameters(args): Parameters<AnalyzeIntentArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let olangc = resolve_olangc(&root);
        let (program, cwd) = match resolve_run_target(&root, &args.path, args.cwd.as_deref()) {
            Ok(target) => target,
            Err(error) => return text_err(error),
        };
        if !backends.is_dir() {
            return text_err(format!("backends missing: {}", backends.display()));
        }
        let ttl_secs = args.ttl_secs.unwrap_or(DEFAULT_INTENT_TTL_SECS);
        if !(1..=MAX_INTENT_TTL_SECS).contains(&ttl_secs) {
            return text_err(format!(
                "ttl_secs must be between 1 and {MAX_INTENT_TTL_SECS}"
            ));
        }
        let timeout = args.timeout_secs.unwrap_or(120);
        if !(1..=MAX_INTENT_OPERATION_TIMEOUT_SECS).contains(&timeout) {
            return text_err(format!(
                "timeout_secs must be between 1 and {MAX_INTENT_OPERATION_TIMEOUT_SECS}"
            ));
        }
        let handle = match random_intent_handle() {
            Ok(handle) => handle,
            Err(error) => return text_err(error),
        };
        let reservation_now = Instant::now();
        let reservation =
            match IntentReservation::acquire(self.intents.clone(), handle.clone(), reservation_now)
            {
                Ok(reservation) => reservation,
                Err(error) => return text_err(error),
            };
        let argv = [
            program.to_string_lossy().into_owned(),
            "--target".to_string(),
            "ir".to_string(),
            "--shim-dir".to_string(),
            backends.to_string_lossy().into_owned(),
            "--execution-intent-json".to_string(),
        ];
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let analysis = run_cmd(
            &olangc,
            &refs,
            Some(&cwd),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
                ("A18_WORK", cwd.display().to_string()),
            ],
            timeout,
        )
        .await;
        let (code, stdout, stderr) = match analysis {
            Ok(output) => output,
            Err(error) => return text_err(error),
        };
        if code != 0 {
            return text_err(format_run(code, &stdout, &stderr));
        }
        let document = match parse_execution_intent(&stdout) {
            Ok(document) => document,
            Err(error) => return text_err(error),
        };
        let now = Instant::now();
        let lease = IntentLease {
            program: program.clone(),
            cwd: cwd.clone(),
            root: root.clone(),
            backends: backends.clone(),
            source_sha256: document.source_sha256.clone(),
            execution_intent_sha256: document.execution_intent_sha256.clone(),
            expires_at: now + Duration::from_secs(ttl_secs),
        };
        if let Err(error) = reservation.commit(lease) {
            return text_err(error);
        }
        text_ok(format!(
            "intent-handle={handle}\nintent-schema={}\nsource-sha256={}\nexecution-intent-sha256={}\nprogram={}\ncwd={}\nroot={}\nbackends={}\nexpires-in-seconds={ttl_secs}\nintent-note=same-intent gate only; not authorization, a capability, runtime health, capacity, or a retained AdmittedExecution\n",
            document.schema,
            document.source_sha256,
            document.execution_intent_sha256,
            program.display(),
            cwd.display(),
            root.display(),
            backends.display(),
        ))
    }

    #[tool(
        description = "Consume a one-use o_analyze_intent handle and ask O to recompute the same stable Intent V1 before fresh V6 admission and dispatch"
    )]
    async fn o_execute_intent(
        &self,
        Parameters(args): Parameters<ExecuteIntentArgs>,
    ) -> Result<CallToolResult, McpError> {
        // Consume first: expiry, mismatched arguments, mutation, admission
        // rejection, timeout, and runtime failure all make the handle unusable.
        let lease = match self
            .intents
            .lock()
            .map_err(|_| "execution-intent store lock is poisoned".to_string())
            .and_then(|mut store| store.take(args.handle.trim(), Instant::now()))
        {
            Ok(lease) => lease,
            Err(error) => return text_err(error),
        };
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let (program, cwd) = match resolve_run_target(&root, &args.path, args.cwd.as_deref()) {
            Ok(target) => target,
            Err(error) => return text_err(error),
        };
        if let Err(error) = validate_intent_target(&lease, &program, &cwd, &root, &backends) {
            return text_err(error);
        }
        let argv = [
            "--require-source-sha256".to_string(),
            lease.source_sha256,
            "--require-execution-intent-sha256".to_string(),
            lease.execution_intent_sha256,
            program.to_string_lossy().into_owned(),
            backends.to_string_lossy().into_owned(),
        ];
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let timeout = args.timeout_secs.unwrap_or(120);
        match run_cmd(
            &o_bin,
            &refs,
            Some(&cwd),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
                ("A18_WORK", cwd.display().to_string()),
            ],
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format_run(code, &stdout, &stderr);
                if code == 0 {
                    text_ok(format!("intent-consumed=true\n{body}"))
                } else {
                    text_err(format!("intent-consumed=true\n{body}"))
                }
            }
            Err(error) => text_err(format!("intent-consumed=true\n{error}")),
        }
    }

    #[tool(description = "Run olangc on a .O file (targets: ir, dot, script, wasm, or omit)")]
    async fn o_olangc(
        &self,
        Parameters(args): Parameters<OlangcArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let olangc = resolve_olangc(&root);
        let path = match resolve_file(&root, &args.path, "program") {
            Ok(path) => path,
            Err(error) => return text_err(error),
        };
        let mut argv: Vec<String> = vec![path.display().to_string()];
        if let Some(t) = &args.target {
            argv.push("--target".into());
            argv.push(t.clone());
        }
        if let Some(o) = &args.output {
            argv.push("-o".into());
            let output = PathBuf::from(o);
            argv.push(
                if output.is_absolute() {
                    output
                } else {
                    root.join(output)
                }
                .display()
                .to_string(),
            );
        }
        argv.push("--shim-dir".into());
        argv.push(backends.display().to_string());
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let timeout = args.timeout_secs.unwrap_or(180);
        match run_cmd(
            &olangc,
            &refs,
            Some(&root),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
            ],
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format_run(code, &stdout, &stderr);
                if code == 0 {
                    text_ok(body)
                } else {
                    text_err(body)
                }
            }
            Err(e) => text_err(e),
        }
    }

    #[tool(
        description = "Toolchain doctor: check O, olangc, backends, shims, all backend runtimes, and optional a18re search/o-run"
    )]
    async fn o_doctor(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let olangc = resolve_olangc(&root);
        let mut lines = vec![
            format!("O_LANG_ROOT={} exists={}", root.display(), root.is_dir()),
            format!(
                "O_BACKENDS_DIR={} exists={}",
                backends.display(),
                backends.is_dir()
            ),
            format!("O={} exists={}", o_bin.display(), o_bin.is_file()),
            format!("olangc={} exists={}", olangc.display(), olangc.is_file()),
            format!("python_shim={}", backends.join("python_shim.py").is_file()),
        ];
        // list a few shims
        if let Ok(rd) = std::fs::read_dir(&backends) {
            let mut shims: Vec<_> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with("_shim.py"))
                .collect();
            shims.sort();
            lines.push(format!("shims({}): {}", shims.len(), shims.join(", ")));
        }
        let a18 = home_dir().join("a18re");
        lines.push(format!(
            "a18re={} o-run={}",
            a18.is_dir(),
            a18.join("search/o-run").is_file()
        ));
        lines.push(
            discover_runtimes(&self.runtime_search, &root)
                .to_text()
                .trim_end()
                .to_string(),
        );
        text_ok(lines.join("\n") + "\n")
    }

    #[tool(
        description = "Run an a18re search tool by name (e.g. sptm_retype_catalog, nscramble_mine, lab_pipeline) with correct backends"
    )]
    async fn o_search_run(
        &self,
        Parameters(args): Parameters<SearchRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_lang_root();
        let backends = resolve_backends(&root);
        let o_bin = resolve_o_bin(&root);
        let requested_work = args
            .work
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("A18_WORK").map(PathBuf::from))
            .unwrap_or_else(|| home_dir().join("a18re"));
        let requested_work_text = requested_work.to_string_lossy();
        let work = match resolve_directory(&root, Some(&requested_work_text), "work") {
            Ok(path) => path,
            Err(error) => return text_err(error),
        };
        let requested_name = args.name.trim();
        let direct = PathBuf::from(requested_name);
        let direct = if direct.is_absolute() {
            direct
        } else {
            work.join(direct)
        };
        let tool_name = requested_name.trim_end_matches(".O");
        let candidate = if direct.is_file() {
            direct
        } else {
            work.join("search").join(format!("{tool_name}.O"))
        };
        let path = match resolve_file(
            candidate.parent().unwrap_or(&work),
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
            "search program",
        ) {
            Ok(path) => path,
            Err(_) => {
                return text_err(format!(
                    "not found: {} (tried search/{}.O under {})",
                    args.name,
                    tool_name,
                    work.display()
                ))
            }
        };
        // Refuse relative backends pitfalls: always pass absolute backends
        let timeout = args.timeout_secs.unwrap_or(300);
        match run_cmd(
            &o_bin,
            &[path.to_str().unwrap_or(""), backends.to_str().unwrap_or("")],
            Some(&work),
            &[
                ("O_LANG_ROOT", root.display().to_string()),
                ("O_BACKENDS_DIR", backends.display().to_string()),
                ("A18_WORK", work.display().to_string()),
            ],
            timeout,
        )
        .await
        {
            Ok((code, stdout, stderr)) => {
                let body = format!(
                    "program={}\nbackends={}\nwork={}\n{}",
                    path.display(),
                    backends.display(),
                    work.display(),
                    format_run(code, &stdout, &stderr)
                );
                if code == 0 {
                    text_ok(body)
                } else {
                    text_err(body)
                }
            }
            Err(e) => text_err(e),
        }
    }
}

#[tool_handler]
impl ServerHandler for OstadixMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Ostadix-lang / O-lang MCP (Rust). Use o_env/o_runtimes/o_doctor first. \
Use o_analyze_intent then o_execute_intent for a one-use same-intent gate; o_run remains direct ungated compatibility execution. \
Use o_information_inspect only for bounded descriptive reads of an existing local Information V1 head; it grants no authority. \
Always run .O programs through an MCP O tool so backends is absolute. \
Never pass the literal string O_BACKENDS_DIR; never put $VAR inside .O sources (O splices $IDENT)."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // GUI/MCP clients often launch with a system-only PATH. Restore known
    // local runtime locations once so discovery and every child backend see
    // the same executable universe.
    let root = resolve_lang_root();
    let runtime_search = runtime_search_path(&root)?;
    std::env::set_var("PATH", &runtime_search.encoded);

    // stderr only — stdout is MCP
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let server = OstadixMcp::new(runtime_search);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        catalog_backends_for, discover_runtimes, is_lang_root, parse_execution_intent,
        random_intent_handle, resolve_directory, resolve_file, resolve_information_state,
        resolve_o_info, resolve_run_target, run_cmd, run_information_inspect_bounded,
        runtime_search_path_with_mode, runtime_search_path_with_mode_and_manager_environment,
        sanitize_information_head_output, validate_information_head_name, validate_intent_target,
        EmptyArgs, InformationInspectRunError, IntentLease, IntentReservation, IntentStore,
        OstadixMcp, RuntimePathMode, RuntimeSearchPath, CATALOG_BACKEND_RUNTIMES,
        CATALOG_LEGACY_SCHEMA_V3, CATALOG_LEGACY_SCHEMA_V4, CATALOG_RUNTIME_REQUIREMENTS,
        CATALOG_SCHEMA, INTENT_SCHEMA_V1, MAX_LIVE_INTENTS,
    };
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is before Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "ostadix-mcp-path-test-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(path.join("backends")).expect("create backends fixture");
            fs::create_dir_all(path.join("examples/space dir")).expect("create examples fixture");
            fs::write(path.join("Cargo.toml"), "[workspace]\n").expect("write Cargo fixture");
            fs::write(path.join("backends/python_shim.py"), "# fixture\n")
                .expect("write shim fixture");
            fs::write(path.join("examples/hello.O"), "text^(ok)_text\n")
                .expect("write hello fixture");
            fs::write(path.join("examples/space dir/demo.O"), "text^(ok)_text\n")
                .expect("write path fixture");
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recognizes_only_complete_language_roots() {
        let fixture = Fixture::new();
        assert!(is_lang_root(&fixture.0));
        fs::remove_file(fixture.0.join("examples/hello.O")).expect("remove hello fixture");
        assert!(!is_lang_root(&fixture.0));
    }

    #[test]
    fn resolves_relative_cwd_then_program_once() {
        let fixture = Fixture::new();
        let cwd = resolve_directory(&fixture.0, Some("examples/space dir"), "cwd")
            .expect("resolve relative cwd");
        let program = resolve_file(&cwd, "demo.O", "program").expect("resolve relative program");
        assert_eq!(
            program,
            fixture
                .0
                .join("examples/space dir/demo.O")
                .canonicalize()
                .expect("canonical fixture program")
        );
    }

    #[test]
    fn missing_program_reports_effective_absolute_candidate() {
        let fixture = Fixture::new();
        let error = resolve_file(&fixture.0, "examples/missing.O", "program")
            .expect_err("missing program must fail");
        assert!(error.contains(&fixture.0.display().to_string()));
        assert!(error.contains("examples/missing.O"));
    }

    #[test]
    fn absolute_run_path_without_cwd_uses_program_parent() {
        let fixture = Fixture::new();
        let requested = fixture.0.join("examples/space dir/demo.O");
        let (program, cwd) = resolve_run_target(
            &fixture.0,
            requested.to_str().expect("fixture path is UTF-8"),
            None,
        )
        .expect("resolve absolute run target");
        assert_eq!(
            program,
            requested.canonicalize().expect("canonical program")
        );
        assert_eq!(
            cwd,
            requested
                .parent()
                .expect("program parent")
                .canonicalize()
                .expect("canonical program parent")
        );
    }

    #[test]
    fn runtime_inventory_is_a_complete_catalog_projection() {
        assert_eq!(CATALOG_SCHEMA, "ostadix.backend-catalog/v5");
        assert_eq!(CATALOG_LEGACY_SCHEMA_V4, "ostadix.backend-catalog/v4");
        assert_eq!(CATALOG_LEGACY_SCHEMA_V3, "ostadix.backend-catalog/v3");
        let requirement_keys = CATALOG_RUNTIME_REQUIREMENTS
            .iter()
            .map(|requirement| requirement.key)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            requirement_keys.len(),
            CATALOG_RUNTIME_REQUIREMENTS.len(),
            "canonical runtime requirement keys must be unique"
        );

        let backend_names = CATALOG_BACKEND_RUNTIMES
            .iter()
            .map(|backend| backend.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            backend_names.len(),
            CATALOG_BACKEND_RUNTIMES.len(),
            "canonical backend names must be unique"
        );

        for backend in CATALOG_BACKEND_RUNTIMES {
            assert!(
                requirement_keys.contains(backend.requirement_key),
                "backend {} references missing runtime requirement {}",
                backend.name,
                backend.requirement_key
            );
        }
        for requirement in CATALOG_RUNTIME_REQUIREMENTS {
            assert!(
                !catalog_backends_for(requirement.key).is_empty(),
                "runtime requirement {} is not referenced by a backend",
                requirement.key
            );
        }

        let stateless = CATALOG_BACKEND_RUNTIMES
            .iter()
            .filter(|backend| backend.state_support == "stateless")
            .count();
        let semantic = CATALOG_BACKEND_RUNTIMES
            .iter()
            .filter(|backend| backend.state_support == "semantic-snapshot")
            .map(|backend| backend.name)
            .collect::<Vec<_>>();
        let external = CATALOG_BACKEND_RUNTIMES
            .iter()
            .filter(|backend| backend.state_support == "external-pinned")
            .map(|backend| backend.name)
            .collect::<Vec<_>>();
        assert_eq!(stateless, 27);
        assert_eq!(semantic, ["sql", "python"]);
        assert_eq!(external, ["ubuntu_vm"]);

        let profiled = CATALOG_BACKEND_RUNTIMES
            .iter()
            .filter_map(|backend| {
                backend
                    .morphism_profile
                    .map(|profile| (backend.name, profile))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            profiled,
            [
                ("python", "python-plain-data"),
                ("rust", "rust-source-constant-stdout"),
                ("javascript", "javascript-binding-stdout"),
            ]
        );
        assert_eq!(
            CATALOG_BACKEND_RUNTIMES
                .iter()
                .filter(|backend| backend.morphism_profile.is_none())
                .count(),
            27
        );
    }

    #[test]
    fn runtime_inventory_projects_value_capabilities() {
        let python = CATALOG_BACKEND_RUNTIMES
            .iter()
            .find(|backend| backend.name == "python")
            .expect("python catalog entry");
        assert_eq!(python.integer_exactness, "arbitrary");
        assert_eq!(python.integer_exactness_bits, None);
        assert_eq!(python.integer_exactness_min, None);
        assert_eq!(python.integer_exactness_max, None);
        assert_eq!(python.rich_numbers, "preserved");
        assert_eq!(python.state_support, "semantic-snapshot");
        assert_eq!(python.state_codec, Some("ostadix.python-graph/v1"));
        assert_eq!(python.state_compatibility, Some("exact-implementation"));
        assert_eq!(python.state_manifest_schema, None);
        assert_eq!(python.morphism_profile, Some("python-plain-data"));

        let sql = CATALOG_BACKEND_RUNTIMES
            .iter()
            .find(|backend| backend.name == "sql")
            .expect("sql catalog entry");
        assert_eq!(sql.state_support, "semantic-snapshot");
        assert_eq!(sql.state_codec, Some("ostadix.sqlite-cli-main/v1"));
        assert_eq!(sql.state_compatibility, Some("exact-implementation"));

        let javascript = CATALOG_BACKEND_RUNTIMES
            .iter()
            .find(|backend| backend.name == "javascript")
            .expect("javascript catalog entry");
        assert_eq!(javascript.integer_exactness, "exact-magnitude-bits");
        assert_eq!(javascript.integer_exactness_bits, Some(53));
        assert_eq!(javascript.integer_exactness_min, None);
        assert_eq!(javascript.integer_exactness_max, None);
        assert_eq!(javascript.rich_numbers, "collapsed");
        assert_eq!(
            javascript.morphism_profile,
            Some("javascript-binding-stdout")
        );

        let java = CATALOG_BACKEND_RUNTIMES
            .iter()
            .find(|backend| backend.name == "java")
            .expect("java catalog entry");
        assert_eq!(java.integer_exactness, "twos-complement-bits");
        assert_eq!(java.integer_exactness_bits, Some(63));
        assert_eq!(java.integer_exactness_min, None);
        assert_eq!(java.integer_exactness_max, None);
        assert_eq!(java.state_support, "stateless");
        assert_eq!(java.morphism_profile, None);

        let ubuntu_vm = CATALOG_BACKEND_RUNTIMES
            .iter()
            .find(|backend| backend.name == "ubuntu_vm")
            .expect("ubuntu_vm catalog entry");
        assert_eq!(ubuntu_vm.state_support, "external-pinned");
        assert_eq!(ubuntu_vm.state_codec, None);
        assert_eq!(ubuntu_vm.state_compatibility, None);
        assert_eq!(
            ubuntu_vm.state_manifest_schema,
            Some("ostadix.multipass-resource/v1")
        );

        let range = catalog_integer_exactness!(ExactRange {
            min: "-10",
            max: "20"
        });
        assert_eq!(range, ("exact-range", None, Some("-10"), Some("20")));
    }

    #[test]
    fn runtime_inventory_output_exposes_profiles_with_an_explicit_nonclaim() {
        let fixture = Fixture::new();
        let search = RuntimeSearchPath::new(RuntimePathMode::InheritedOnly, Vec::new())
            .expect("empty inherited runtime search path");
        let output = discover_runtimes(&search, &fixture.0).to_text();
        assert!(output.contains("runtime-catalog-schema=ostadix.backend-catalog/v5\n"));
        assert!(output.contains("runtime-catalog-legacy-schema-v4=ostadix.backend-catalog/v4\n"));
        assert!(output.contains(
            "runtime-capability backend=python integer-exactness=arbitrary rich-numbers=preserved state-support=semantic-snapshot codec=ostadix.python-graph/v1 compatibility=exact-implementation morphism-profile=python-plain-data provenance=catalog"
        ));
        assert!(output.contains(
            "runtime-capability backend=javascript integer-exactness=exact-magnitude-bits:53 rich-numbers=collapsed state-support=stateless morphism-profile=javascript-binding-stdout provenance=catalog"
        ));
        assert!(output.contains(
            "runtime-capability backend=html integer-exactness=arbitrary rich-numbers=collapsed state-support=stateless morphism-profile=none provenance=catalog"
        ));
        assert!(output.contains(
            "runtime-note morphism profiles are bounded shadow descriptions; they do not authorize execution or claim generic backend crossings"
        ));
    }

    #[test]
    fn runtime_inventory_preserves_catalog_group_and_requirement_order() {
        assert_eq!(
            CATALOG_RUNTIME_REQUIREMENTS
                .iter()
                .map(|requirement| requirement.key)
                .collect::<Vec<_>>(),
            [
                "builtin",
                "python",
                "bash",
                "shell",
                "javascript",
                "ruby",
                "rust",
                "c",
                "cpp",
                "java",
                "nix",
                "nixos_test",
                "sql",
                "haskell",
                "ocaml",
                "racket",
                "common_lisp",
                "csharp",
                "matlab",
                "mathematica",
                "webassembly",
                "ubuntu_vm",
            ]
        );
        assert_eq!(
            catalog_backends_for("builtin"),
            ["O", "quote", "nix_expr", "html", "markdown", "latex", "text"]
        );
        assert_eq!(catalog_backends_for("nix"), ["nix", "nix_store"]);
        assert_eq!(catalog_backends_for("common_lisp"), ["lisp", "common_lisp"]);
        let webassembly = CATALOG_RUNTIME_REQUIREMENTS
            .iter()
            .find(|requirement| requirement.key == "webassembly")
            .expect("canonical WebAssembly runtime requirement");
        assert_eq!(webassembly.precision, "conservative-all-sources");
    }

    #[test]
    fn runtime_path_preserves_client_order_then_adds_local_fallbacks() {
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        let first = fixture.0.join("client-first");
        let second = fixture.0.join("client-second");
        let explicit = fixture.0.join("explicit-extra");
        for directory in [
            home.join(".local/bin"),
            fixture.0.join("target/release"),
            first.clone(),
            second.clone(),
            explicit.clone(),
        ] {
            fs::create_dir_all(directory).expect("create runtime path fixture");
        }
        let inherited = std::env::join_paths([&first, &second]).expect("join inherited PATH");
        let explicit_path = std::env::join_paths([&explicit]).expect("join explicit PATH");
        let search = runtime_search_path_with_mode(
            &fixture.0,
            &home,
            Some(OsStr::new(&inherited)),
            Some(OsStr::new(&explicit_path)),
            RuntimePathMode::DiscoverLocal,
        )
        .expect("construct discover-local runtime path");
        let paths = search
            .entries
            .iter()
            .map(|entry| entry.directory.clone())
            .collect::<Vec<_>>();

        assert_eq!(&paths[..3], &[first, second, explicit]);
        assert!(paths.contains(&fixture.0.join("target/release")));
        assert!(paths.contains(&home.join(".local/bin")));
        assert_eq!(paths.iter().collect::<BTreeSet<_>>().len(), paths.len());
        assert_eq!(search.mode, RuntimePathMode::DiscoverLocal);
        assert_eq!(search.entries[0].source, "inherited:0");
        assert_eq!(search.entries[2].source, "explicit:0");
        assert_eq!(search.entries[3].source, "repository-release");
    }

    #[test]
    fn runtime_path_modes_have_exact_visibility_and_provenance() {
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        let inherited_dir = fixture.0.join("inherited");
        let explicit_dir = fixture.0.join("explicit");
        for directory in [
            home.join(".local/bin"),
            fixture.0.join("target/release"),
            inherited_dir.clone(),
            explicit_dir.clone(),
        ] {
            fs::create_dir_all(directory).expect("create runtime path fixture");
        }
        let inherited = std::env::join_paths([&inherited_dir]).expect("join inherited PATH");
        let explicit = std::env::join_paths([&explicit_dir]).expect("join explicit PATH");

        let inherited_only = runtime_search_path_with_mode(
            &fixture.0,
            &home,
            Some(OsStr::new(&inherited)),
            Some(OsStr::new(&explicit)),
            RuntimePathMode::InheritedOnly,
        )
        .expect("construct inherited-only path");
        assert_eq!(
            inherited_only.entries,
            [super::RuntimePathEntry {
                directory: inherited_dir.clone(),
                source: "inherited:0".to_string(),
            }]
        );

        let inherited_plus_explicit = runtime_search_path_with_mode(
            &fixture.0,
            &home,
            Some(OsStr::new(&inherited)),
            Some(OsStr::new(&explicit)),
            RuntimePathMode::InheritedPlusExplicit,
        )
        .expect("construct inherited-plus-explicit path");
        assert_eq!(inherited_plus_explicit.entries.len(), 2);
        assert_eq!(inherited_plus_explicit.entries[1].directory, explicit_dir);
        assert_eq!(inherited_plus_explicit.entries[1].source, "explicit:0");

        let discover_local = runtime_search_path_with_mode(
            &fixture.0,
            &home,
            Some(OsStr::new(&inherited)),
            Some(OsStr::new(&explicit)),
            RuntimePathMode::DiscoverLocal,
        )
        .expect("construct discover-local path");
        assert_eq!(discover_local.entries[0].directory, inherited_dir);
        assert_eq!(discover_local.entries[1].directory, explicit_dir);
        assert!(discover_local
            .entries
            .iter()
            .any(|entry| entry.source == "repository-release"));
        assert!(discover_local
            .entries
            .iter()
            .any(|entry| entry.source == "home-local-bin"));
    }

    #[test]
    fn runtime_path_deduplication_preserves_first_source() {
        let fixture = Fixture::new();
        let shared = fixture.0.join("shared");
        fs::create_dir_all(&shared).expect("create shared runtime directory");
        let inherited = std::env::join_paths([&shared]).expect("join inherited PATH");
        let explicit = std::env::join_paths([&shared]).expect("join explicit PATH");
        let search = runtime_search_path_with_mode(
            &fixture.0,
            &fixture.0.join("home"),
            Some(OsStr::new(&inherited)),
            Some(OsStr::new(&explicit)),
            RuntimePathMode::InheritedPlusExplicit,
        )
        .expect("construct deduplicated runtime path");

        assert_eq!(search.entries.len(), 1);
        assert_eq!(search.entries[0].source, "inherited:0");
        assert_eq!(
            search.source_for_executable(&shared.join("python3")),
            "inherited:0"
        );
    }

    #[test]
    fn runtime_manager_environment_precedes_generic_system_fallbacks() {
        let fixture = Fixture::new();
        let java_home = fixture.0.join("selected-java");
        fs::create_dir_all(java_home.join("bin")).expect("create selected JAVA_HOME bin");
        let manager_environment = vec![(
            std::ffi::OsString::from("JAVA_HOME"),
            java_home.as_os_str().to_os_string(),
        )];
        let search = runtime_search_path_with_mode_and_manager_environment(
            &fixture.0,
            &fixture.0.join("home"),
            None,
            None,
            RuntimePathMode::DiscoverLocal,
            &manager_environment,
        )
        .expect("construct discover-local path with selected manager root");
        let manager_index = search
            .entries
            .iter()
            .position(|entry| entry.source == "manager-env:JAVA_HOME")
            .expect("JAVA_HOME must be represented");
        let first_system_index = search
            .entries
            .iter()
            .position(|entry| entry.source.starts_with("system-fallback:"))
            .expect("at least one generic system fallback exists on the test host");
        assert!(manager_index < first_system_index);
    }

    #[test]
    fn runtime_path_mode_rejects_unknown_values() {
        let error = "implicit-cloud"
            .parse::<RuntimePathMode>()
            .expect_err("unknown runtime path policy must fail closed");
        assert!(error
            .to_string()
            .contains("invalid OSTADIX_RUNTIME_PATH_MODE"));
    }

    #[test]
    fn empty_tool_args_emit_strict_object_schema() {
        let schema = rmcp::handler::server::tool::schema_for_type::<EmptyArgs>();
        assert_eq!(schema.get("type"), Some(&serde_json::json!("object")));
        assert_eq!(schema.get("properties"), Some(&serde_json::json!({})));
    }

    #[test]
    fn parses_only_well_formed_v1_execution_intents() {
        let digest = "a".repeat(64);
        let document = parse_execution_intent(&format!(
            "{{\"schema\":\"{INTENT_SCHEMA_V1}\",\"source_sha256\":\"{digest}\",\"execution_intent_sha256\":\"{digest}\"}}"
        ))
        .expect("parse valid execution intent");
        assert_eq!(document.schema, INTENT_SCHEMA_V1);

        let wrong_schema = parse_execution_intent(&format!(
            "{{\"schema\":\"oexec.execution-intent/v999\",\"source_sha256\":\"{digest}\",\"execution_intent_sha256\":\"{digest}\"}}"
        ))
        .expect_err("unknown schema must fail closed");
        assert!(wrong_schema.contains("unsupported execution-intent schema"));

        let malformed = parse_execution_intent(&format!(
            "{{\"schema\":\"{INTENT_SCHEMA_V1}\",\"source_sha256\":\"short\",\"execution_intent_sha256\":\"{digest}\"}}"
        ))
        .expect_err("malformed digest must fail closed");
        assert!(malformed.contains("malformed execution-intent digest"));
    }

    #[test]
    fn information_head_output_is_strictly_parsed_and_state_path_is_not_returned() {
        let revision = "a".repeat(64);
        let snapshot = "b".repeat(64);
        let fact = "c".repeat(64);
        let raw = format!(
            "head state=/private/location with spaces name=main revision={revision} snapshot={snapshot} facts=1\nfact={fact}\nauthority=information presence and signatures grant no execution authority\n"
        );
        let sanitized = sanitize_information_head_output(&raw, "main").unwrap();
        assert!(!sanitized.contains("/private/location"));
        assert!(sanitized.contains(&format!("revision={revision}")));
        assert!(sanitized.contains(&format!("fact={fact}")));
        assert!(sanitized.contains("source=local-o-info-read-only"));

        let duplicate = format!(
            "head state=/state name=main revision={revision} snapshot={snapshot} facts=2\nfact={fact}\nfact={fact}\nauthority=information presence and signatures grant no execution authority\n"
        );
        assert!(sanitize_information_head_output(&duplicate, "main").is_err());
        let unexpected = format!(
            "head state=/state name=main revision={revision} snapshot={snapshot} facts=0\ncapability=forged\nauthority=information presence and signatures grant no execution authority\n"
        );
        assert!(sanitize_information_head_output(&unexpected, "main").is_err());
    }

    #[test]
    fn information_head_arguments_are_fixed_bounded_tokens() {
        assert!(validate_information_head_name("main").is_ok());
        assert!(validate_information_head_name("../main").is_err());
        assert!(validate_information_head_name("main\nforged").is_err());
        assert!(validate_information_head_name(&"x".repeat(129)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn information_state_resolution_rejects_symlink_and_missing_roots_without_creation() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let state = fixture.0.join("state");
        fs::create_dir(&state).unwrap();
        let link = fixture.0.join("state-link");
        symlink(&state, &link).unwrap();
        assert!(resolve_information_state(&fixture.0, link.to_str().unwrap()).is_err());

        let missing = fixture.0.join("missing-state");
        assert!(resolve_information_state(&fixture.0, missing.to_str().unwrap()).is_err());
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn fixed_information_inspector_binary_rejects_final_component_symlink() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let release = fixture.0.join("target/release");
        fs::create_dir_all(&release).unwrap();
        symlink("/bin/sh", release.join("o-info")).unwrap();
        let error = resolve_o_info(&fixture.0).unwrap_err();
        assert!(error.contains("must not be a symlink"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn information_inspector_kills_oversized_stdout_and_stderr() {
        let cwd = PathBuf::from("/");
        let stdout_error =
            run_information_inspect_bounded(PathBuf::from("/usr/bin/yes").as_path(), &[], &cwd, 5)
                .await
                .expect_err("unbounded stdout must be killed at the inspection cap");
        assert_eq!(stdout_error, InformationInspectRunError::StdoutLimit);

        let stderr_error = run_information_inspect_bounded(
            PathBuf::from("/bin/sh").as_path(),
            &["-c", "while :; do printf x >&2; done"],
            &cwd,
            5,
        )
        .await
        .expect_err("unbounded stderr must be killed at the inspection cap");
        assert_eq!(stderr_error, InformationInspectRunError::StderrLimit);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn information_inspector_timeout_kills_descendants() {
        let fixture = Fixture::new();
        let sentinel = fixture.0.join("late-information-inspector-write");
        let command = format!(
            "(/bin/sleep 2; printf late > '{}') & wait",
            sentinel.display()
        );
        let error = run_information_inspect_bounded(
            PathBuf::from("/bin/sh").as_path(),
            &["-c", &command],
            &fixture.0,
            1,
        )
        .await
        .expect_err("information inspector must time out");
        assert_eq!(error, InformationInspectRunError::Timeout);
        tokio::time::sleep(Duration::from_millis(1_250)).await;
        assert!(!sentinel.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn information_inspector_clears_the_inherited_environment() {
        let result = run_information_inspect_bounded(
            PathBuf::from("/bin/sh").as_path(),
            &[
                "-c",
                "if [ -z \"${HOME+x}\" ]; then printf cleared; else printf inherited; fi",
            ],
            PathBuf::from("/").as_path(),
            5,
        )
        .await
        .unwrap();
        assert_eq!(result, (0, "cleared".to_string(), String::new()));
    }

    #[test]
    fn intent_store_is_expiring_and_one_use() {
        let fixture = Fixture::new();
        let now = Instant::now();
        let lease = IntentLease {
            program: fixture.0.join("examples/hello.O"),
            cwd: fixture.0.clone(),
            root: fixture.0.clone(),
            backends: fixture.0.join("backends"),
            source_sha256: "a".repeat(64),
            execution_intent_sha256: "b".repeat(64),
            expires_at: now + Duration::from_secs(1),
        };
        let mut store = IntentStore::default();
        store
            .reserve("one".to_string(), now)
            .expect("reserve lease slot");
        store
            .insert_reserved("one".to_string(), lease.clone())
            .expect("insert lease");
        let taken = store.take("one", now).expect("consume lease once");
        assert_eq!(taken.execution_intent_sha256, "b".repeat(64));
        assert!(store.take("one", now).unwrap_err().contains("consumed"));

        let mut expired = lease;
        expired.expires_at = now;
        store
            .reserve("expired".to_string(), now - Duration::from_secs(1))
            .expect("reserve expired lease slot before its expiry");
        store
            .insert_reserved("expired".to_string(), expired)
            .expect("insert lease before its expiry");
        assert_eq!(
            store.take("expired", now).unwrap_err(),
            "execution-intent handle expired"
        );
    }

    #[test]
    fn intent_store_reservations_bound_in_progress_analysis_capacity() {
        let now = Instant::now();
        let mut store = IntentStore::default();
        for index in 0..MAX_LIVE_INTENTS {
            store
                .reserve(format!("reservation-{index}"), now)
                .expect("reserve bounded in-progress intent analysis");
        }
        assert!(store
            .reserve("overflow".to_string(), now)
            .unwrap_err()
            .contains("store is full"));

        store.cancel_reservation("reservation-0");
        store
            .reserve("replacement".to_string(), now)
            .expect("released reservation restores one capacity slot");
    }

    #[test]
    fn dropped_intent_reservation_releases_in_progress_capacity() {
        let runtime_search = super::RuntimeSearchPath::new(RuntimePathMode::InheritedOnly, vec![])
            .expect("construct empty runtime search path");
        let server = OstadixMcp::new(runtime_search);
        {
            let _reservation = IntentReservation::acquire(
                server.intents.clone(),
                "cancelled".to_string(),
                Instant::now(),
            )
            .expect("reserve intent analysis capacity");
            assert_eq!(server.intents.lock().unwrap().reservations.len(), 1);
        }
        assert!(server.intents.lock().unwrap().reservations.is_empty());
    }

    #[test]
    fn intent_target_comparison_binds_program_cwd_root_and_backends() {
        let fixture = Fixture::new();
        let program = fixture
            .0
            .join("examples/hello.O")
            .canonicalize()
            .expect("canonical program");
        let cwd = fixture.0.canonicalize().expect("canonical root");
        let backends = fixture
            .0
            .join("backends")
            .canonicalize()
            .expect("canonical backends");
        let lease = IntentLease {
            program: program.clone(),
            cwd: cwd.clone(),
            root: cwd.clone(),
            backends: backends.clone(),
            source_sha256: "a".repeat(64),
            execution_intent_sha256: "b".repeat(64),
            expires_at: Instant::now() + Duration::from_secs(1),
        };
        validate_intent_target(&lease, &program, &cwd, &cwd, &backends).expect("matching target");
        let mismatch = validate_intent_target(
            &lease,
            &fixture.0.join("examples/space dir/demo.O"),
            &cwd,
            &cwd,
            &backends,
        )
        .expect_err("program mismatch must fail closed");
        assert!(mismatch.contains("program mismatch"));
    }

    #[test]
    fn intent_handles_use_os_entropy_and_fixed_hex_encoding() {
        let first = random_intent_handle().expect("first handle");
        let second = random_intent_handle().expect("second handle");
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_backend_descendants_before_they_can_commit() {
        let fixture = Fixture::new();
        let sentinel = fixture.0.join("late-backend-write");
        let command = format!("(sleep 2; printf late > '{}') & wait", sentinel.display());
        let error = run_cmd(
            PathBuf::from("/bin/sh").as_path(),
            &["-c", &command],
            None,
            &[],
            1,
        )
        .await
        .expect_err("backend process group must time out");
        assert_eq!(error, "timeout after 1s");
        tokio::time::sleep(std::time::Duration::from_millis(1_250)).await;
        assert!(
            !sentinel.exists(),
            "a backend descendant survived the timeout and wrote {}",
            sentinel.display()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_covers_pipe_drain_after_group_leader_exits() {
        let fixture = Fixture::new();
        let sentinel = fixture.0.join("late-write-after-leader-exit");
        let command = format!("(sleep 2; printf late > '{}') & exit 0", sentinel.display());
        let error = run_cmd(
            PathBuf::from("/bin/sh").as_path(),
            &["-c", &command],
            None,
            &[],
            1,
        )
        .await
        .expect_err("pipe drain and child wait must share one timeout");
        assert_eq!(error, "timeout after 1s");
        tokio::time::sleep(std::time::Duration::from_millis(1_250)).await;
        assert!(
            !sentinel.exists(),
            "a descendant retained the pipes and survived the timeout: {}",
            sentinel.display()
        );
    }
}
