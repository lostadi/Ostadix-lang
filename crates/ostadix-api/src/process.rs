use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, BufReader, BufWriter, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(unix))]
use std::sync::OnceLock;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::backend_state::{
    ensure_evaluator_snapshot_bound, sandbox_policy_sha256, BackendCheckpointV1,
    BackendRestoreReceiptV1, BackendStateCapabilitiesV1, BackendStateTierV1, BackendWireCommandV2,
    BackendWireResponseV2, EvaluatorActorCheckpointV1, EvaluatorStateSnapshotV1,
};
use crate::capability::BackendSandboxPolicy;
use crate::value::OValue;
use crate::wire;

static LIFECYCLE_TRACE_LOCK: Mutex<()> = Mutex::new(());
static BACKEND_SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
const DEFAULT_BACKEND_OPERATION_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_BACKEND_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const BACKEND_FALLBACK_REAP_TIMEOUT: Duration = Duration::from_millis(250);
const DEFAULT_MAX_OPEN_BACKEND_SESSIONS: usize = 128;
const DEFAULT_MAX_OPEN_BACKEND_SESSIONS_PER_BACKEND: usize = 32;

/// Marker carried through `anyhow` so the worker pool can distinguish a
/// physical execution failure from a language-level backend error.
#[derive(Debug)]
struct BackendInfrastructureError {
    source: anyhow::Error,
}

impl fmt::Display for BackendInfrastructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.source)
    }
}

impl StdError for BackendInfrastructureError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
struct BackendSemanticError(String);

impl fmt::Display for BackendSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl StdError for BackendSemanticError {}

/// A checkpoint was refused without damaging the live session. Callers may
/// continue on the same actor, but must not claim restart or migration.
#[derive(Debug)]
pub(crate) struct BackendStatePinned {
    pub(crate) backend: String,
    pub(crate) path: String,
    pub(crate) message: String,
}

impl fmt::Display for BackendStatePinned {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "state.pin-required: backend={} path={} message={}",
            self.backend, self.path, self.message
        )
    }
}

impl StdError for BackendStatePinned {}

/// A one-shot compatibility backend completed its semantic operation but
/// could not participate in the explicit shutdown handshake.  This marker is
/// intentionally narrower than "the process is now terminal": protocol-shape
/// errors and acknowledged shutdowns with lingering descendants must remain
/// failures even when forced cleanup eventually succeeds.
#[derive(Debug)]
struct BackendShutdownUnacknowledged(String);

