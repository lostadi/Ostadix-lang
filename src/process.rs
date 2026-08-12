use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, BufReader, BufWriter, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
#[cfg(not(unix))]
use std::sync::OnceLock;
use std::sync::{mpsc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::capability::BackendSandboxPolicy;
use crate::value::{OValue, OWireCommand, OWireResponse};
use crate::wire;

static LIFECYCLE_TRACE_LOCK: Mutex<()> = Mutex::new(());
const DEFAULT_BACKEND_OPERATION_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_BACKEND_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const BACKEND_FALLBACK_REAP_TIMEOUT: Duration = Duration::from_millis(250);

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
    responses: mpsc::Receiver<std::result::Result<OWireResponse, String>>,
    reader: Option<JoinHandle<()>>,
    terminal: bool,
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
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
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
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
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
            serde_json::to_string(&[runtime_root])?,
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
) -> Result<Command> {
    let executable = std::env::current_exe().context("failed to locate current executable")?;
    let executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_path_buf());
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
    let mut command = macos_sandbox_command(&executable, sandbox, &runtime_root)?;
    #[cfg(not(target_os = "macos"))]
    let mut command = Command::new(&executable);

    command.arg("--o-backend").arg(lang).env(
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

#[cfg(target_os = "macos")]
fn macos_sandbox_command(
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

    let mut command = Command::new("/usr/bin/sandbox-exec");
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
    fn new(lang: &str, shim_path: &Path, sandbox: &BackendSandboxPolicy) -> Result<Self> {
        #[cfg(test)]
        let mut command = legacy_backend_command(shim_path, sandbox)?;

        #[cfg(not(test))]
        let mut command = rust_backend_command(lang, shim_path, sandbox)?;

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
                match wire::read_frame::<_, OWireResponse>(&mut stdout) {
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
        })
    }

    fn send_command(&mut self, command: &OWireCommand) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("backend command channel is closed"))?;
        wire::write_frame(stdin, command).context("failed to write backend wire command")
    }

    fn recv_step(&mut self) -> Result<ExecStep> {
        let response = self
            .responses
            .recv()
            .map_err(|_| anyhow!("backend process closed stdout unexpectedly"))?
            .map_err(anyhow::Error::msg)?;

        Self::response_step(response)
    }

    fn recv_step_timeout(&mut self, timeout: Duration) -> Result<ExecStep> {
        let response = self
            .responses
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
            .map_err(anyhow::Error::msg)?;

        Self::response_step(response)
    }

    fn response_step(response: OWireResponse) -> Result<ExecStep> {
        match response {
            OWireResponse::Ok { value } => Ok(ExecStep::Done(value)),
            OWireResponse::Err { message } => {
                Err(anyhow::Error::new(BackendSemanticError(message)))
            }
            OWireResponse::EvalRequest { src, scope } => Ok(ExecStep::EvalRequest { src, scope }),
        }
    }

    fn send_eval_result(&mut self, value: OValue) -> Result<()> {
        self.send_command(&OWireCommand::EvalResult { value })
    }

    fn exec(&mut self, code: &str, bindings: HashMap<String, OValue>) -> Result<OValue> {
        self.send_command(&OWireCommand::Exec {
            code: code.to_string(),
            bindings,
        })?;
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

    fn shutdown(&mut self, timeout: Duration) -> Result<()> {
        let deadline = bounded_deadline(timeout, "backend shutdown")?;
        if let Err(error) = self.send_command(&OWireCommand::Shutdown) {
            let termination = self.force_terminate(BACKEND_FALLBACK_REAP_TIMEOUT);
            return match termination {
                Ok(()) => Err(error.context("failed to send backend shutdown")),
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
                    Ok(()) => Err(error.context("backend shutdown was not acknowledged")),
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
    mut evaluate: F,
) -> Result<OValue>
where
    F: FnMut(String, Option<OValue>, Duration) -> Result<OValue>,
{
    let mut process = BackendProcess::new(language, shim_path, sandbox)
        .with_context(|| format!("failed to start ephemeral backend `{language}`"))
        .map_err(infrastructure_error)?;
    let operation_timeout = backend_operation_timeout();
    let operation_deadline = Instant::now()
        .checked_add(operation_timeout)
        .ok_or_else(|| infrastructure_error(anyhow!("backend operation deadline overflowed")))?;

    let execution = (|| {
        process
            .send_command(&OWireCommand::Exec {
                code: code.to_string(),
                bindings,
            })
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
                .shutdown(backend_shutdown_timeout())
                .with_context(|| {
                    format!(
                        "ephemeral backend `{language}` returned a value but did not shut down cleanly"
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

pub struct ProcessRegistry {
    registry: HashMap<(String, u32, BackendSandboxPolicy), BackendProcess>,
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
        shim_path: &Path,
        sandbox: &BackendSandboxPolicy,
    ) -> Result<()> {
        let key = (lang.to_string(), env_id, sandbox.clone());
        if !self.registry.contains_key(&key) {
            let process = BackendProcess::new(lang, shim_path, sandbox)
                .with_context(|| format!("failed to start backend for language `{lang}`"))?;
            self.registry.insert(key.clone(), process);
        }
        self.registry
            .get_mut(&key)
            .expect("backend was just inserted but is missing")
            .send_command(&OWireCommand::Exec {
                code: code.to_string(),
                bindings,
            })
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
        let key = (lang.to_string(), env_id, sandbox.clone());
        let step = self
            .registry
            .get_mut(&key)
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?
            .recv_step();

        if step.is_err() {
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
        let key = (lang.to_string(), env_id, sandbox.clone());
        let step = self
            .registry
            .get_mut(&key)
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?
            .recv_step_timeout(timeout);
        let step = match step {
            Ok(step) => step,
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
        let key = (lang.to_string(), env_id, sandbox.clone());
        self.registry
            .get_mut(&key)
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?
            .send_eval_result(value)
            .with_context(|| format!("failed to send eval_result to backend `{lang}`"))
    }

    pub(crate) fn exec(
        &mut self,
        lang: &str,
        env_id: u32,
        code: &str,
        bindings: HashMap<String, OValue>,
        shim_path: &Path,
        sandbox: &BackendSandboxPolicy,
    ) -> Result<OValue> {
        let key = (lang.to_string(), env_id, sandbox.clone());

        if !self.registry.contains_key(&key) {
            let process = BackendProcess::new(lang, shim_path, sandbox)
                .with_context(|| format!("failed to start backend for language `{lang}`"))?;
            self.registry.insert(key.clone(), process);
        }

        let result = self
            .registry
            .get_mut(&key)
            .expect("backend was just inserted but is missing")
            .exec(code, bindings);

        if result.is_err() {
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

    pub fn cleanup_env(&mut self, lang: &str, env_id: u32) -> Result<()> {
        let keys = self
            .registry
            .keys()
            .filter(|(candidate_lang, candidate_env, _)| {
                candidate_lang == lang && *candidate_env == env_id
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(mut process) = self.registry.remove(&key) {
                process.shutdown(backend_shutdown_timeout())?;
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

    fn python_shim_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("backends/python_shim.py")
    }

    fn spawn_python_shim() -> Result<BackendProcess> {
        BackendProcess::new("python", &python_shim_path(), &BackendSandboxPolicy::none())
    }

    fn spawn_python_shim_with(
        permissions: impl IntoIterator<Item = crate::value::BackendAuthority>,
    ) -> Result<BackendProcess> {
        BackendProcess::new(
            "python",
            &python_shim_path(),
            &BackendSandboxPolicy::new(permissions),
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

        process.send_command(&OWireCommand::Ping)?;
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

        process.send_command(&OWireCommand::Cleanup)?;
        let value = expect_done(process.recv_step()?);

        assert_eq!(value, OValue::Null);
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
        std::fs::copy(&common, temp.path().join("o_shim_common.py"))?;

        let source = std::fs::read_to_string(python_shim_path())?;
        let original = concat!(
            "        elif tag == \"shutdown\":\n",
            "            send_ok(None)\n",
            "            break\n",
        );
        let delayed = concat!(
            "        elif tag == \"shutdown\":\n",
            "            __import__(\"time\").sleep(0.2)\n",
            "            send_ok(None)\n",
            "            __import__(\"time\").sleep(0.4)\n",
            "            break\n",
        );
        assert!(
            source.contains(original),
            "Python shim shutdown branch changed; update the deadline fixture"
        );
        std::fs::write(&shim, source.replacen(original, delayed, 1))?;

        let mut process = BackendProcess::new("python", &shim, &BackendSandboxPolicy::none())?;
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
                &python_shim_path(),
                &sandbox,
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
    fn nonresponsive_shutdown_is_bounded_and_fails_explicitly() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let shim = temp.path().join("nonresponsive.py");
        std::fs::write(&shim, "import time\ntime.sleep(60)\n")?;
        let mut process = BackendProcess::new("python", &shim, &BackendSandboxPolicy::none())?;
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