impl fmt::Display for BackendShutdownUnacknowledged {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl StdError for BackendShutdownUnacknowledged {}

pub(crate) fn infrastructure_error(error: anyhow::Error) -> anyhow::Error {
    if error.is::<BackendInfrastructureError>() {
        error
    } else {
        anyhow::Error::new(BackendInfrastructureError { source: error })
    }
}

pub(crate) fn is_infrastructure_error(error: &anyhow::Error) -> bool {
    error.is::<BackendInfrastructureError>()
}

/// Append one best-effort process-lifecycle event when diagnostics are enabled.
///
/// `O_LIFECYCLE_TRACE` names the trace file. Events intentionally contain no
/// source or value payloads, and tracing failure never changes execution.
pub(crate) fn lifecycle_trace(event: &str, detail: impl AsRef<str>) {
    let Some(path) = std::env::var_os("O_LIFECYCLE_TRACE") else {
        return;
    };
    let detail = detail.as_ref().replace(['\n', '\r'], " ");
    let line = format!(
        "monotonic_ns={} pid={} thread={:?} event={} {}\n",
        monotonic_nanos(),
        std::process::id(),
        std::thread::current().id(),
        event,
        detail,
    );
    let _guard = LIFECYCLE_TRACE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(parent) = Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(unix)]
fn monotonic_nanos() -> u128 {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timestamp` is valid writable storage for one `timespec`, and
    // CLOCK_MONOTONIC requires no additional caller-managed lifetime.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } == 0 {
        (timestamp.tv_sec as u128)
            .saturating_mul(1_000_000_000)
            .saturating_add(timestamp.tv_nsec as u128)
    } else {
        0
    }
}

#[cfg(not(unix))]
fn monotonic_nanos() -> u128 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos()
}

#[cfg(test)]
const PYTHON_POLICY_BOOTSTRAP: &str = r#"
import json, os, runpy, sys
_o_permissions = frozenset(json.loads(os.environ.pop("O_BACKEND_AUTHORITIES", "[]")))
_o_runtime_candidates = json.loads(os.environ.pop("O_BACKEND_RUNTIME_ROOTS", "[]"))
_o_import_roots = list(_o_runtime_candidates)
_o_runtime_candidates.extend(path for path in sys.path if path)
_o_runtime_candidates.extend((sys.prefix, sys.base_prefix, os.path.dirname(sys.executable)))

def _o_realpath_or_none(path):
    try:
        return os.path.realpath(os.fspath(path))
    except (TypeError, ValueError, OSError):
        return None

_o_runtime_roots = tuple(sorted({
    real
    for path in _o_runtime_candidates
    if path
    for real in [_o_realpath_or_none(path)]
    if real
}))

def _o_under(path, roots):
    try:
        real = os.path.realpath(os.fspath(path))
    except (TypeError, ValueError, OSError):
        return False
    return any(real == root or real.startswith(root + os.sep) for root in roots)

for _o_root in reversed(_o_import_roots):
    _o_real_root = _o_realpath_or_none(_o_root)
    if _o_real_root and _o_real_root not in sys.path:
        sys.path.insert(0, _o_real_root)

def _o_audit(event, args):
    if event == "open" and args:
        path = args[0]
        if isinstance(path, int):
            return
        mode = args[1] if len(args) > 1 else "r"
        flags = args[2] if len(args) > 2 and isinstance(args[2], int) else 0
        writing = (
            isinstance(mode, str) and any(marker in mode for marker in "wax+")
        ) or bool(flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC | os.O_APPEND))
        if writing and "fs_write" not in _o_permissions:
            raise PermissionError("O backend capability denies filesystem write")
        if (
            not writing
            and "fs_read" not in _o_permissions
            and not _o_under(path, _o_runtime_roots)
        ):
            raise PermissionError("O backend capability denies filesystem read")
    if event in {
        "os.listdir", "os.scandir"
    } and "fs_read" not in _o_permissions and args and not _o_under(args[0], _o_runtime_roots):
        raise PermissionError("O backend capability denies filesystem read")
    if event in {
        "os.remove", "os.rename", "os.rmdir", "os.mkdir", "os.chmod",
        "os.chown", "os.link", "os.symlink", "os.truncate", "os.utime"
    } and "fs_write" not in _o_permissions:
        raise PermissionError("O backend capability denies filesystem write")
    if (event in {
        "os.system", "os.fork", "os.forkpty", "os.posix_spawn",
        "os.posix_spawnp", "subprocess.Popen", "pty.spawn"
    } or event.startswith("os.exec") or event.startswith("os.spawn")) and "process" not in _o_permissions:
        raise PermissionError("O backend capability denies process spawn")
    if (event.startswith("socket.") or event.startswith("ssl.")) and "network" not in _o_permissions:
        raise PermissionError("O backend capability denies network access")
    if event == "ctypes.dlopen" and set(_o_permissions) != {"fs_read", "fs_write", "network", "process"}:
        raise PermissionError("O backend capability denies native-library loading under a restricted policy")

sys.addaudithook(_o_audit)
runpy.run_path(sys.argv[1], run_name="__main__")
"#;

/// One step in the exec-reply cycle.
///
/// After sending an `Exec` command to a shim, the runtime reads one response.
/// If the shim is done, it sends `Ok` or `Err`. If the shim's user code called
/// `O.eval(q)`, the shim sends `EvalRequest` and expects the runtime to
/// evaluate the quoted source and reply with an `EvalResult` command before
/// the shim resumes execution and eventually sends `Ok`/`Err`.
#[derive(Debug)]
pub enum ExecStep {
    /// The shim finished executing and returned a value.
    Done(OValue),
    /// The shim needs the runtime to evaluate an O source fragment. `scope` is
    /// an optional explicit OValue::Scope supplied by user code.
    EvalRequest { src: String, scope: Option<OValue> },
}

struct BackendProcess {
    language: String,
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    responses: mpsc::Receiver<std::result::Result<BackendWireResponseV2, String>>,
    reader: Option<JoinHandle<()>>,
    terminal: bool,
    exec_pending: bool,
}

pub(crate) fn backend_operation_timeout() -> Duration {
    duration_from_env(
        "O_BACKEND_OPERATION_TIMEOUT_MS",
        DEFAULT_BACKEND_OPERATION_TIMEOUT,
        Duration::from_secs(60 * 60),
    )
}

pub(crate) fn backend_shutdown_timeout() -> Duration {
    duration_from_env(
        "O_BACKEND_SHUTDOWN_TIMEOUT_MS",
        DEFAULT_BACKEND_SHUTDOWN_TIMEOUT,
        Duration::from_secs(60),
    )
}

fn duration_from_env(name: &str, default: Duration, maximum: Duration) -> Duration {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    raw.parse::<u64>()
        .ok()
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .map(|duration| duration.min(maximum))
        .unwrap_or(default)
}

fn bounded_deadline(timeout: Duration, subject: &str) -> Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| anyhow!("{subject} deadline overflowed"))
}

/// Return true when a non-authoritative `/proc` observation raced with normal
/// process exit. Callers must continue to surface every other I/O error.
#[cfg(target_os = "linux")]
pub(crate) fn linux_process_observation_disappeared(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

/// Ostadix POSIX v1 containment governs descendants that remain in the
/// backend's inherited process group. A descendant that deliberately creates
/// a new session or process group (for example with `setsid`) escapes this v1
/// boundary; complete containment requires a stronger OS facility such as a
/// Linux cgroup or a Windows job object.
#[cfg(target_os = "linux")]
fn owned_group_has_no_active_descendants(group: i32) -> io::Result<bool> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if linux_process_observation_disappeared(&error) => continue,
            Err(error) => return Err(error),
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == group {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if linux_process_observation_disappeared(&error) => continue,
            Err(error) => return Err(error),
        };
        let close = stat.rfind(')').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed /proc process stat")
        })?;
        let mut fields = stat[close + 1..].split_whitespace();
        let state = fields.next();
        let _parent = fields.next();
        let candidate_group = fields
            .next()
            .and_then(|field| field.parse::<i32>().ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "malformed /proc process group")
            })?;
        // Orphaned grandchildren cannot be waited by this process. Zombies
        // and Linux's transient dead states have no executable state and
        // cannot retain backend protocol descriptors, so they are terminal
        // for this physical-completion boundary.
        if candidate_group == group && !matches!(state, Some("Z" | "X" | "x")) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
fn owned_group_has_no_active_descendants(group: i32) -> io::Result<bool> {
    // SAFETY: a null buffer asks libproc for a conservative PID capacity and
    // does not dereference memory.
    let capacity = unsafe { libc::proc_listpgrppids(group, std::ptr::null_mut(), 0) };
    if capacity <= 0 {
        let enumeration_error = io::Error::last_os_error();
        // After the anchored leader is reaped, libproc can report no result
        // without a useful errno. Confirm absence through the process-group
        // namespace before treating that as quiescence.
        // SAFETY: signal zero performs existence/permission checks only.
        if unsafe { libc::kill(-group, 0) } != 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return Ok(true);
        }
        return Err(enumeration_error);
    }
    let mut members = vec![0 as libc::pid_t; capacity as usize];
    let bytes = members
        .len()
        .checked_mul(std::mem::size_of::<libc::pid_t>())
        .and_then(|size| i32::try_from(size).ok())
        .ok_or_else(|| io::Error::other("Darwin process-group buffer is too large"))?;
    // SAFETY: `members` owns `bytes` writable bytes and remains alive for the
    // complete libproc call.
    let count = unsafe { libc::proc_listpgrppids(group, members.as_mut_ptr().cast(), bytes) };
    if count <= 0 {
        let enumeration_error = io::Error::last_os_error();
        // SAFETY: signal zero performs existence/permission checks only.
        if unsafe { libc::kill(-group, 0) } != 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return Ok(true);
        }
        return Err(enumeration_error);
    }
    let count = usize::try_from(count)
        .map_err(|_| io::Error::other("Darwin process-group count does not fit usize"))?;
    if count > members.len() {
        return Err(io::Error::other(
            "Darwin process-group enumeration exceeded its buffer",
        ));
    }
    members.truncate(count);
    for member in members.into_iter().filter(|member| *member != group) {
        // SAFETY: `information` is valid writable storage of the exact size
        // supplied to libproc for the queried PID.
        let mut information: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let information_size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
            .map_err(|_| io::Error::other("Darwin process info buffer is too large"))?;
        let read = unsafe {
            libc::proc_pidinfo(
                member,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut information as *mut libc::proc_bsdinfo).cast(),
                information_size,
            )
        };
        if read <= 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error);
        }
        if read != information_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Darwin returned a partial process info record",
            ));
        }
        // Revalidate the PGID after enumeration to reject PID reuse. Zombies
        // are inert and cannot retain backend protocol descriptors.
        if information.pbi_pgid == group as u32 && information.pbi_status != libc::SZOMB {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn owned_group_has_no_active_descendants(_group: i32) -> io::Result<bool> {
    // POSIX exposes group signalling but no portable membership-enumeration
    // API. The shutdown path still kills the inherited group on failure, but
    // the stronger active-descendant proof is currently Linux/Darwin only.
    Ok(true)
}

#[cfg(not(unix))]
fn owned_group_has_no_active_descendants(_group: i32) -> io::Result<bool> {
    Ok(true)
}

fn signal_owned_process_group(child: &mut Child) -> Result<()> {
    #[cfg(unix)]
    {
        if let Ok(group) = i32::try_from(child.id()) {
            // SAFETY: backend children are created as leaders of their own
            // process groups; a negative PID addresses only that owned group.
            if unsafe { libc::kill(-group, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    let _ = child.kill();
                    return Err(error).context("failed to terminate backend process group");
                }
            }
        }
    }
    let _ = child.kill();
    Ok(())
}

fn kill_and_reap_process_group(child: &mut Child, timeout: Duration) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let signal_result = signal_owned_process_group(child);
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| anyhow!("backend reap deadline overflowed"))?;
    loop {
        if child.try_wait()?.is_some() {
            return signal_result;
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "backend process {} did not become waitable within {} ms",
                child.id(),
                timeout.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
fn python_shim_command(shim_path: &Path, sandbox: &BackendSandboxPolicy) -> Result<Command> {
    let python = which::which("python3").context("python3 is required for backend shims")?;
    let shim = shim_path
        .canonicalize()
        .unwrap_or_else(|_| shim_path.to_path_buf());
    let runtime_root = shim
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    #[cfg(target_os = "macos")]
    let mut command = macos_sandbox_command(&python, sandbox, &runtime_root)?;
    #[cfg(not(target_os = "macos"))]
    let mut command = Command::new(&python);

    // A no-file-read macOS profile denies metadata traversal of the caller's
    // arbitrary working directory. Python asks for getcwd() during bootstrap,
    // so anchor it inside the already admitted immutable shim runtime root.
    // Relative user-file access remains unavailable under the same profile.
    #[cfg(target_os = "macos")]
    if !sandbox.contains(crate::value::BackendAuthority::FileRead) {
        command.current_dir(&runtime_root);
    }

    command
        .arg("-c")
        .arg(PYTHON_POLICY_BOOTSTRAP)
        .arg(&shim)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env(
            "O_BACKEND_AUTHORITIES",
            serde_json::to_string(&sandbox.names())?,
        )
        .env(
            "O_BACKEND_RUNTIME_ROOTS",
            serde_json::to_string(std::slice::from_ref(&runtime_root))?,
        );
    Ok(command)
}

#[cfg(test)]
fn direct_shim_command(shim_path: &Path, sandbox: &BackendSandboxPolicy) -> Command {
    #[cfg(not(target_os = "macos"))]
    let _ = sandbox;
    #[cfg(target_os = "macos")]
    if let Ok(command) = macos_sandbox_command(
        shim_path,
        sandbox,
        shim_path.parent().unwrap_or_else(|| Path::new(".")),
    ) {
        return command;
    }
    Command::new(shim_path)
}

#[cfg(not(test))]
fn rust_backend_command(
    lang: &str,
    shim_path: &Path,
    sandbox: &BackendSandboxPolicy,
    executable_leases: &crate::runtime_exec::ExecutableLeaseSet,
) -> Result<Command> {
    executable_leases.verify_backend(lang)?;
    #[cfg(target_os = "macos")]
    let executable = executable_leases.current_o_path()?.to_path_buf();
    #[cfg(not(target_os = "macos"))]
    let executable = executable_leases.current_o_invocation_path()?.to_path_buf();
    let runtime_root = if shim_path.exists() {
        shim_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        executable
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };

    #[cfg(target_os = "macos")]
    let mut command = macos_sandbox_command_with_launcher(
        executable_leases
            .sandbox_exec_path()?
            .ok_or_else(|| anyhow!("admission omitted the macOS sandbox-exec artifact"))?,
        &executable,
        sandbox,
        &runtime_root,
    )?;
    #[cfg(not(target_os = "macos"))]
    let mut command = executable_leases.current_o_command()?;

    command
        .arg("--o-backend")
        .arg(lang)
        .env(
            crate::runtime_exec::ADMITTED_EXECUTABLE_MANIFEST_ENV,
            executable_leases.backend_manifest_json(lang)?,
        )
        .env(
            "O_BACKEND_AUTHORITIES",
            serde_json::to_string(&sandbox.names())?,
        );
    if shim_path.exists() {
        command.env("O_BACKEND_LEGACY_SHIM", shim_path);
        command.env(
            "O_BACKEND_RUNTIME_ROOTS",
            serde_json::to_string(&[runtime_root])?,
        );
    }
    Ok(command)
}

#[cfg(test)]
fn legacy_backend_command(shim_path: &Path, sandbox: &BackendSandboxPolicy) -> Result<Command> {
    if !shim_path.exists() {
        return Err(anyhow!("backend shim not found: {}", shim_path.display()));
    }
    if shim_path.extension().and_then(|s| s.to_str()) == Some("py") {
        python_shim_command(shim_path, sandbox)
    } else {
        Ok(direct_shim_command(shim_path, sandbox))
    }
}

#[cfg(all(test, target_os = "macos"))]
fn macos_sandbox_command(
    executable: &Path,
    sandbox: &BackendSandboxPolicy,
    runtime_root: &Path,
) -> Result<Command> {
    macos_sandbox_command_with_launcher(
        Path::new("/usr/bin/sandbox-exec"),
        executable,
        sandbox,
        runtime_root,
    )
}

#[cfg(target_os = "macos")]
fn macos_sandbox_command_with_launcher(
    sandbox_launcher: &Path,
    executable: &Path,
    sandbox: &BackendSandboxPolicy,
    runtime_root: &Path,
) -> Result<Command> {
    let executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_path_buf());
    let mut profile = String::from("(version 1)\n(allow default)\n");
    if !sandbox.contains(crate::value::BackendAuthority::Network) {
        profile.push_str("(deny network*)\n");
    }
    if !sandbox.contains(crate::value::BackendAuthority::Process) {
        profile.push_str("(deny process-fork)\n(deny process-exec)\n");
        let executable_root = executable
            .ancestors()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("python@"))
            })
            .unwrap_or(&executable);
        profile.push_str(&format!(
            "(allow process-exec (literal \"{}\") (subpath \"{}\"))\n",
            sandbox_quote(&executable),
            sandbox_quote(executable_root)
        ));
    }
    if !sandbox.contains(crate::value::BackendAuthority::FileWrite) {
        profile.push_str("(deny file-write*)\n");
    }
    if !sandbox.contains(crate::value::BackendAuthority::FileRead) {
        profile.push_str(
            "(deny file-read-data (subpath \"/Users\") (subpath \"/home\") (subpath \"/root\"))\n",
        );
        profile.push_str(&format!(
            "(allow file-read-data (subpath \"{}\"))\n",
            sandbox_quote(runtime_root)
        ));
    }

    let mut command = Command::new(sandbox_launcher);
    command.arg("-p").arg(profile).arg(executable);
    Ok(command)
}

#[cfg(target_os = "macos")]
fn sandbox_quote(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

impl BackendProcess {
    fn new(
        lang: &str,
        shim_path: &Path,
        sandbox: &BackendSandboxPolicy,
        executable_leases: Option<&crate::runtime_exec::ExecutableLeaseSet>,
    ) -> Result<Self> {
        let ordinal = BACKEND_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let session_id = hex::encode(Sha256::digest(format!(
            "ostadix-ephemeral-session-v1\0{}\0{}\0{}",
            std::process::id(),
            ordinal,
            lang
        )));
        Self::new_with_session(lang, shim_path, sandbox, executable_leases, &session_id)
    }

    fn new_with_session(
        lang: &str,
        shim_path: &Path,
        sandbox: &BackendSandboxPolicy,
        executable_leases: Option<&crate::runtime_exec::ExecutableLeaseSet>,
        session_id: &str,
    ) -> Result<Self> {
        if session_id.len() != 64 || !session_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("backend session identity must be a 64-character SHA-256 digest");
        }
        #[cfg(test)]
        let mut command = legacy_backend_command(shim_path, sandbox)?;
        #[cfg(test)]
        if let Some(executable_leases) = executable_leases {
            executable_leases.verify_backend(lang)?;
            command.env(
                crate::runtime_exec::ADMITTED_EXECUTABLE_MANIFEST_ENV,
                executable_leases.backend_manifest_json(lang)?,
            );
        }

        #[cfg(not(test))]
        let mut command = rust_backend_command(
            lang,
            shim_path,
            sandbox,
            executable_leases.ok_or_else(|| {
                anyhow!("backend `{lang}` has no admitted executable lease authority")
            })?,
        )?;

        command.env("O_BACKEND_SESSION_ID", session_id);

        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to spawn backend process for `{lang}`"))?;

        lifecycle_trace(
            "worker.backend_spawned",
            format!("language={lang} backend_pid={}", child.id()),
        );

        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let cleanup =
                    kill_and_reap_process_group(&mut child, BACKEND_FALLBACK_REAP_TIMEOUT);
                return match cleanup {
                    Ok(()) => Err(anyhow!("backend process did not provide stdin")),
                    Err(cleanup) => Err(anyhow!(
                        "backend process did not provide stdin; backend cleanup also failed: {cleanup:#}"
                    )),
                };
            }
        };

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                drop(stdin);
                let cleanup =
                    kill_and_reap_process_group(&mut child, BACKEND_FALLBACK_REAP_TIMEOUT);
                return match cleanup {
                    Ok(()) => Err(anyhow!("backend process did not provide stdout")),
                    Err(cleanup) => Err(anyhow!(
                        "backend process did not provide stdout; backend cleanup also failed: {cleanup:#}"
                    )),
                };
            }
        };

        // The protocol permits one response per command. Capacity one keeps a
        // faulty backend from converting the reader thread into an unbounded
        // memory queue while retaining timeout-capable receives.
        let (responses_tx, responses) = mpsc::sync_channel(1);
        let reader_name = format!("ostadix-backend-reader-{}", child.id());
        let reader = match thread::Builder::new().name(reader_name).spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                match wire::read_frame::<_, BackendWireResponseV2>(&mut stdout) {
                    Ok(Some(response)) => {
                        if responses_tx.send(Ok(response)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(error) => {
                        let _ = responses_tx.send(Err(format!(
                            "failed to read backend wire response: {error:#}"
                        )));
                        return;
                    }
                }
            }
        }) {
            Ok(reader) => reader,
            Err(error) => {
                let cleanup =
                    kill_and_reap_process_group(&mut child, BACKEND_FALLBACK_REAP_TIMEOUT);
                return match cleanup {
                    Ok(()) => Err(error).context("failed to create backend response reader"),
                    Err(cleanup) => Err(anyhow!(
                        "failed to create backend response reader: {error}; backend cleanup also failed: {cleanup:#}"
                    )),
                };
            }
        };

        Ok(Self {
            language: lang.to_string(),
            child,
            stdin: Some(BufWriter::new(stdin)),
            responses,
            reader: Some(reader),
            terminal: false,
            exec_pending: false,
        })
    }

    fn send_command(&mut self, command: &BackendWireCommandV2) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("backend command channel is closed"))?;
        wire::write_frame(stdin, command).context("failed to write backend wire command")
    }

    fn recv_step(&mut self) -> Result<ExecStep> {
        let step = Self::response_step(self.recv_response()?);
        if !matches!(&step, Ok(ExecStep::EvalRequest { .. })) {
            self.exec_pending = false;
        }
        step
    }

    fn recv_response(&mut self) -> Result<BackendWireResponseV2> {
        self.responses
            .recv()
            .map_err(|_| anyhow!("backend process closed stdout unexpectedly"))?
            .map_err(anyhow::Error::msg)
    }

    fn recv_step_timeout(&mut self, timeout: Duration) -> Result<ExecStep> {
        let step = Self::response_step(self.recv_response_timeout(timeout)?);
        if !matches!(&step, Ok(ExecStep::EvalRequest { .. })) {
            self.exec_pending = false;
        }
        step
    }

    fn recv_response_timeout(&mut self, timeout: Duration) -> Result<BackendWireResponseV2> {
        self.responses
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => anyhow!(
                    "backend `{}` did not answer within {} ms",
                    self.language,
                    timeout.as_millis()
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    anyhow!("backend process closed stdout unexpectedly")
                }
            })?
            .map_err(anyhow::Error::msg)
    }

    fn response_step(response: BackendWireResponseV2) -> Result<ExecStep> {
        match response {
            BackendWireResponseV2::Ok { value } => Ok(ExecStep::Done(value)),
            BackendWireResponseV2::Err { message } => {
                Err(anyhow::Error::new(BackendSemanticError(message)))
            }
            BackendWireResponseV2::EvalRequest { src, scope } => {
                Ok(ExecStep::EvalRequest { src, scope })
            }
            BackendWireResponseV2::StateCapabilitiesV1 { .. }
            | BackendWireResponseV2::CheckpointV1 { .. }
            | BackendWireResponseV2::RestoreV1 { .. }
            | BackendWireResponseV2::StatePinRequiredV1 { .. }
            | BackendWireResponseV2::StateErrorV1 { .. } => {
                bail!("backend returned a state-protocol response during execution")
            }
        }
    }

    fn send_eval_result(&mut self, value: OValue) -> Result<()> {
        if !self.exec_pending {
            bail!("backend has no pending execution awaiting eval_result");
        }
        self.send_command(&BackendWireCommandV2::EvalResult { value })
    }

    fn begin_exec(&mut self, code: &str, bindings: HashMap<String, OValue>) -> Result<()> {
        if self.exec_pending {
            bail!("backend already has a pending execution");
        }
        self.send_command(&BackendWireCommandV2::Exec {
            code: code.to_string(),
            bindings,
        })?;
        self.exec_pending = true;
        Ok(())
    }

    fn exec(&mut self, code: &str, bindings: HashMap<String, OValue>) -> Result<OValue> {
        self.begin_exec(code, bindings)?;
        match self.recv_step()? {
            ExecStep::Done(v) => Ok(v),
            ExecStep::EvalRequest { src, .. } => Err(anyhow!(
                "unexpected eval_request from shim (src: {:?}): \
                 O.eval is only supported when the evaluator uses the \
                 exec_with_eval_callback path",
                &src[..src.len().min(60)]
            )),
        }
    }

    fn state_capabilities(&mut self) -> Result<BackendStateCapabilitiesV1> {
        self.ensure_state_boundary()?;
        self.send_command(&BackendWireCommandV2::StateCapabilitiesV1)?;
        match self.recv_response_timeout(backend_operation_timeout())? {
            BackendWireResponseV2::StateCapabilitiesV1 { capabilities } => {
                capabilities.validate()?;
                Ok(capabilities)
            }
            response => Err(unexpected_state_response("state_capabilities_v1", response)),
        }
    }

    fn checkpoint(&mut self, max_bytes: u64) -> Result<BackendCheckpointV1> {
        self.ensure_state_boundary()?;
        self.send_command(&BackendWireCommandV2::CheckpointV1 { max_bytes })?;
        match self.recv_response_timeout(backend_operation_timeout())? {
            BackendWireResponseV2::CheckpointV1 { checkpoint } => {
                checkpoint.validate()?;
                crate::backend_state::ensure_checkpoint_bound(&checkpoint, max_bytes)?;
                Ok(checkpoint)
            }
            BackendWireResponseV2::StatePinRequiredV1 { reason } => {
                Err(anyhow::Error::new(BackendStatePinned {
                    backend: reason.backend,
                    path: reason.path,
                    message: reason.message,
                }))
            }
            BackendWireResponseV2::StateErrorV1 { error } => bail!(
                "{}: backend={} message={}",
                error.code,
                error.backend,
                error.message
            ),
            response => Err(unexpected_state_response("checkpoint_v1", response)),
        }
    }

    fn restore(&mut self, checkpoint: BackendCheckpointV1) -> Result<BackendRestoreReceiptV1> {
        self.ensure_state_boundary()?;
        checkpoint.validate()?;
        self.send_command(&BackendWireCommandV2::RestoreV1 {
            checkpoint: checkpoint.clone(),
        })?;
        match self.recv_response_timeout(backend_operation_timeout())? {
            BackendWireResponseV2::RestoreV1 { receipt } => {
                if !receipt.restored
                    || receipt.backend != checkpoint.backend
                    || receipt.checkpoint_sha256 != checkpoint.checkpoint_sha256()?
                {
                    bail!("backend returned an invalid restore receipt");
                }
                Ok(receipt)
            }
            BackendWireResponseV2::StatePinRequiredV1 { reason } => {
                Err(anyhow::Error::new(BackendStatePinned {
                    backend: reason.backend,
                    path: reason.path,
                    message: reason.message,
                }))
            }
            BackendWireResponseV2::StateErrorV1 { error } => bail!(
                "{}: backend={} message={}",
                error.code,
                error.backend,
                error.message
            ),
            response => Err(unexpected_state_response("restore_v1", response)),
        }
    }

    fn ensure_state_boundary(&self) -> Result<()> {
        if self.exec_pending {
            bail!("state.not-settled: backend execution is still pending");
        }
        Ok(())
    }

    fn shutdown(&mut self, timeout: Duration) -> Result<()> {
        let deadline = bounded_deadline(timeout, "backend shutdown")?;
        if let Err(error) = self.send_command(&BackendWireCommandV2::Shutdown) {
            let termination = self.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
            return match termination {
                Ok(()) => Err(anyhow::Error::new(BackendShutdownUnacknowledged(
                    format!("failed to send backend shutdown: {error:#}"),
                ))),
                Err(termination) => Err(anyhow!(
                    "failed to send backend shutdown: {error:#}; forced termination also failed: {termination:#}"
                )),
            };
        }
        lifecycle_trace(
            "worker.shutdown_sent",
            format!("language={} backend_pid={}", self.language, self.child.id()),
        );
        // The complete framed command has been flushed. Closing the command
        // pipe now guarantees EOF even for an older proxy or shim.
        self.stdin.take();

        match self.recv_shutdown_step_before(deadline, timeout) {
            Ok(ExecStep::Done(OValue::Null)) => lifecycle_trace(
                "worker.shutdown_acknowledged",
                format!("language={} backend_pid={}", self.language, self.child.id()),
            ),
            Ok(ExecStep::Done(other)) => {
                let termination = self.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
                return Err(anyhow!(
                    "backend `{}` acknowledged shutdown with {}, expected null{}",
                    self.language,
                    other.type_name(),
                    termination
                        .err()
                        .map(|error| format!("; forced termination failed: {error:#}"))
                        .unwrap_or_default()
                ));
            }
            Ok(ExecStep::EvalRequest { .. }) => {
                let termination = self.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
                return Err(anyhow!(
                    "backend `{}` requested O.eval while acknowledging shutdown{}",
                    self.language,
                    termination
                        .err()
                        .map(|error| format!("; forced termination failed: {error:#}"))
                        .unwrap_or_default()
                ));
            }
            Err(error) => {
                let termination = self.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
                return match termination {
                    Ok(()) => Err(anyhow::Error::new(BackendShutdownUnacknowledged(
                        format!("backend shutdown was not acknowledged: {error:#}"),
                    ))),
                    Err(termination) => Err(anyhow!(
                        "backend shutdown was not acknowledged: {error:#}; forced termination also failed: {termination:#}"
                    )),
                };
            }
        }

        // An acknowledgement is the backend's final protocol action. A
        // conforming backend exits naturally after it; avoiding an eager
        // signal also avoids conflating Darwin's zombie-only process-group
        // race with successful shutdown. The same absolute deadline governs
        // acknowledgement, leader exit, reader completion, and proof that no
        // active descendant remains in the owned process group.
        if let Err(natural_exit) = self.finish_graceful_shutdown(deadline, timeout) {
            let termination = self.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
            return match termination {
                Ok(()) => Err(natural_exit.context(
                    "backend acknowledged shutdown but did not terminate cleanly",
                )),
                Err(termination) => Err(anyhow!(
                    "backend acknowledged shutdown but did not terminate cleanly: {natural_exit:#}; forced termination also failed: {termination:#}"
                )),
            };
        }
        Ok(())
    }

    /// Retire a one-shot backend after a fresh environment completes.
    ///
    /// `shutdown` deliberately reports a missing protocol acknowledgement,
    /// even when its bounded fallback has already killed and reaped the owned
    /// process group. That strict diagnostic is useful for explicit lifecycle
    /// checks, but older compatibility shims do not implement the shutdown
    /// verb. Fresh-environment isolation requires physical retirement, not a
    /// particular acknowledgement. Accept the fallback only when `shutdown`
    /// proved the process, response reader, and owned group fully terminal.
    fn retire_fresh_attempt(&mut self, timeout: Duration) -> Result<()> {
        match self.shutdown(timeout) {
            Ok(()) => Ok(()),
            Err(graceful_error)
                if self.terminal && graceful_error.is::<BackendShutdownUnacknowledged>() =>
            {
                lifecycle_trace(
                    "worker.shutdown_forced_compatible",
                    format!(
                        "language={} backend_pid={} reason={graceful_error:#}",
                        self.language,
                        self.child.id()
                    ),
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn recv_shutdown_step_before(
        &mut self,
        deadline: Instant,
        timeout: Duration,
    ) -> Result<ExecStep> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                anyhow!(
                    "backend `{}` did not answer within {} ms",
                    self.language,
                    timeout.as_millis()
                )
            })?;
        let response = self
            .responses
            .recv_timeout(remaining)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => anyhow!(
                    "backend `{}` did not answer within {} ms",
                    self.language,
                    timeout.as_millis()
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    anyhow!("backend process closed stdout unexpectedly")
                }
            })?
            .map_err(anyhow::Error::msg)?;
        Self::response_step(response)
    }

    fn finish_graceful_shutdown(&mut self, deadline: Instant, timeout: Duration) -> Result<()> {
        self.wait_for_leader_exit_before(deadline, timeout)?;
        self.finish_reader_before(deadline, timeout)?;
        self.wait_for_owned_group_quiescence_before(deadline, timeout)?;
        self.reap_leader_and_mark_terminal()
    }

    fn force_terminate(&mut self, timeout: Duration) -> Result<()> {
        if self.terminal {
            return self.finish_reader_bounded(timeout);
        }
        let deadline = bounded_deadline(timeout, "forced backend termination")?;
        self.stdin.take();
        let mut failures = Vec::new();
        if let Err(error) = signal_owned_process_group(&mut self.child) {
            failures.push(format!("backend SIGKILL failed: {error:#}"));
        }

        let leader_exited = match self.wait_for_leader_exit_before(deadline, timeout) {
            Ok(()) => true,
            Err(error) => {
                failures.push(format!("backend leader wait failed: {error:#}"));
                false
            }
        };
        let reader_finished = match self.finish_reader_before(deadline, timeout) {
            Ok(()) => true,
            Err(error) => {
                failures.push(format!("backend response-reader wait failed: {error:#}"));
                false
            }
        };
        let group_quiescent = match self.wait_for_owned_group_quiescence_before(deadline, timeout) {
            Ok(()) => true,
            Err(error) => {
                failures.push(format!("backend process-group wait failed: {error:#}"));
                false
            }
        };
        if leader_exited && reader_finished && group_quiescent {
            if let Err(error) = self.reap_leader_and_mark_terminal() {
                failures.push(format!("backend leader reap failed: {error:#}"));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(failures.join("; ")))
        }
    }

    fn wait_for_leader_exit_before(&mut self, deadline: Instant, timeout: Duration) -> Result<()> {
        loop {
            if self.leader_exited_without_reaping()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "backend `{}` process {} did not terminate within {} ms",
                    self.language,
                    self.child.id(),
                    timeout.as_millis()
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(Duration::from_millis(2).min(remaining));
        }
    }

    #[cfg(unix)]
    fn leader_exited_without_reaping(&mut self) -> Result<bool> {
        loop {
            // SAFETY: `waitid` initializes siginfo for this exact direct child.
            // WNOWAIT observes exit without releasing the PID that anchors the
            // owned process-group identity while descendants are inspected.
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
                // SAFETY: successful waitid initialized `information`; si_pid
                // is zero when WNOHANG found no waitable state.
                return Ok(unsafe { information.si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).context("failed to inspect backend leader state");
            }
        }
    }

    #[cfg(not(unix))]
    fn leader_exited_without_reaping(&mut self) -> Result<bool> {
        self.child
            .try_wait()
            .context("failed to inspect backend leader state")
            .map(|status| status.is_some())
    }

    fn wait_for_owned_group_quiescence_before(
        &self,
        deadline: Instant,
        timeout: Duration,
    ) -> Result<()> {
        let group = i32::try_from(self.child.id())
            .map_err(|_| anyhow!("backend pid does not fit process-group identifier"))?;
        loop {
            if owned_group_has_no_active_descendants(group).with_context(|| {
                format!(
                    "failed to inspect backend `{}` process group {group}",
                    self.language
                )
            })? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "backend `{}` process group {} still contains an active descendant after {} ms",
                    self.language,
                    group,
                    timeout.as_millis()
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(Duration::from_millis(2).min(remaining));
        }
    }

    fn reap_leader_and_mark_terminal(&mut self) -> Result<()> {
        self.child
            .wait()
            .context("failed to reap backend leader after termination")?;
        self.terminal = true;
        lifecycle_trace(
            "worker.backend_wait_returned",
            format!("language={} backend_pid={}", self.language, self.child.id()),
        );
        Ok(())
    }

    fn finish_reader_bounded(&mut self, timeout: Duration) -> Result<()> {
        let deadline = bounded_deadline(timeout, "backend response-reader")?;
        self.finish_reader_before(deadline, timeout)
    }

    fn finish_reader_before(&mut self, deadline: Instant, timeout: Duration) -> Result<()> {
        while self
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.is_finished())
        {
            while self.responses.try_recv().is_ok() {}
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "backend `{}` response reader did not terminate within {} ms",
                    self.language,
                    timeout.as_millis()
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(Duration::from_millis(2).min(remaining));
        }
        if let Some(reader) = self.reader.take() {
            reader
                .join()
                .map_err(|_| anyhow!("backend response reader panicked"))?;
        }
        Ok(())
    }
}

fn unexpected_state_response(operation: &str, response: BackendWireResponseV2) -> anyhow::Error {
    let kind = match response {
        BackendWireResponseV2::Ok { .. } => "ok",
        BackendWireResponseV2::Err { .. } => "err",
        BackendWireResponseV2::EvalRequest { .. } => "eval_request",
        BackendWireResponseV2::StateCapabilitiesV1 { .. } => "state_capabilities_v1",
        BackendWireResponseV2::CheckpointV1 { .. } => "checkpoint_v1",
        BackendWireResponseV2::RestoreV1 { .. } => "restore_v1",
        BackendWireResponseV2::StatePinRequiredV1 { .. } => "state_pin_required_v1",
        BackendWireResponseV2::StateErrorV1 { .. } => "state_error_v1",
    };
    anyhow!("backend returned `{kind}` while answering `{operation}`")
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        if self.terminal {
            let _ = self.finish_reader_bounded(BACKEND_FALLBACK_REAP_TIMEOUT);
            return;
        }
        lifecycle_trace(
            "worker.backend_drop_fallback",
            format!("language={} backend_pid={}", self.language, self.child.id()),
        );
        // Never run a request/response protocol from a destructor. Closing the
        // pipe, killing the owned process group, and briefly polling for reap
        // are bounded best-effort fallback actions only.
        let _ = self.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
    }
}

/// Execute one ephemeral backend operation and close its physical process
/// before returning the semantic result to the worker pool.
pub(crate) fn run_ephemeral_with_eval_callback<F>(
    language: &str,
    code: &str,
    bindings: HashMap<String, OValue>,
    shim_path: &Path,
    sandbox: &BackendSandboxPolicy,
    executable_leases: Option<&Arc<crate::runtime_exec::ExecutableLeaseSet>>,
    mut evaluate: F,
) -> Result<OValue>
where
    F: FnMut(String, Option<OValue>, Duration) -> Result<OValue>,
{
    let mut process = BackendProcess::new(
        language,
        shim_path,
        sandbox,
        executable_leases.map(AsRef::as_ref),
    )
    .with_context(|| format!("failed to start ephemeral backend `{language}`"))
    .map_err(infrastructure_error)?;
    let operation_timeout = backend_operation_timeout();
    let operation_deadline = Instant::now()
        .checked_add(operation_timeout)
        .ok_or_else(|| infrastructure_error(anyhow!("backend operation deadline overflowed")))?;

    let execution = (|| {
        process
            .begin_exec(code, bindings)
            .map_err(infrastructure_error)?;
        lifecycle_trace(
            "worker.exec_sent",
            format!("language={language} environment=ephemeral"),
        );
        loop {
            let remaining = operation_deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    infrastructure_error(anyhow!(
                        "backend `{language}` operation exceeded {} ms",
                        operation_timeout.as_millis()
                    ))
                })?;
            let step = process.recv_step_timeout(remaining).map_err(|error| {
                if error.is::<BackendSemanticError>() {
                    error
                } else {
                    infrastructure_error(error)
                }
            })?;
            match step {
                ExecStep::Done(value) => {
                    lifecycle_trace(
                        "worker.done_received",
                        format!("language={language} environment=ephemeral"),
                    );
                    return Ok(value);
                }
                ExecStep::EvalRequest { src, scope } => {
                    let value = evaluate(src, scope, remaining)?;
                    process
                        .send_eval_result(value)
                        .map_err(infrastructure_error)?;
                }
            }
        }
    })();

    match execution {
        Ok(value) => {
            process
                .retire_fresh_attempt(backend_shutdown_timeout())
                .with_context(|| {
                    format!(
                        "ephemeral backend `{language}` returned a value but could not be retired"
                    )
                })
                .map_err(infrastructure_error)?;
            Ok(value)
        }
        Err(error) => match process.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT) {
            Ok(()) => Err(error),
            Err(termination) => Err(infrastructure_error(anyhow!(
                "{error:#}; ephemeral backend termination also failed: {termination:#}"
            ))),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackendSessionLimits {
    pub(crate) total: usize,
    pub(crate) per_backend: usize,
}

impl BackendSessionLimits {
    fn from_env() -> Self {
        let total = positive_usize_from_env(
            "O_BACKEND_MAX_OPEN_SESSIONS",
            DEFAULT_MAX_OPEN_BACKEND_SESSIONS,
        );
        let per_backend = positive_usize_from_env(
            "O_BACKEND_MAX_OPEN_SESSIONS_PER_BACKEND",
            DEFAULT_MAX_OPEN_BACKEND_SESSIONS_PER_BACKEND,
        )
        .min(total);
        Self { total, per_backend }
    }
}

fn positive_usize_from_env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub struct ProcessRegistry {
    registry: HashMap<(String, u32, BackendSandboxPolicy, String), BackendProcess>,
    session_limits: BackendSessionLimits,
    registry_identity_sha256: String,
}

/// Admission-scoped physical launch authority shared by persistent and
/// ephemeral registry entry points. Grouping these immutable launch facts
/// keeps the semantic execution arguments separate from process authority.
pub(crate) struct BackendLaunchContext<'a> {
    pub(crate) shim_path: &'a Path,
    pub(crate) sandbox: &'a BackendSandboxPolicy,
    pub(crate) executable_leases: Option<&'a Arc<crate::runtime_exec::ExecutableLeaseSet>>,
    /// Canonical admission projection over this backend's exact executable
    /// set, consumed shim artifact, and child launch context. Persistent
    /// actors may be reused only within one such generation.
    pub(crate) launch_generation_sha256: Option<&'a str>,
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessRegistry {
    pub fn new() -> Self {
        Self {
            registry: HashMap::new(),
            session_limits: BackendSessionLimits::from_env(),
            registry_identity_sha256: fresh_registry_identity(),
        }
    }

    #[cfg(test)]
    fn with_session_limits(total: usize, per_backend: usize) -> Self {
        assert!(total > 0 && per_backend > 0 && per_backend <= total);
        Self {
            registry: HashMap::new(),
            session_limits: BackendSessionLimits { total, per_backend },
            registry_identity_sha256: fresh_registry_identity(),
        }
    }

    /// Ensure the process for `(lang, env_id)` is running and send the Exec
    /// command. The caller must then drive the reply cycle with
    /// `recv_exec_step` / `send_eval_result` until a `Done` step arrives.
    pub(crate) fn send_exec(
        &mut self,
        lang: &str,
        env_id: u32,
        code: &str,
        bindings: HashMap<String, OValue>,
        launch: BackendLaunchContext<'_>,
    ) -> Result<()> {
        let launch_generation_sha256 = actor_launch_generation(&launch, lang)?;
        self.reject_generation_conflict(lang, env_id, launch.sandbox, &launch_generation_sha256)?;
        let key = (
            lang.to_string(),
            env_id,
            launch.sandbox.clone(),
            launch_generation_sha256,
        );
        if !self.registry.contains_key(&key) {
            self.ensure_session_capacity(lang)?;
            let session_id = actor_session_identity(
                &self.registry_identity_sha256,
                lang,
                env_id,
                launch.sandbox,
            );
            let process = BackendProcess::new_with_session(
                lang,
                launch.shim_path,
                launch.sandbox,
                launch.executable_leases.map(AsRef::as_ref),
                &session_id,
            )
            .with_context(|| format!("failed to start backend for language `{lang}`"))?;
            self.registry.insert(key.clone(), process);
        }
        self.registry
            .get_mut(&key)
            .expect("backend was just inserted but is missing")
            .begin_exec(code, bindings)
            .with_context(|| format!("failed to send Exec to backend `{lang}`"))?;
        lifecycle_trace(
            "worker.exec_sent",
            format!("language={lang} environment={env_id}"),
        );
        Ok(())
    }

    /// Read the next step from the shim for `(lang, env_id)`.
    pub(crate) fn recv_exec_step(
        &mut self,
        lang: &str,
        env_id: u32,
        sandbox: &BackendSandboxPolicy,
    ) -> Result<ExecStep> {
        let key = self.process_key(lang, env_id, sandbox)?;
        let step = self
            .registry
            .get_mut(&key)
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?
            .recv_step();

        if step
            .as_ref()
            .is_err_and(|error| !error.is::<BackendSemanticError>())
        {
            self.registry.remove(&key);
        }
        let step = step.with_context(|| format!("backend `{lang}[{env_id}]` recv_step failed"))?;
        if matches!(step, ExecStep::Done(_)) {
            lifecycle_trace(
                "worker.done_received",
                format!("language={lang} environment={env_id}"),
            );
        }
        Ok(step)
    }

    /// Read one backend step under a caller-owned deadline. This is used by
    /// coordinator-side work nested inside an admitted worker callback so the
    /// outer operation cannot time out while recursive hosted execution waits
    /// forever in the coordinator.
    pub(crate) fn recv_exec_step_timeout(
        &mut self,
        lang: &str,
        env_id: u32,
        sandbox: &BackendSandboxPolicy,
        timeout: Duration,
    ) -> Result<ExecStep> {
        let key = self.process_key(lang, env_id, sandbox)?;
        let step = self
            .registry
            .get_mut(&key)
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?
            .recv_step_timeout(timeout);
        let step = match step {
            Ok(step) => step,
            Err(error) if error.is::<BackendSemanticError>() => {
                return Err(error).with_context(|| {
                    format!("backend `{lang}[{env_id}]` returned an execution error")
                });
            }
            Err(error) => {
                let mut process = self
                    .registry
                    .remove(&key)
                    .expect("timed receive process was present before removal");
                let termination = process.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
                let error = error.context(format!(
                    "backend `{lang}[{env_id}]` did not settle within the inherited callback deadline"
                ));
                return match termination {
                    Err(termination) => Err(infrastructure_error(anyhow!(
                        "{error:#}; backend termination also failed: {termination:#}"
                    ))),
                    Ok(()) if error.is::<BackendSemanticError>() => Err(error),
                    Ok(()) => Err(infrastructure_error(error)),
                };
            }
        };
        if matches!(step, ExecStep::Done(_)) {
            lifecycle_trace(
                "worker.done_received",
                format!("language={lang} environment={env_id}"),
            );
        }
        Ok(step)
    }

    /// Send an eval_result back to the shim so it can resume execution.
    pub(crate) fn send_eval_result(
        &mut self,
        lang: &str,
        env_id: u32,
        value: OValue,
        sandbox: &BackendSandboxPolicy,
    ) -> Result<()> {
        let key = self.process_key(lang, env_id, sandbox)?;
        self.registry
            .get_mut(&key)
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?
            .send_eval_result(value)
            .with_context(|| format!("failed to send eval_result to backend `{lang}`"))
    }

    /// Whether an exact logical actor/sandbox still has a published physical
    /// process, independent of its launch generation. Used only to classify a
    /// failed split-protocol settlement as a semantic refusal (actor retained)
    /// or ambiguous infrastructure loss (actor no longer usable).
    pub(crate) fn has_live_env(
        &self,
        lang: &str,
        env_id: u32,
        sandbox: &BackendSandboxPolicy,
    ) -> bool {
        self.registry
            .keys()
            .any(|(candidate_lang, candidate_env, candidate_sandbox, _)| {
                candidate_lang == lang && *candidate_env == env_id && candidate_sandbox == sandbox
            })
    }

    pub(crate) fn exec(
        &mut self,
        lang: &str,
        env_id: u32,
        code: &str,
        bindings: HashMap<String, OValue>,
        launch: BackendLaunchContext<'_>,
    ) -> Result<OValue> {
        let launch_generation_sha256 = actor_launch_generation(&launch, lang)?;
        self.reject_generation_conflict(lang, env_id, launch.sandbox, &launch_generation_sha256)?;
        let key = (
            lang.to_string(),
            env_id,
            launch.sandbox.clone(),
            launch_generation_sha256,
        );

        if !self.registry.contains_key(&key) {
            self.ensure_session_capacity(lang)?;
            let session_id = actor_session_identity(
                &self.registry_identity_sha256,
                lang,
                env_id,
                launch.sandbox,
            );
            let process = BackendProcess::new_with_session(
                lang,
                launch.shim_path,
                launch.sandbox,
                launch.executable_leases.map(AsRef::as_ref),
                &session_id,
            )
            .with_context(|| format!("failed to start backend for language `{lang}`"))?;
            self.registry.insert(key.clone(), process);
        }

        let result = self
            .registry
            .get_mut(&key)
            .expect("backend was just inserted but is missing")
            .exec(code, bindings);

        if result
            .as_ref()
            .is_err_and(|error| !error.is::<BackendSemanticError>())
        {
            self.registry.remove(&key);
        }

        result.with_context(|| {
            let env_label = if env_id == u32::MAX {
                "*ephemeral*".to_string()
            } else {
                env_id.to_string()
            };

            format!(
                "backend `{}` env [{}] failed while executing code",
                lang, env_label
            )
        })
    }

    pub(crate) fn state_capabilities(
        &mut self,
        lang: &str,
        env_id: u32,
        sandbox: &BackendSandboxPolicy,
    ) -> Result<BackendStateCapabilitiesV1> {
        let key = self.process_key(lang, env_id, sandbox)?;
        self.registry
            .get_mut(&key)
            .expect("resolved backend process is missing")
            .state_capabilities()
            .with_context(|| {
                format!("failed to query backend state capabilities for `{lang}[{env_id}]`")
            })
    }

    pub(crate) fn checkpoint_env(
        &mut self,
        lang: &str,
        env_id: u32,
        sandbox: &BackendSandboxPolicy,
        max_bytes: u64,
    ) -> Result<BackendCheckpointV1> {
        let key = self.process_key(lang, env_id, sandbox)?;
        self.registry
            .get_mut(&key)
            .expect("resolved backend process is missing")
            .checkpoint(max_bytes)
            .with_context(|| format!("failed to checkpoint backend `{lang}[{env_id}]`"))
    }

    /// Capture every settled persistent actor without removing, replacing, or
    /// shutting down any registry entry. A refusal or protocol error aborts
    /// the whole aggregate, while actors already queried remain live.
    pub(crate) fn checkpoint_persistent_actors(
        &mut self,
        max_total_bytes: u64,
    ) -> Result<EvaluatorStateSnapshotV1> {
        if max_total_bytes == 0 {
            bail!("evaluator snapshot byte limit must be non-zero");
        }
        let mut ordered = self
            .registry
            .keys()
            .filter(|(_, env_id, _, _)| *env_id <= crate::environment::MAX_PERSISTENT_ENV_ID)
            .map(|key| {
                sandbox_policy_sha256(key.2.permissions()).map(|digest| (key.clone(), digest))
            })
            .collect::<Result<Vec<_>>>()?;
        ordered.sort_by(|(left, left_sandbox), (right, right_sandbox)| {
            (&left.0, left.1, left_sandbox, &left.3).cmp(&(
                &right.0,
                right.1,
                right_sandbox,
                &right.3,
            ))
        });

        let mut actors = Vec::with_capacity(ordered.len());
        let empty = EvaluatorStateSnapshotV1::new(Vec::new())?;
        ensure_evaluator_snapshot_bound(&empty, max_total_bytes)?;
        for ((lang, env_id, sandbox, launch_generation_sha256), _) in ordered {
            let partial = EvaluatorStateSnapshotV1::new(actors.clone())?;
            let used = u64::try_from(partial.encoded_len()?)
                .context("partial evaluator snapshot length exceeds u64")?;
            let remaining = max_total_bytes.checked_sub(used).ok_or_else(|| {
                anyhow!("evaluator snapshot metadata exhausted its aggregate byte limit")
            })?;
            if remaining == 0 {
                bail!("evaluator snapshot metadata exhausted its aggregate byte limit");
            }

            let capabilities = self.state_capabilities(&lang, env_id, &sandbox)?;
            if capabilities.backend != lang {
                bail!(
                    "backend `{lang}[{env_id}]` reported state identity `{}`",
                    capabilities.backend
                );
            }
            if capabilities.tier == BackendStateTierV1::ExternalPinned
                || !capabilities.restore_supported
            {
                return Err(anyhow::Error::new(BackendStatePinned {
                    backend: lang,
                    path: "$actor".to_string(),
                    message:
                        "backend state is pinned to external resources and has no portable restore"
                            .to_string(),
                }));
            }
            let checkpoint = self.checkpoint_env(&lang, env_id, &sandbox, remaining)?;
            if checkpoint.tier != capabilities.tier || checkpoint.codec != capabilities.codec {
                bail!(
                    "backend `{lang}[{env_id}]` checkpoint disagrees with its advertised state capabilities"
                );
            }
            actors.push(EvaluatorActorCheckpointV1::new(
                lang,
                env_id,
                sandbox.permissions().to_vec(),
                launch_generation_sha256,
                checkpoint,
            )?);
            let partial = EvaluatorStateSnapshotV1::new(actors.clone())?;
            ensure_evaluator_snapshot_bound(&partial, max_total_bytes)?;
        }

        let snapshot = EvaluatorStateSnapshotV1::new(actors)?;
        ensure_evaluator_snapshot_bound(&snapshot, max_total_bytes)?;
        Ok(snapshot)
    }

    /// Validate a staged restore set against the live registry without
    /// mutating either side. This makes multi-actor staging all-or-nothing.
    pub(crate) fn ensure_restore_targets_vacant(
        &self,
        actors: &[EvaluatorActorCheckpointV1],
    ) -> Result<()> {
        for actor in actors {
            let sandbox = BackendSandboxPolicy::new(actor.sandbox_permissions.iter().copied());
            if self
                .registry
                .keys()
                .any(|(candidate_lang, candidate_env, candidate_sandbox, _)| {
                    candidate_lang == &actor.canonical_backend
                        && *candidate_env == actor.environment_id
                        && candidate_sandbox == &sandbox
                })
            {
                bail!(
                    "state.restore-conflict: backend `{}[{}]` already owns an open session for sandbox {}",
                    actor.canonical_backend,
                    actor.environment_id,
                    actor.sandbox_policy_sha256
                );
            }
        }
        Ok(())
    }

    /// Restore a checkpoint into a new physical actor and publish it only
    /// after the backend returns a matching receipt. An already-open logical
    /// session is never evicted or overwritten.
    pub(crate) fn restore_env(
        &mut self,
        lang: &str,
        env_id: u32,
        checkpoint: BackendCheckpointV1,
        launch: BackendLaunchContext<'_>,
    ) -> Result<BackendRestoreReceiptV1> {
        checkpoint.validate()?;
        if checkpoint.backend != lang {
            bail!(
                "state.restore-incompatible: checkpoint backend `{}` cannot restore as `{lang}`",
                checkpoint.backend
            );
        }
        if self
            .registry
            .keys()
            .any(|(candidate_lang, candidate_env, candidate_sandbox, _)| {
                candidate_lang == lang
                    && *candidate_env == env_id
                    && candidate_sandbox == launch.sandbox
            })
        {
            bail!(
                "state.restore-conflict: backend `{lang}[{env_id}]` already owns an open session"
            );
        }
        self.ensure_session_capacity(lang)?;
        let launch_generation_sha256 = actor_launch_generation(&launch, lang)?;
        let key = (
            lang.to_string(),
            env_id,
            launch.sandbox.clone(),
            launch_generation_sha256,
        );
        let session_id =
            actor_session_identity(&self.registry_identity_sha256, lang, env_id, launch.sandbox);
        let mut process = BackendProcess::new_with_session(
            lang,
            launch.shim_path,
            launch.sandbox,
            launch.executable_leases.map(AsRef::as_ref),
            &session_id,
        )
        .with_context(|| format!("failed to start restore target for `{lang}[{env_id}]`"))?;
        match process.restore(checkpoint) {
            Ok(receipt) => {
                self.registry.insert(key, process);
                Ok(receipt)
            }
            Err(error) => {
                let termination = process.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
                match termination {
                    Ok(()) => Err(error),
                    Err(termination) => Err(anyhow!(
                        "{error:#}; failed restore target termination also failed: {termination:#}"
                    )),
                }
            }
        }
    }

    pub fn cleanup_env(&mut self, lang: &str, env_id: u32) -> Result<()> {
        let keys = self
            .registry
            .keys()
            .filter(|(candidate_lang, candidate_env, _, _)| {
                candidate_lang == lang && *candidate_env == env_id
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(mut process) = self.registry.remove(&key) {
                process.retire_fresh_attempt(backend_shutdown_timeout())?;
            }
        }
        Ok(())
    }

    /// Explicitly shut down every persistent backend, attempting all entries
    /// and reporting the combined physical teardown failures to the caller.
    pub fn shutdown_all(&mut self, timeout: Duration) -> Result<()> {
        let processes: Vec<_> = self.registry.drain().map(|(_, process)| process).collect();
        let mut failures = Vec::new();
        for mut process in processes {
            if let Err(error) = process.shutdown(timeout) {
                failures.push(format!("{error:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "one or more backend processes failed explicit shutdown: {}",
                failures.join(" | ")
            ))
        }
    }

    fn process_key(
        &self,
        lang: &str,
        env_id: u32,
        sandbox: &BackendSandboxPolicy,
    ) -> Result<(String, u32, BackendSandboxPolicy, String)> {
        let mut matching =
            self.registry
                .keys()
                .filter(|(candidate_lang, candidate_env, candidate_sandbox, _)| {
                    candidate_lang == lang
                        && *candidate_env == env_id
                        && candidate_sandbox == sandbox
                });
        let key = matching
            .next()
            .cloned()
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?;
        if matching.next().is_some() {
            bail!("multiple launch generations are live for backend `{lang}[{env_id}]`");
        }
        Ok(key)
    }

    fn ensure_session_capacity(&self, lang: &str) -> Result<()> {
        if self.registry.len() >= self.session_limits.total {
            bail!(
                "session.capacity-exhausted: open backend sessions reached configured total quota {}",
                self.session_limits.total
            );
        }
        let backend_count = self
            .registry
            .keys()
            .filter(|(candidate_lang, _, _, _)| candidate_lang == lang)
            .count();
        if backend_count >= self.session_limits.per_backend {
            bail!(
                "session.capacity-exhausted: backend `{lang}` reached configured per-backend quota {}",
                self.session_limits.per_backend
            );
        }
        Ok(())
    }

    fn reject_generation_conflict(
        &self,
        lang: &str,
        env_id: u32,
        sandbox: &BackendSandboxPolicy,
        launch_generation_sha256: &str,
    ) -> Result<()> {
        let conflicting_generation = self
            .registry
            .keys()
            .find(
                |(candidate_lang, candidate_env, candidate_sandbox, candidate_digest)| {
                    candidate_lang == lang
                        && *candidate_env == env_id
                        && candidate_sandbox == sandbox
                        && candidate_digest != launch_generation_sha256
                },
            )
            .map(|(_, _, _, digest)| digest);
        if let Some(conflicting_generation) = conflicting_generation {
            bail!(
                "session.generation-conflict: backend `{lang}[{env_id}]` remains pinned to launch generation `{conflicting_generation}`; explicitly checkpoint/cleanup/restore before using `{launch_generation_sha256}`"
            );
        }
        Ok(())
    }
}

fn actor_launch_generation(launch: &BackendLaunchContext<'_>, lang: &str) -> Result<String> {
    if let Some(executable_leases) = launch.executable_leases {
        executable_leases.verify_backend(lang)?;
        let generation = launch.launch_generation_sha256.with_context(|| {
            format!("backend `{lang}` has no admitted launch-generation identity")
        })?;
        if generation.is_empty() {
            bail!("backend `{lang}` has an empty admitted launch-generation identity");
        }
        return Ok(generation.to_string());
    }
    #[cfg(test)]
    {
        Ok(launch
            .launch_generation_sha256
            .unwrap_or("unit-test-legacy-shim")
            .to_string())
    }
    #[cfg(not(test))]
    {
        bail!("backend `{lang}` has no admitted executable lease authority")
    }
}

fn fresh_registry_identity() -> String {
    let mut random = [0_u8; 32];
    if getrandom::fill(&mut random).is_ok() {
        return hex::encode(random);
    }
    let ordinal = BACKEND_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    hex::encode(Sha256::digest(format!(
        "ostadix-registry-fallback/v1\0{}\0{}\0{}",
        std::process::id(),
        monotonic_nanos(),
        ordinal
    )))
}

fn actor_session_identity(
    registry_identity_sha256: &str,
    lang: &str,
    env_id: u32,
    sandbox: &BackendSandboxPolicy,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ostadix-backend-session/v1\0");
    digest.update(registry_identity_sha256.as_bytes());
    digest.update([0]);
    digest.update(lang.as_bytes());
    digest.update([0]);
    digest.update(env_id.to_be_bytes());
    for authority in sandbox.names() {
        digest.update([0]);
        digest.update(authority.as_bytes());
    }
    hex::encode(digest.finalize())
}

impl Drop for ProcessRegistry {
    fn drop(&mut self) {
        // `BackendProcess::drop` performs only bounded best-effort local
        // termination. Explicit protocol shutdown belongs to callers such as
        // `cleanup_env`, never to this destructor.
        self.registry.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_procfs_vanished_entry_errors_are_narrowly_classified() {
        let not_found = io::Error::new(io::ErrorKind::NotFound, "vanished proc entry");
        let esrch = io::Error::from_raw_os_error(libc::ESRCH);
        let permission_denied = io::Error::from_raw_os_error(libc::EACCES);
        let malformed = io::Error::new(io::ErrorKind::InvalidData, "malformed stat");

        assert!(linux_process_observation_disappeared(&not_found));
        assert!(linux_process_observation_disappeared(&esrch));
        assert!(!linux_process_observation_disappeared(&permission_denied));
        assert!(!linux_process_observation_disappeared(&malformed));
    }

    fn python_shim_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("backends/python_shim.py")
    }

    fn test_launch_context<'a>(
        shim_path: &'a Path,
        sandbox: &'a BackendSandboxPolicy,
        launch_generation_sha256: &'a str,
    ) -> BackendLaunchContext<'a> {
        BackendLaunchContext {
            shim_path,
            sandbox,
            executable_leases: None,
            launch_generation_sha256: Some(launch_generation_sha256),
        }
    }

    fn spawn_python_shim() -> Result<BackendProcess> {
        BackendProcess::new(
            "python",
            &python_shim_path(),
            &BackendSandboxPolicy::none(),
            None,
        )
    }

    fn spawn_python_shim_with(
        permissions: impl IntoIterator<Item = crate::value::BackendAuthority>,
    ) -> Result<BackendProcess> {
        BackendProcess::new(
            "python",
            &python_shim_path(),
            &BackendSandboxPolicy::new(permissions),
            None,
        )
    }

    fn expect_done(step: ExecStep) -> OValue {
        match step {
            ExecStep::Done(value) => value,
            ExecStep::EvalRequest { src, .. } => {
                panic!("expected Done step from shim, got EvalRequest({src:?})")
            }
        }
    }

    #[test]
    fn ping_round_trip_returns_null() -> Result<()> {
        let mut process = spawn_python_shim()?;

        process.send_command(&BackendWireCommandV2::Ping)?;
        let value = expect_done(process.recv_step()?);

        assert_eq!(value, OValue::Null);
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_without_bindings_returns_int_result() -> Result<()> {
        let mut process = spawn_python_shim()?;

        let value = process.exec("__oval_result__ = 42", HashMap::new())?;

        assert_eq!(value, OValue::int(42));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_python_big_int_result_uses_number_ovalue() -> Result<()> {
        use crate::value::ONumber;

        let mut process = spawn_python_shim()?;

        let value = process.exec("__oval_result__ = 2 ** 100", HashMap::new())?;

        match value {
            OValue::Number {
                v: ONumber::Int { v },
            } => {
                let expected = num_bigint::BigInt::from(1_u8) << 100_u32;
                assert_eq!(v, expected);
            }
            other => panic!("expected number/int OValue, got {other:?}"),
        }
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_python_huge_numbers_bypass_decimal_digit_limits() -> Result<()> {
        use crate::backend_catalog::SpliceRenderer;
        use crate::eval_core::render_with;
        use crate::value::ONumber;

        let mut process = spawn_python_shim()?;
        let digits = "9".repeat(5_000);
        let decimal_coefficient = num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10).unwrap();
        let hex_digits = "f".repeat(5_000);
        let integer_value =
            OValue::big_int(num_bigint::BigInt::parse_bytes(hex_digits.as_bytes(), 16).unwrap());
        let integer_source = render_with(SpliceRenderer::Python, &integer_value);

        let integer = process.exec(
            &format!("int = None\n__oval_result__ = {integer_source}"),
            HashMap::new(),
        )?;
        assert_eq!(integer, integer_value.clone());

        let decimal_value = OValue::number(ONumber::Decimal {
            coeff: decimal_coefficient,
            exp10: 0,
            special: None,
        });
        let decimal_source = render_with(SpliceRenderer::Python, &decimal_value);
        let decimal = process.exec(
            &format!("__oval_result__ = {decimal_source}"),
            HashMap::new(),
        )?;
        assert_eq!(decimal, decimal_value);

        let bindings = HashMap::from([("huge".to_string(), integer_value)]);
        assert_eq!(
            process.exec(
                &format!("__oval_result__ = huge == {integer_source}"),
                bindings,
            )?,
            OValue::bool_(true),
        );

        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn python_scope_with_invalid_nested_numbers_round_trips_without_crashing() -> Result<()> {
        use crate::backend_catalog::SpliceRenderer;
        use crate::eval_core::render_with;
        use crate::value::{FloatFormat, ONumber};

        let scope = OValue::scope(HashMap::from([
            (
                "zero_denominator".to_string(),
                OValue::number(ONumber::Rational {
                    num: 1.into(),
                    den: 0.into(),
                }),
            ),
            (
                "malformed_float".to_string(),
                OValue::number(ONumber::BinaryFloat {
                    format: FloatFormat::F64,
                    bits: vec![0],
                }),
            ),
            (
                "malformed_blob".to_string(),
                OValue::Blob {
                    v: "a".to_string(),
                    mime: "application/octet-stream".to_string(),
                },
            ),
        ]));
        let source = render_with(SpliceRenderer::Python, &scope);
        let mut process = spawn_python_shim()?;

        assert_eq!(
            process.exec(&format!("__oval_result__ = {source}"), HashMap::new())?,
            scope,
        );

        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_python_bytes_result_uses_structural_bytes() -> Result<()> {
        let mut process = spawn_python_shim()?;

        let value = process.exec("__oval_result__ = bytes([0, 1, 255])", HashMap::new())?;

        match value {
            OValue::Bytes { v } => {
                assert_eq!(v.bytes, vec![0, 1, 255]);
                assert_eq!(v.media_type.as_deref(), Some("application/octet-stream"));
            }
            other => panic!("expected bytes OValue, got {other:?}"),
        }
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_python_set_result_uses_structural_unordered_set() -> Result<()> {
        use crate::value::SetKind;

        let mut process = spawn_python_shim()?;

        let value = process.exec("__oval_result__ = {1, 2}", HashMap::new())?;

        match value {
            OValue::Set { kind, mut items } => {
                assert_eq!(kind, SetKind::Unordered);
                items.sort_by_key(OValue::canonical_bytes);
                assert_eq!(items, vec![OValue::int(1), OValue::int(2)]);
            }
            other => panic!("expected structural set OValue, got {other:?}"),
        }
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_python_unsupported_object_fails_instead_of_stringifying() -> Result<()> {
        let mut process = spawn_python_shim()?;

        let error = process
            .exec(
                concat!(
                    "class HiddenState:\n",
                    "    def __str__(self):\n",
                    "        return 'silently-erased-object'\n",
                    "__oval_result__ = HiddenState()\n",
                ),
                HashMap::new(),
            )
            .expect_err("an unsupported Python object must not cross as O text");
        let message = format!("{error:#}");

        assert!(
            message.contains("unsupported Python value for OValue projection:")
                && message.contains("HiddenState"),
            "{message}"
        );
        assert!(!message.contains("silently-erased-object"), "{message}");
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_with_string_binding_round_trips_through_shim() -> Result<()> {
        let mut process = spawn_python_shim()?;
        let bindings = HashMap::from([("msg".to_string(), OValue::str_("hello"))]);

        let value = process.exec("__oval_result__ = msg.upper()", bindings)?;

        assert_eq!(value, OValue::str_("HELLO"));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn exec_reports_backend_errors_without_panicking() -> Result<()> {
        let mut process = spawn_python_shim()?;

        let err = process
            .exec("raise RuntimeError('boom from shim')", HashMap::new())
            .unwrap_err();

        assert!(err.to_string().contains("boom from shim"));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn cleanup_command_returns_ok_null() -> Result<()> {
        let mut process = spawn_python_shim()?;

        process.send_command(&BackendWireCommandV2::Cleanup)?;
        let value = expect_done(process.recv_step()?);

        assert_eq!(value, OValue::Null);
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn python_checkpoint_restore_preserves_mutable_alias_cycle() -> Result<()> {
        let mut source = spawn_python_shim()?;
        source.exec(
            "x = []\nx.append(x)\ny = x\n__oval_result__ = 'ready'",
            HashMap::new(),
        )?;
        let checkpoint = source.checkpoint(1024 * 1024)?;
        source.shutdown(backend_shutdown_timeout())?;

        let mut target = spawn_python_shim()?;
        let receipt = target.restore(checkpoint.clone())?;
        assert!(receipt.restored);
        assert_eq!(receipt.checkpoint_sha256, checkpoint.checkpoint_sha256()?);
        assert_eq!(
            target.exec("__oval_result__ = x is y and x[0] is x", HashMap::new())?,
            OValue::bool_(true)
        );
        target.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn python_checkpoint_restore_preserves_unbounded_integers_and_fractions() -> Result<()> {
        let mut source = spawn_python_shim()?;
        source.exec(
            concat!(
                "huge = 10 ** 5000\n",
                "ratio = __import__('fractions').Fraction(huge, 3)\n",
                "__oval_result__ = 'ready'",
            ),
            HashMap::new(),
        )?;
        let checkpoint = source.checkpoint(1024 * 1024)?;
        source.shutdown(backend_shutdown_timeout())?;

        let mut target = spawn_python_shim()?;
        target.restore(checkpoint)?;
        assert_eq!(
            target.exec(
                concat!(
                    "__oval_result__ = (\n",
                    "    huge == 10 ** 5000\n",
                    "    and ratio.numerator == huge\n",
                    "    and ratio.denominator == 3\n",
                    ")",
                ),
                HashMap::new(),
            )?,
            OValue::bool_(true)
        );
        target.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn unsupported_python_checkpoint_pins_and_keeps_actor_live() -> Result<()> {
        let mut process = spawn_python_shim()?;
        process.exec("f = lambda: 42\n__oval_result__ = 'ready'", HashMap::new())?;
        let error = process
            .checkpoint(1024 * 1024)
            .expect_err("functions are outside the constrained graph codec");
        let pin = error
            .downcast_ref::<BackendStatePinned>()
            .expect("checkpoint refusal must retain a typed pin reason");
        assert_eq!(pin.path, "$globals['f']");
        assert_eq!(
            process.exec("__oval_result__ = f()", HashMap::new())?,
            OValue::int(42)
        );
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn shutdown_is_acknowledged_and_process_is_reaped() -> Result<()> {
        let mut process = spawn_python_shim()?;
        let pid = process.child.id();

        process.shutdown(Duration::from_secs(2))?;

        assert!(process.terminal, "backend {pid} was not marked terminal");
        assert!(
            process.child.try_wait()?.is_some(),
            "backend {pid} was not reaped"
        );
        Ok(())
    }

    #[test]
    fn shutdown_timeout_is_one_deadline_across_acknowledgement_and_exit() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let shim = temp.path().join("delayed_shutdown.py");
        let common = Path::new(env!("CARGO_MANIFEST_DIR")).join("backends/o_shim_common.py");
        std::fs::copy(python_shim_path(), &shim)?;

        let source = std::fs::read_to_string(&common)?;
        let original = concat!(
            "            elif tag == \"shutdown\":\n",
            "                send_ok({\"t\": \"null\"})\n",
            "                break\n",
        );
        let delayed = concat!(
            "            elif tag == \"shutdown\":\n",
            "                __import__(\"time\").sleep(0.2)\n",
            "                send_ok({\"t\": \"null\"})\n",
            "                __import__(\"time\").sleep(0.4)\n",
            "                break\n",
        );
        assert!(
            source.contains(original),
            "shared shim shutdown branch changed; update the deadline fixture"
        );
        std::fs::write(
            temp.path().join("o_shim_common.py"),
            source.replacen(original, delayed, 1),
        )?;

        let mut process =
            BackendProcess::new("python", &shim, &BackendSandboxPolicy::none(), None)?;
        let started = Instant::now();
        let error = process
            .shutdown(Duration::from_millis(500))
            .expect_err("acknowledgement and exit must share one 500 ms deadline");

        assert!(started.elapsed() < Duration::from_secs(2), "{error:#}");
        assert!(
            format!("{error:#}").contains("did not terminate within 500 ms"),
            "{error:#}"
        );
        assert!(process.terminal, "forced termination did not reap backend");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn graceful_shutdown_force_kills_lingering_same_group_descendant() -> Result<()> {
        use crate::value::BackendAuthority;

        // DEVNULL is opened read/write by Python's subprocess module, so the
        // fixture needs FileWrite in addition to the Process authority that
        // permits the descendant spawn itself.
        let mut process =
            spawn_python_shim_with([BackendAuthority::Process, BackendAuthority::FileWrite])?;
        let group = i32::try_from(process.child.id())?;
        let value = process.exec(
            concat!(
                "import subprocess\n",
                "_o_lingering_child = subprocess.Popen(\n",
                "    ['/bin/sleep', '60'],\n",
                "    stdin=subprocess.DEVNULL,\n",
                "    stdout=subprocess.DEVNULL,\n",
                "    stderr=subprocess.DEVNULL,\n",
                ")\n",
                "__oval_result__ = _o_lingering_child.pid",
            ),
            HashMap::new(),
        )?;
        let descendant = i32::try_from(value.as_int()?)?;
        // SAFETY: `getpgid` only inspects the live PID returned by Popen.
        assert_eq!(unsafe { libc::getpgid(descendant) }, group);

        let error = process
            .shutdown(Duration::from_millis(500))
            .expect_err("a lingering active descendant must fail graceful shutdown");

        assert!(
            format!("{error:#}").contains("still contains an active descendant"),
            "{error:#}"
        );
        assert!(process.terminal, "forced group termination did not settle");
        assert!(
            owned_group_has_no_active_descendants(group)?,
            "backend group {group} retained an active descendant after forced shutdown"
        );
        assert!(
            process.child.try_wait()?.is_some(),
            "backend group leader was not reaped"
        );
        Ok(())
    }

    #[test]
    fn registry_shutdown_all_is_explicit_and_drains_every_process() -> Result<()> {
        let mut registry = ProcessRegistry::new();
        let sandbox = BackendSandboxPolicy::none();
        for env_id in [0, 1] {
            registry.send_exec(
                "python",
                env_id,
                "__oval_result__ = 'done'",
                HashMap::new(),
                BackendLaunchContext {
                    shim_path: &python_shim_path(),
                    sandbox: &sandbox,
                    executable_leases: None,
                    launch_generation_sha256: None,
                },
            )?;
            assert!(matches!(
                registry.recv_exec_step("python", env_id, &sandbox)?,
                ExecStep::Done(value) if value == OValue::str_("done")
            ));
        }

        registry.shutdown_all(Duration::from_secs(2))?;
        assert!(registry.registry.is_empty());
        Ok(())
    }

    #[test]
    fn session_quota_refuses_new_actor_without_evicting_open_session() -> Result<()> {
        let mut registry = ProcessRegistry::with_session_limits(1, 1);
        let sandbox = BackendSandboxPolicy::none();
        let shim = python_shim_path();
        registry.exec(
            "python",
            1,
            "retained = 42\n'created'",
            HashMap::new(),
            test_launch_context(&shim, &sandbox, "quota-test-generation"),
        )?;

        let error = registry
            .exec(
                "python",
                2,
                "'must-not-run'",
                HashMap::new(),
                test_launch_context(&shim, &sandbox, "quota-test-generation"),
            )
            .expect_err("a second actor must exceed the configured quota");
        assert!(
            format!("{error:#}").contains("session.capacity-exhausted"),
            "{error:#}"
        );

        assert_eq!(
            registry.exec(
                "python",
                1,
                "retained",
                HashMap::new(),
                test_launch_context(&shim, &sandbox, "quota-test-generation"),
            )?,
            OValue::int(42)
        );
        registry.shutdown_all(Duration::from_secs(2))?;
        Ok(())
    }

    #[test]
    fn semantic_error_does_not_retire_session_owned_actor() -> Result<()> {
        let mut registry = ProcessRegistry::new();
        let sandbox = BackendSandboxPolicy::none();
        let shim = python_shim_path();
        registry.exec(
            "python",
            3,
            "retained = 42\n'created'",
            HashMap::new(),
            test_launch_context(&shim, &sandbox, "semantic-error-generation"),
        )?;
        registry
            .exec(
                "python",
                3,
                "raise RuntimeError('expected')",
                HashMap::new(),
                test_launch_context(&shim, &sandbox, "semantic-error-generation"),
            )
            .expect_err("the language-level error must surface");
        assert_eq!(
            registry.exec(
                "python",
                3,
                "retained",
                HashMap::new(),
                test_launch_context(&shim, &sandbox, "semantic-error-generation"),
            )?,
            OValue::int(42)
        );
        registry.shutdown_all(Duration::from_secs(2))?;
        Ok(())
    }

    #[test]
    fn persistent_actor_refuses_implicit_launch_generation_replacement() -> Result<()> {
        let mut registry = ProcessRegistry::new();
        let sandbox = BackendSandboxPolicy::none();
        let shim = python_shim_path();
        let first_generation = "admitted-launch-generation-v1";
        let second_generation = "admitted-launch-generation-v2";

        let first = registry.exec(
            "python",
            7,
            concat!(
                "import os\n",
                "retained_across_commands = 'present'\n",
                "__oval_result__ = str(os.getpid())",
            ),
            HashMap::new(),
            BackendLaunchContext {
                shim_path: &shim,
                sandbox: &sandbox,
                executable_leases: None,
                launch_generation_sha256: Some(first_generation),
            },
        )?;
        let first_pid = first.as_str()?.to_string();

        let same_generation = registry.exec(
            "python",
            7,
            "__oval_result__ = retained_across_commands",
            HashMap::new(),
            BackendLaunchContext {
                shim_path: &shim,
                sandbox: &sandbox,
                executable_leases: None,
                launch_generation_sha256: Some(first_generation),
            },
        )?;
        assert_eq!(same_generation, OValue::str_("present"));

        let error = registry
            .exec(
                "python",
                7,
                "__oval_result__ = 'must-not-run'",
                HashMap::new(),
                BackendLaunchContext {
                    shim_path: &shim,
                    sandbox: &sandbox,
                    executable_leases: None,
                    launch_generation_sha256: Some(second_generation),
                },
            )
            .expect_err("an open session must not be implicitly retired");
        assert!(
            format!("{error:#}").contains("session.generation-conflict"),
            "{error:#}"
        );

        let retained = registry.exec(
            "python",
            7,
            "import os\n__oval_result__ = str(os.getpid()) + ':' + retained_across_commands",
            HashMap::new(),
            BackendLaunchContext {
                shim_path: &shim,
                sandbox: &sandbox,
                executable_leases: None,
                launch_generation_sha256: Some(first_generation),
            },
        )?;
        assert_eq!(retained.as_str()?, format!("{first_pid}:present"));

        registry.shutdown_all(Duration::from_secs(2))?;
        Ok(())
    }

    #[test]
    fn nonresponsive_shutdown_is_bounded_and_fails_explicitly() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let shim = temp.path().join("nonresponsive.py");
        std::fs::write(&shim, "import time\ntime.sleep(60)\n")?;
        let mut process =
            BackendProcess::new("python", &shim, &BackendSandboxPolicy::none(), None)?;
        let started = Instant::now();

        let error = process
            .shutdown(Duration::from_millis(100))
            .expect_err("a nonresponsive backend must fail shutdown");

        assert!(started.elapsed() < Duration::from_secs(2), "{error:#}");
        assert!(
            format!("{error:#}").contains("did not answer within 100 ms"),
            "{error:#}"
        );
        assert!(process.terminal, "forced termination did not reap backend");
        Ok(())
    }

    #[test]
    fn restricted_python_shim_denies_process_spawn() -> Result<()> {
        let mut process = spawn_python_shim()?;
        let error = process
            .exec(
                "import os\n__oval_result__ = os.system('echo forbidden')",
                HashMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("denies process spawn"));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn restricted_python_shim_denies_filesystem_write() -> Result<()> {
        let mut process = spawn_python_shim()?;
        let error = process
            .exec(
                "open('/tmp/o-backend-forbidden', 'w').write('no')",
                HashMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("denies filesystem write"));
        assert!(!Path::new("/tmp/o-backend-forbidden").exists());
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn restricted_python_shim_denies_filesystem_read_outside_runtime() -> Result<()> {
        let mut process = spawn_python_shim()?;
        let error = process
            .exec(
                "__oval_result__ = open('/etc/hosts').read()",
                HashMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("denies filesystem read"));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn declared_filesystem_read_authority_changes_the_sandbox_policy() -> Result<()> {
        use crate::value::BackendAuthority;

        let mut process = spawn_python_shim_with([BackendAuthority::FileRead])?;
        let value = process.exec(
            "__oval_result__ = len(open('/etc/hosts').read()) > 0",
            HashMap::new(),
        )?;
        assert_eq!(value, OValue::bool_(true));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn restricted_python_shim_denies_network_socket_creation() -> Result<()> {
        let mut process = spawn_python_shim()?;
        let error = process
            .exec(
                "import socket\n__oval_result__ = socket.socket()",
                HashMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("denies network access"));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn declared_process_authority_changes_the_sandbox_policy() -> Result<()> {
        use crate::value::BackendAuthority;

        let mut process = spawn_python_shim_with([BackendAuthority::Process])?;
        let value = process.exec(
            "import os\n__oval_result__ = os.system('true')",
            HashMap::new(),
        )?;
        assert_eq!(value, OValue::int(0));
        process.shutdown(backend_shutdown_timeout())?;
        Ok(())
    }

    #[test]
    fn python_bootstrap_skips_runtime_candidates_that_cannot_be_resolved() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let probe = temp.path().join("probe.py");
        std::fs::write(&probe, "print('bootstrap-ok')\n")?;

        let denied_candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or_else(|| Path::new("/Users"))
            .to_path_buf();

        let python = which::which("python3").context("python3 is required for backend shims")?;

        #[cfg(target_os = "macos")]
        let mut command =
            macos_sandbox_command(&python, &BackendSandboxPolicy::none(), temp.path())?;

        #[cfg(not(target_os = "macos"))]
        let mut command = Command::new(&python);

        let output = command
            .arg("-c")
            .arg(PYTHON_POLICY_BOOTSTRAP)
            .arg(&probe)
            .env("O_BACKEND_AUTHORITIES", "[]")
            .env(
                "O_BACKEND_RUNTIME_ROOTS",
                serde_json::to_string(&[temp.path(), &denied_candidate])?,
            )
            .output()
            .context("failed to run Python policy bootstrap probe")?;

        assert!(
            output.status.success(),
            "bootstrap failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("bootstrap-ok"));
        Ok(())
    }
}
