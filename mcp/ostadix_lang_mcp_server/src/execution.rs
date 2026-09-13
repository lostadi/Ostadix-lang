//! Session-owned concurrent processes. Output is spooled in full to disk;
//! response limits affect previews only and never terminate a chatty process.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex as AsyncMutex};

const MAX_READ_BYTES: usize = 256 * 1024;
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
type Reader = Box<dyn AsyncRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

pub struct ExecutionRequest {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    /// Initial input; EOF is a separate, explicit write(..., close=true).
    pub stdin: Option<String>,
    pub timeout_secs: Option<u64>,
    pub pty: bool,
}

#[derive(Clone)]
pub struct JobManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    session: String,
    directory: PathBuf,
    sequence: AtomicU64,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

struct Job {
    pid: u32,
    ownership: Arc<ProcessOwnership>,
    stdout: PathBuf,
    stderr: PathBuf,
    pty: bool,
    input: Arc<AsyncMutex<Option<Writer>>>,
    input_open: AtomicBool,
    running: AtomicBool,
    cancel: watch::Sender<bool>,
    state: watch::Sender<Value>,
}

struct ProcessOwnership {
    pid: u32,
    active: Mutex<bool>,
}

impl ProcessOwnership {
    fn cleanup(&self) -> CleanupReport {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *active {
            terminate_session(self.pid)
        } else {
            CleanupReport::default()
        }
    }

    fn disarm(&self) {
        // Serialize only cleanup/disarm for this one job. Once this returns,
        // no concurrent drop guard can signal the soon-to-be-reaped identity.
        *self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = false;
    }
}

// Until start has delivered a usable ID, the caller cannot install its own
// cancellation guard. Cover cancellation/errors at every post-spawn await.
struct StartHandoff(Option<Arc<Job>>);
impl Drop for StartHandoff {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            job.cancel.send_replace(true);
            job.ownership.cleanup();
        }
    }
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ManagerInner {
    fn drop(&mut self) {
        // Monitors do not own ManagerInner, so the last session handle dropping
        // cancels jobs even while those monitors still own individual Job Arcs.
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        for job in jobs.values() {
            if job.running.load(Ordering::Acquire) {
                job.cancel.send_replace(true);
                job.ownership.cleanup();
            }
        }
    }
}

impl JobManager {
    pub fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let session = SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::new(ManagerInner {
                session: format!("{}-{stamp}-{session}", std::process::id()),
                directory: std::env::temp_dir().join(format!(
                    "ostadix-mcp-jobs-{}-{stamp}-{session}",
                    std::process::id()
                )),
                sequence: AtomicU64::new(1),
                jobs: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn job(&self, id: &str) -> Result<Arc<Job>, String> {
        self.inner
            .jobs
            .lock()
            .map_err(|_| "job registry lock poisoned".to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown job {id}; jobs belong to this MCP session"))
    }

    pub async fn start(&self, request: ExecutionRequest) -> Result<Value, String> {
        if request.timeout_secs == Some(0) {
            return Err("timeout_secs must be positive when specified".into());
        }
        let id = format!(
            "job-{}-{}",
            self.inner.session,
            self.inner.sequence.fetch_add(1, Ordering::Relaxed)
        );
        let directory = self.inner.directory.join(&id);
        create_private_directory(&directory)?;
        let stdout = directory.join("stdout.log");
        let stderr = directory.join("stderr.log");
        let stdout_file = tokio::fs::File::create(&stdout)
            .await
            .map_err(|e| format!("create stdout log: {e}"))?;
        let stderr_file = tokio::fs::File::create(&stderr)
            .await
            .map_err(|e| format!("create stderr log: {e}"))?;

        let mut command = Command::new(&request.program);
        command
            .args(&request.args)
            .current_dir(&request.cwd)
            .envs(&request.env)
            .kill_on_drop(true);
        if request.pty && !request.env.contains_key("TERM") && std::env::var_os("TERM").is_none() {
            command.env("TERM", "xterm-256color");
        }
        let (child, input, output, errors) = spawn(&mut command, request.pty)
            .map_err(|e| format!("spawn {}: {e}", request.program.display()))?;
        let pid = child.id().ok_or("spawn returned no process ID")?;
        let (cancel, cancel_rx) = watch::channel(false);
        let (state, _) = watch::channel(json!({
            "job_id": id, "pid": pid, "state": "running", "exit_code": null,
            "signal": null, "pty": request.pty, "error": null,
            "initial_input_state": if request.stdin.is_some() {"pending"} else {"not_supplied"},
            "input_error": null,
            "session_scoped": true, "restart_persistence": false,
            "cleanup": {"process_group_signaled": false, "child_reaped": false,
                "logs_drained": false},
        }));
        let job = Arc::new(Job {
            pid,
            ownership: Arc::new(ProcessOwnership {
                pid,
                active: Mutex::new(true),
            }),
            stdout,
            stderr,
            pty: request.pty,
            input: Arc::new(AsyncMutex::new(Some(input))),
            input_open: AtomicBool::new(true),
            running: AtomicBool::new(true),
            cancel,
            state,
        });
        let mut handoff = StartHandoff(Some(job.clone()));
        // Acquire before publishing the job: listing/writing from another tool
        // must not overtake the initial input, and start must not await before
        // the monitor takes ownership of the spawned child.
        let seeded_input = request.stdin.map(|initial| {
            (
                initial,
                job.input
                    .clone()
                    .try_lock_owned()
                    .expect("new job input is unlocked"),
            )
        });
        self.inner
            .jobs
            .lock()
            .map_err(|_| "job registry lock poisoned".to_string())?
            .insert(id, job.clone());

        // Reserve the per-job input lock before returning start, so a subsequent
        // EOF/write cannot overtake the caller's initial input.
        if let Some((initial, mut input)) = seeded_input {
            let input_job = job.clone();
            tokio::spawn(async move {
                let result = async {
                    let writer = input
                        .as_mut()
                        .ok_or("initial stdin was closed".to_string())?;
                    writer
                        .write_all(initial.as_bytes())
                        .await
                        .map_err(|e| format!("initial stdin: {e}"))?;
                    writer
                        .flush()
                        .await
                        .map_err(|e| format!("initial stdin flush: {e}"))
                }
                .await;
                input_job.state.send_modify(|state| {
                    state["initial_input_state"] =
                        json!(if result.is_ok() { "written" } else { "failed" });
                    state["input_error"] = json!(result.err());
                });
            });
        }
        let spool = tokio::spawn(async move {
            tokio::try_join!(
                spool_stream(output, stdout_file),
                spool_stream(errors, stderr_file)
            )
            .map(|_| ())
        });
        tokio::spawn(monitor(
            job.clone(),
            child,
            spool,
            cancel_rx,
            request.timeout_secs,
        ));
        let result = snapshot(&job).await?;
        handoff.0 = None;
        Ok(result)
    }

    pub async fn status(&self, id: &str) -> Result<Value, String> {
        let job = self.job(id)?;
        snapshot(&job).await
    }

    pub async fn list(&self) -> Result<Value, String> {
        let mut jobs: Vec<_> = self
            .inner
            .jobs
            .lock()
            .map_err(|_| "job registry lock poisoned".to_string())?
            .iter()
            .map(|(id, job)| (id.clone(), job.clone()))
            .collect();
        jobs.sort_by(|left, right| left.0.cmp(&right.0));
        let mut entries = Vec::with_capacity(jobs.len());
        for (_, job) in jobs {
            entries.push(snapshot(&job).await?);
        }
        Ok(json!({"jobs": entries, "session_scoped": true, "restart_persistence": false}))
    }

    pub async fn wait(&self, id: &str) -> Result<Value, String> {
        let job = self.job(id)?;
        let mut state = job.state.subscribe();
        loop {
            if state.borrow_and_update()["state"] != "running" {
                return snapshot(&job).await;
            }
            state.changed().await.map_err(|_| "job monitor closed")?;
        }
    }

    pub async fn read(
        &self,
        id: &str,
        stream: &str,
        offset: u64,
        limit: usize,
    ) -> Result<Value, String> {
        self.read_encoded(id, stream, offset, limit, "utf8_lossy")
            .await
    }

    pub async fn read_encoded(
        &self,
        id: &str,
        stream: &str,
        offset: u64,
        limit: usize,
        encoding: &str,
    ) -> Result<Value, String> {
        if !matches!(encoding, "utf8_lossy" | "base64") {
            return Err("encoding must be utf8_lossy or base64".into());
        }
        let job = self.job(id)?;
        let path = match stream {
            "stdout" => &job.stdout,
            "stderr" => &job.stderr,
            _ => return Err("stream must be stdout or stderr".into()),
        };
        let limit = limit.clamp(1, MAX_READ_BYTES);
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| format!("open log: {e}"))?;
        let total_bytes = file.metadata().await.map_err(|e| e.to_string())?.len();
        if offset > total_bytes {
            return Err(format!(
                "offset {offset} exceeds current log size {total_bytes}"
            ));
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| format!("seek log: {e}"))?;
        let mut bytes = vec![0; limit.min((total_bytes - offset).min(usize::MAX as u64) as usize)];
        file.read_exact(&mut bytes)
            .await
            .map_err(|e| format!("read log: {e}"))?;
        let next_offset = offset + bytes.len() as u64;
        let mut result = json!({
            "job_id": id, "stream": stream, "path": path,
            "offset": offset, "next_offset": next_offset,
            "encoding": encoding,
            "bytes_read": bytes.len(), "total_bytes": total_bytes,
            "limit_bytes": limit, "eof": next_offset >= total_bytes,
            "complete": !job.running.load(Ordering::Acquire),
            "merged_into": if job.pty && stream == "stderr" { Some("stdout") } else { None },
        });
        if encoding == "base64" {
            use base64::Engine;
            result["data"] = json!(base64::engine::general_purpose::STANDARD.encode(&bytes));
        } else {
            result["text"] = json!(String::from_utf8_lossy(&bytes));
        }
        Ok(result)
    }

    pub async fn write(&self, id: &str, input: &str, close: bool) -> Result<Value, String> {
        self.write_with_timeout(id, input, close, Some(30)).await
    }

    pub async fn write_with_timeout(
        &self,
        id: &str,
        input: &str,
        close: bool,
        timeout_secs: Option<u64>,
    ) -> Result<Value, String> {
        if timeout_secs == Some(0) {
            return Err("stdin timeout must be positive when specified".into());
        }
        let job = self.job(id)?;
        let mut written = 0;
        let mut eof_bytes_written = 0;
        let operation = async {
            let mut slot = job.input.lock().await;
            let writer = slot.as_mut().ok_or("job stdin is closed")?;
            if !job.running.load(Ordering::Acquire) {
                return Err("job has already finished".into());
            }
            write_counted(writer, input.as_bytes(), &mut written).await?;
            writer
                .flush()
                .await
                .map_err(|e| format!("flush stdin: {e}"))?;
            if close {
                if job.pty {
                    // Terminals have no write-half close. With default canonical
                    // settings, Ctrl-D flushes a pending line, then signals EOF.
                    write_counted(writer, b"\x04\x04", &mut eof_bytes_written).await?;
                    writer
                        .flush()
                        .await
                        .map_err(|e| format!("terminal flush: {e}"))?;
                } else {
                    writer
                        .shutdown()
                        .await
                        .map_err(|e| format!("close stdin: {e}"))?;
                }
                *slot = None;
                job.input_open.store(false, Ordering::Release);
            }
            Ok(
                json!({"job_id": id, "bytes_written": written, "stdin_open": !close,
                "eof": if close && job.pty {"terminal_veof_sent"} else if close {"pipe_closed"} else {"not_requested"}}),
            )
        };
        let result = match timeout_secs {
            Some(seconds) => {
                match tokio::time::timeout(Duration::from_secs(seconds), operation).await {
                    Ok(result) => result,
                    Err(_) => Err(format!(
                        "stdin write timed out after {seconds}s (including input-lock wait)"
                    )),
                }
            }
            None => operation.await,
        };
        result.map_err(|error: String| format!("{error}; {written} of {} input bytes accepted by stdin transport; {eof_bytes_written} terminal EOF bytes sent; EOF completion unconfirmed", input.len()))
    }

    pub async fn cancel(&self, id: &str) -> Result<Value, String> {
        self.request_cancel(id)?;
        self.wait(id).await
    }

    /// Signal every job first, then observe all monitors finish their cleanup.
    pub async fn shutdown(&self) -> Result<(), String> {
        let ids: Vec<_> = self
            .inner
            .jobs
            .lock()
            .map_err(|_| "job registry lock poisoned".to_string())?
            .iter()
            .filter(|(_, job)| job.running.load(Ordering::Acquire))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.request_cancel(id)?;
        }
        for id in &ids {
            self.wait(id).await?;
        }
        Ok(())
    }

    /// Signal cancellation without waiting, for cancellation-safe caller guards.
    /// Use cancel() when a response must include the observed cleanup result.
    pub fn request_cancel(&self, id: &str) -> Result<(), String> {
        self.job(id)?.cancel.send_replace(true);
        Ok(())
    }
}

async fn write_counted(
    writer: &mut Writer,
    bytes: &[u8],
    written: &mut usize,
) -> Result<(), String> {
    while *written < bytes.len() {
        let count = writer
            .write(&bytes[*written..])
            .await
            .map_err(|e| format!("write stdin: {e}"))?;
        if count == 0 {
            return Err("write stdin made no progress".into());
        }
        *written += count;
    }
    Ok(())
}

fn create_private_directory(path: &std::path::Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|e| format!("create job log directory: {e}"))
}

async fn snapshot(job: &Job) -> Result<Value, String> {
    let mut state = job.state.borrow().clone();
    let (stdout, stderr) = tokio::join!(
        tokio::fs::metadata(&job.stdout),
        tokio::fs::metadata(&job.stderr)
    );
    state["stdout"] =
        json!({"path": job.stdout, "bytes": stdout.map_err(|e| e.to_string())?.len()});
    state["stderr"] = json!({"path": job.stderr, "bytes": stderr.map_err(|e| e.to_string())?.len(),
        "merged_into": if job.pty {Some("stdout")} else {None}});
    state["stdin_open"] = json!(job.input_open.load(Ordering::Acquire));
    state["stdin_eof_mode"] = json!(if job.pty {
        "terminal_veof_canonical_mode_only"
    } else {
        "pipe_close"
    });
    Ok(state)
}

async fn spool_stream(mut source: Reader, mut log: tokio::fs::File) -> Result<(), String> {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let count = source
            .read(&mut buffer)
            .await
            .map_err(|e| format!("read process output: {e}"))?;
        if count == 0 {
            break;
        }
        log.write_all(&buffer[..count])
            .await
            .map_err(|e| format!("write process log: {e}"))?;
    }
    log.flush()
        .await
        .map_err(|e| format!("flush process log: {e}"))
}

// Also runs when the Tokio runtime drops an unfinished monitor future.
struct SessionGuard(Arc<ProcessOwnership>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.cleanup();
    }
}

#[derive(Default)]
struct CleanupReport {
    group_signaled: bool,
    processes_signaled: usize,
    session_scan_complete: bool,
    error: Option<String>,
}

/// Ordinary O backends create their own groups. The session created at launch
/// therefore owns the cancellation boundary, including groups whose leader has
/// already exited/reparented. A deliberate setsid daemon is outside that boundary.
/// Scanning happens only during cleanup; execution has no polling or global lock.
fn terminate_session(pid: u32) -> CleanupReport {
    let mut report = CleanupReport::default();
    #[cfg(unix)]
    if let Ok(session) = i32::try_from(pid) {
        let mut owned = BTreeSet::new();
        for _ in 0..8 {
            let pids = match process_ids() {
                Ok(pids) => pids,
                Err(error) => {
                    report.error = Some(error);
                    // Preserve verified original-group cleanup if the native
                    // census is unavailable; never act on a stale group number.
                    if unsafe { libc::getsid(session) } == session {
                        owned.insert(session);
                    }
                    break;
                }
            };
            let mut discovered = false;
            for candidate in pids {
                if candidate > 0 && unsafe { libc::getsid(candidate) } == session {
                    // A stopped member cannot fork or move to another session;
                    // repeat the census to include children forked during it.
                    unsafe {
                        libc::kill(candidate, libc::SIGSTOP);
                    }
                    discovered |= owned.insert(candidate);
                }
            }
            if !discovered {
                report.session_scan_complete = true;
                break;
            }
        }
        if !report.session_scan_complete && report.error.is_none() {
            report.error =
                Some("session discovery did not stabilize after eight cleanup scans".into());
        }
        // The original group may have disappeared after leader reaping. Never
        // signal its stored numeric ID without a presently verified member.
        if owned.iter().any(|candidate| unsafe {
            libc::getsid(*candidate) == session && libc::getpgid(*candidate) == session
        }) {
            report.group_signaled = unsafe { libc::kill(-session, libc::SIGKILL) } == 0;
        }
        // Recheck each PID immediately before signaling, including the leader.
        // Membership is the authority; a stored PID alone is insufficient.
        for candidate in owned {
            if unsafe { libc::getsid(candidate) } == session
                && unsafe { libc::kill(candidate, libc::SIGKILL) } == 0
            {
                report.processes_signaled += 1;
            }
        }
    }
    let _ = pid;
    report
}

#[cfg(target_os = "macos")]
fn process_ids() -> Result<Vec<i32>, String> {
    let mut pids = vec![0i32; 1024];
    loop {
        let bytes = i32::try_from(pids.len() * std::mem::size_of::<i32>())
            .map_err(|_| "process census is too large".to_string())?;
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Err(format!(
                "process census: {}",
                std::io::Error::last_os_error()
            ));
        }
        if (count as usize) < pids.len() {
            pids.truncate(count as usize);
            return Ok(pids);
        }
        pids.resize(pids.len() * 2, 0);
    }
}

#[cfg(target_os = "linux")]
fn process_ids() -> Result<Vec<i32>, String> {
    let entries = std::fs::read_dir("/proc").map_err(|error| format!("process census: {error}"))?;
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse().ok())
        .collect())
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn process_ids() -> Result<Vec<i32>, String> {
    Err("native session enumeration is unavailable on this Unix platform; original process-group cleanup only".into())
}

#[cfg(unix)]
fn exit_observation(pid: u32) -> Result<Option<bool>, String> {
    loop {
        let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut information,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("observe unreaped child: {error}"));
        }
        if unsafe { information.si_pid() } != 0 {
            let success =
                information.si_code == libc::CLD_EXITED && unsafe { information.si_status() } == 0;
            // WNOWAIT keeps the exited leader's PID/session identity reserved
            // until output drains and all session-cleanup guards are disarmed.
            return Ok(Some(success));
        }
        return Ok(None);
    }
}

#[cfg(unix)]
async fn observe_exit(
    _child: &mut Child,
    pid: u32,
) -> Result<(bool, Option<std::process::ExitStatus>), String> {
    // Subscribe before probing: an exit between the probe and recv must remain
    // observable. SIGCHLD wakes the existing Tokio reactor; there is no timer or
    // recurring process census during ordinary execution.
    let mut changes = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())
        .map_err(|error| format!("observe child exit signal: {error}"))?;
    loop {
        if let Some(success) = exit_observation(pid)? {
            return Ok((success, None));
        }
        if changes.recv().await.is_none() {
            return Err("child-exit signal stream closed".into());
        }
    }
}

#[cfg(not(unix))]
async fn observe_exit(
    child: &mut Child,
    _pid: u32,
) -> Result<(bool, Option<std::process::ExitStatus>), String> {
    let status = child
        .wait()
        .await
        .map_err(|error| format!("wait process: {error}"))?;
    Ok((status.success(), Some(status)))
}

async fn monitor(
    job: Arc<Job>,
    mut child: Child,
    mut spool: tokio::task::JoinHandle<Result<(), String>>,
    mut cancel: watch::Receiver<bool>,
    timeout_secs: Option<u64>,
) {
    let guard = SessionGuard(job.ownership.clone());
    let mut exit = None;
    let mut successful_exit = false;
    let mut spool_done = false;
    let mut logs_drained = false;
    let deadline = async {
        match timeout_secs {
            Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
            None => std::future::pending::<()>().await,
        }
    };
    let completion = async {
        let wait = async {
            let (success, status) = observe_exit(&mut child, job.pid).await?;
            successful_exit = success;
            exit = status;
            Ok::<_, String>(())
        };
        let drain = async {
            let result = (&mut spool).await;
            spool_done = true;
            result.map_err(|e| format!("output task: {e}"))??;
            logs_drained = true;
            Ok::<_, String>(())
        };
        tokio::try_join!(wait, drain).map(|_| ())
    };
    let (mut state, mut error) = tokio::select! {
        result = completion => match result {
            Ok(()) if successful_exit => ("completed", None),
            Ok(()) => ("failed", Some("process exited unsuccessfully".into())),
            Err(error) => ("failed", Some(error)),
        },
        _ = async {
            loop {
                if *cancel.borrow_and_update() { break; }
                if cancel.changed().await.is_err() { break; }
            }
        } => ("cancelled", None),
        _ = deadline => ("timed_out", Some(format!("timeout after {}s", timeout_secs.unwrap_or(0)))),
    };
    let mut cleanup = CleanupReport::default();
    if state != "completed" {
        cleanup = job.ownership.cleanup();
        if let Some(detail) = &cleanup.error {
            error = Some(format!("{}; cleanup: {detail}", error.unwrap_or_default()));
        }
        if exit.is_none() {
            let _ = child.start_kill();
        }
        if !spool_done {
            match tokio::time::timeout(Duration::from_secs(2), &mut spool).await {
                Ok(Ok(Ok(()))) => logs_drained = true,
                Ok(Ok(Err(e))) => error = Some(format!("{}; {e}", error.unwrap_or_default())),
                Ok(Err(e)) => {
                    error = Some(format!("{}; output task: {e}", error.unwrap_or_default()))
                }
                Err(_) => {
                    spool.abort();
                    let _ = spool.await;
                    error = Some(format!(
                        "{}; output drain incomplete after process-group cleanup",
                        error.unwrap_or_default()
                    ));
                }
            }
        }
    }
    // The unreaped Unix leader anchors the session through cleanup and pipe
    // drain. Disarm every potential signaler before releasing that identity.
    guard.0.disarm();
    if exit.is_none() {
        match child.wait().await {
            Ok(status) => exit = Some(status),
            Err(error_wait) => {
                state = "failed";
                error = Some(format!(
                    "{}; reap process: {error_wait}",
                    error.unwrap_or_default()
                ));
            }
        }
    }
    if error.as_deref() == Some("process exited unsuccessfully") {
        error = Some(format!(
            "process exited unsuccessfully: {}",
            exit.as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unknown status".into())
        ));
    }
    // Dropping stdin must not wait behind a client whose pipe write is blocked.
    // Killing/reaping closes the other end; the pending write then unblocks.
    job.input_open.store(false, Ordering::Release);
    if let Ok(mut input) = job.input.try_lock() {
        *input = None;
    }
    job.running.store(false, Ordering::Release);
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        exit.as_ref().and_then(|status| status.signal())
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    job.state.send_modify(|result| {
        result["state"] = json!(state);
        result["exit_code"] = json!(exit.as_ref().map(|status| status.code().unwrap_or(-1)));
        result["signal"] = json!(signal);
        result["error"] = json!(error);
        result["timeout_secs"] = json!(timeout_secs);
        result["cleanup"] = json!({"process_group_signaled": cleanup.group_signaled,
            "session_id": if cfg!(unix) {Some(job.pid)} else {None},
            "session_processes_signaled": cleanup.processes_signaled,
            "session_scan_complete": cleanup.session_scan_complete,
            "session_identity_anchor": if cfg!(unix) {"unreaped_leader_until_cleanup_disarmed"} else {"direct_child_handle"},
            "child_reaped": exit.is_some(), "logs_drained": logs_drained,
            "scope": if cfg!(any(target_os = "macos", target_os = "linux")) {"job_session_including_nested_process_groups"} else if cfg!(unix) {"verified_original_process_group"} else {"direct_child"},
            "detached_new_sessions_included": false});
    });
}

fn spawn(command: &mut Command, pty: bool) -> std::io::Result<(Child, Writer, Reader, Reader)> {
    if pty {
        #[cfg(unix)]
        {
            return terminal::spawn(command);
        }
        #[cfg(not(unix))]
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "PTY is only supported on Unix; set pty=false for pipes",
            ));
        }
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let input = Box::new(child.stdin.take().expect("piped stdin"));
    let output = Box::new(child.stdout.take().expect("piped stdout"));
    let errors = Box::new(child.stderr.take().expect("piped stderr"));
    Ok((child, input, output, errors))
}

#[cfg(unix)]
mod terminal {
    use super::*;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::pin::Pin;
    use std::task::{ready, Context, Poll};
    use tokio::io::unix::AsyncFd;
    use tokio::io::ReadBuf;

    struct Master(Arc<AsyncFd<File>>);

    impl AsyncRead for Master {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            loop {
                let mut guard = ready!(self.0.poll_read_ready(cx))?;
                match guard.try_io(|fd| {
                    let bytes = buf.initialize_unfilled();
                    let count = unsafe {
                        libc::read(
                            fd.get_ref().as_raw_fd(),
                            bytes.as_mut_ptr().cast(),
                            bytes.len(),
                        )
                    };
                    if count < 0 {
                        let error = io::Error::last_os_error();
                        // Linux PTYs use EIO after the last slave descriptor closes.
                        if error.raw_os_error() == Some(libc::EIO) {
                            return Ok(0);
                        }
                        return Err(error);
                    }
                    Ok(count as usize)
                }) {
                    Ok(Ok(count)) => {
                        buf.advance(count);
                        return Poll::Ready(Ok(()));
                    }
                    Ok(Err(error)) => return Poll::Ready(Err(error)),
                    Err(_) => continue,
                }
            }
        }
    }

    impl AsyncWrite for Master {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            loop {
                let mut guard = ready!(self.0.poll_write_ready(cx))?;
                match guard.try_io(|fd| {
                    let count = unsafe {
                        libc::write(fd.get_ref().as_raw_fd(), buf.as_ptr().cast(), buf.len())
                    };
                    if count < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(count as usize)
                    }
                }) {
                    Ok(result) => return Poll::Ready(result),
                    Err(_) => continue,
                }
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Writer, Reader, Reader)> {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = Arc::new(AsyncFd::new(master)?);
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        // Command keeps its configured slave Stdio values until overwritten;
        // release them now so terminal EOF follows child/descendant exit.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok((
            child,
            Box::new(Master(master.clone())),
            Box::new(Master(master)),
            Box::new(tokio::io::empty()),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell(source: &str) -> ExecutionRequest {
        ExecutionRequest {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), source.into()],
            cwd: std::env::temp_dir(),
            env: BTreeMap::new(),
            stdin: None,
            timeout_secs: Some(10),
            pty: false,
        }
    }
    fn id(start: &Value) -> &str {
        start["job_id"].as_str().unwrap()
    }

    #[tokio::test]
    async fn concurrent_jobs_keep_input_and_output_independent() {
        let jobs = JobManager::new();
        let first = jobs
            .start(shell("read line; printf 'first:%s' \"$line\""))
            .await
            .unwrap();
        let second = jobs.start(shell("printf second")).await.unwrap();
        let done = tokio::time::timeout(Duration::from_secs(2), jobs.wait(id(&second)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done["exit_code"], 0);
        assert_eq!(jobs.status(id(&first)).await.unwrap()["state"], "running");
        jobs.write(id(&first), "hello\n", true).await.unwrap();
        jobs.wait(id(&first)).await.unwrap();
        assert_eq!(
            jobs.read(id(&first), "stdout", 0, 100).await.unwrap()["text"],
            "first:hello"
        );
        assert_eq!(
            jobs.read(id(&second), "stdout", 0, 100).await.unwrap()["text"],
            "second"
        );
    }

    #[tokio::test]
    async fn output_larger_than_preview_is_retained_and_seekable() {
        let jobs = JobManager::new();
        let start = jobs
            .start(shell(
                "head -c 400000 /dev/zero; printf final-marker; printf err >&2",
            ))
            .await
            .unwrap();
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["state"], "completed");
        assert_eq!(done["stdout"]["bytes"], 400012);
        let first = jobs
            .read(id(&start), "stdout", 0, usize::MAX)
            .await
            .unwrap();
        assert_eq!(first["bytes_read"], MAX_READ_BYTES);
        assert_eq!(first["eof"], false);
        assert_eq!(
            jobs.read(id(&start), "stdout", 400000, 100).await.unwrap()["text"],
            "final-marker"
        );
        assert_eq!(
            jobs.read(id(&start), "stderr", 0, 100).await.unwrap()["text"],
            "err"
        );
    }

    #[tokio::test]
    async fn pipe_eof_and_initial_input_are_ordered() {
        let jobs = JobManager::new();
        let mut request = shell("cat");
        request.stdin = Some("seed-".repeat(20000));
        let start = jobs.start(request).await.unwrap();
        jobs.write(id(&start), "tail", true).await.unwrap();
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["stdout"]["bytes"], 100004);
        assert_eq!(
            jobs.read(id(&start), "stdout", 100000, 4).await.unwrap()["text"],
            "tail"
        );
    }

    #[tokio::test]
    async fn timeout_covers_descendant_pipe_drain() {
        let jobs = JobManager::new();
        let marker = jobs.inner.directory.join("descendant-survived");
        create_private_directory(&jobs.inner.directory).unwrap();
        let mut request = shell("(sleep 2; printf survived > \"$MARKER\") & exit 0");
        request
            .env
            .insert("MARKER".into(), marker.to_string_lossy().into_owned());
        request.timeout_secs = Some(1);
        let start = jobs.start(request).await.unwrap();
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["state"], "timed_out");
        assert_eq!(done["cleanup"]["child_reaped"], true);
        assert_eq!(done["cleanup"]["logs_drained"], true);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(!marker.exists(), "descendant survived timeout");
    }

    #[tokio::test]
    async fn cancel_reaps_before_responding_and_unknown_sessions_reject_ids() {
        let jobs = JobManager::new();
        let start = jobs.start(shell("sleep 30")).await.unwrap();
        let done = jobs.cancel(id(&start)).await.unwrap();
        assert_eq!(done["state"], "cancelled");
        assert_eq!(done["cleanup"]["child_reaped"], true);
        assert_eq!(done["cleanup"]["logs_drained"], true);
        let other = JobManager::new();
        let other_start = other.start(shell("printf other-session")).await.unwrap();
        assert_ne!(id(&start), id(&other_start));
        assert!(other.status(id(&start)).await.is_err());
        other.wait(id(&other_start)).await.unwrap();
        assert_eq!(
            other.list().await.unwrap()["jobs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn last_manager_drop_kills_owned_processes() {
        let jobs = JobManager::new();
        let marker = jobs.inner.directory.join("drop-survived");
        create_private_directory(&jobs.inner.directory).unwrap();
        let mut request = shell("sleep 1; printf survived > \"$MARKER\"");
        request
            .env
            .insert("MARKER".into(), marker.to_string_lossy().into_owned());
        let start = jobs.start(request).await.unwrap();
        let job = jobs.job(id(&start)).unwrap();
        drop(jobs);
        let mut status = job.state.subscribe();
        tokio::time::timeout(Duration::from_secs(3), async {
            while status.borrow_and_update()["state"] == "running" {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(status.borrow()["cleanup"]["child_reaped"], true);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn pty_is_a_real_controlling_terminal_and_accepts_input() {
        let jobs = JobManager::new();
        let mut request = shell("test -t 0 && test -t 1 && test -t 2 || exit 9; read line; printf 'tty:%s' \"$line\"; printf ':stderr' >&2");
        request.pty = true;
        let start = jobs.start(request).await.unwrap();
        jobs.write(id(&start), "hello\n", false).await.unwrap();
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["exit_code"], 0);
        assert_eq!(done["stderr"]["merged_into"], "stdout");
        let text = jobs.read(id(&start), "stdout", 0, 1024).await.unwrap()["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("tty:hello:stderr"), "{text}");
    }

    #[tokio::test]
    async fn pty_canonical_eof_ends_a_reader_without_killing_it() {
        let jobs = JobManager::new();
        let mut request = shell("cat; printf EOF-observed");
        request.pty = true;
        let start = jobs.start(request).await.unwrap();
        let result = jobs
            .write(id(&start), "terminal-input\n", true)
            .await
            .unwrap();
        assert_eq!(result["eof"], "terminal_veof_sent");
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["state"], "completed");
        assert_eq!(done["exit_code"], 0);
        assert_eq!(done["cleanup"]["process_group_signaled"], false);
        let output = jobs.read(id(&start), "stdout", 0, 1024).await.unwrap();
        assert!(output["text"].as_str().unwrap().contains("EOF-observed"));
    }

    #[tokio::test]
    async fn binary_pages_round_trip_even_when_utf8_characters_are_split() {
        use base64::Engine;
        let jobs = JobManager::new();
        let start = jobs
            .start(shell(r"printf '\000\377\303\251\360\237\246\200'"))
            .await
            .unwrap();
        jobs.wait(id(&start)).await.unwrap();
        let mut offset = 0;
        let mut rebuilt = Vec::new();
        loop {
            let page = jobs
                .read_encoded(id(&start), "stdout", offset, 3, "base64")
                .await
                .unwrap();
            assert!(page.get("text").is_none());
            rebuilt.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(page["data"].as_str().unwrap())
                    .unwrap(),
            );
            offset = page["next_offset"].as_u64().unwrap();
            if page["eof"] == true {
                break;
            }
        }
        assert_eq!(rebuilt, [0, 255, 195, 169, 240, 159, 166, 128]);
        assert!(jobs
            .read_encoded(id(&start), "stdout", 0, 10, "unknown")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn stdin_deadline_reports_partial_transport_writes_and_keeps_job_cancellable() {
        let jobs = JobManager::new();
        let start = jobs.start(shell("sleep 30")).await.unwrap();
        let error = jobs
            .write_with_timeout(id(&start), &"x".repeat(1024 * 1024), false, Some(1))
            .await
            .unwrap_err();
        assert!(error.contains("timed out after 1s"), "{error}");
        assert!(error.contains("of 1048576 input bytes accepted"), "{error}");
        assert!(!error.contains("; 1048576 of 1048576"), "{error}");
        assert_eq!(jobs.status(id(&start)).await.unwrap()["state"], "running");
        assert_eq!(
            jobs.cancel(id(&start)).await.unwrap()["cleanup"]["child_reaped"],
            true
        );
    }

    #[tokio::test]
    async fn stdin_deadline_includes_waiting_behind_seeded_input() {
        let jobs = JobManager::new();
        let mut request = shell("sleep 30");
        request.stdin = Some("x".repeat(1024 * 1024));
        let start = jobs.start(request).await.unwrap();
        let error = jobs
            .write_with_timeout(id(&start), "tail", true, Some(1))
            .await
            .unwrap_err();
        assert!(error.contains("0 of 4 input bytes accepted"), "{error}");
        assert_eq!(
            jobs.cancel(id(&start)).await.unwrap()["cleanup"]["child_reaped"],
            true
        );
    }

    #[tokio::test]
    async fn undelivered_start_handoff_cancels_and_reaps_process() {
        let jobs = JobManager::new();
        let start = jobs.start(shell("sleep 30")).await.unwrap();
        // Simulate dropping the handoff while response delivery is pending;
        // exercise the real process cleanup, not merely the watch signal.
        let handoff = StartHandoff(Some(jobs.job(id(&start)).unwrap()));
        drop(handoff);
        let done = tokio::time::timeout(Duration::from_secs(3), jobs.wait(id(&start)))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(done["state"], "running");
        assert_eq!(done["cleanup"]["child_reaped"], true);
        assert_eq!(done["cleanup"]["logs_drained"], true);
    }

    #[tokio::test]
    async fn nonzero_exit_is_a_failed_job_with_exact_exit_code() {
        let jobs = JobManager::new();
        let start = jobs
            .start(shell("printf rejected >&2; exit 17"))
            .await
            .unwrap();
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["state"], "failed");
        assert_eq!(done["exit_code"], 17);
        assert_eq!(
            jobs.read(id(&start), "stderr", 0, 100).await.unwrap()["text"],
            "rejected"
        );
    }

    #[tokio::test]
    async fn timeout_kills_a_nested_group_after_original_leader_exit() {
        let jobs = JobManager::new();
        let marker = jobs.inner.directory.join("nested-group-survived");
        create_private_directory(&jobs.inner.directory).unwrap();
        let mut request = shell("");
        request.program = which::which("python3")
            .expect("python3 is required for the nested backend group regression");
        request.args = vec!["-c".into(),
            "import os,subprocess,sys; subprocess.Popen([sys.executable,'-c',\"import os,time; from pathlib import Path; time.sleep(2); Path(os.environ['MARKER']).write_text('survived')\"],preexec_fn=os.setpgrp)".into()];
        request
            .env
            .insert("MARKER".into(), marker.to_string_lossy().into_owned());
        request.timeout_secs = Some(1);
        let start = jobs.start(request).await.unwrap();
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["state"], "timed_out");
        assert_eq!(
            done["exit_code"], 0,
            "the original leader should already have exited"
        );
        assert_eq!(done["cleanup"]["session_scan_complete"], true);
        assert!(
            done["cleanup"]["session_processes_signaled"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert_eq!(done["cleanup"]["logs_drained"], true);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            !marker.exists(),
            "reparented nested process group survived timeout"
        );
    }

    #[tokio::test]
    async fn cancelling_one_session_preserves_other_jobs_and_shutdown_reaps_all() {
        let jobs = JobManager::new();
        let first = jobs.start(shell("sleep 30")).await.unwrap();
        let second = jobs
            .start(shell("read line; printf 'independent:%s' \"$line\""))
            .await
            .unwrap();
        let third = jobs.start(shell("sleep 30")).await.unwrap();
        jobs.cancel(id(&first)).await.unwrap();
        jobs.write(id(&second), "alive\n", true).await.unwrap();
        assert_eq!(jobs.wait(id(&second)).await.unwrap()["exit_code"], 0);
        assert_eq!(
            jobs.read(id(&second), "stdout", 0, 100).await.unwrap()["text"],
            "independent:alive"
        );
        jobs.shutdown().await.unwrap();
        assert_eq!(jobs.status(id(&third)).await.unwrap()["state"], "cancelled");
        assert_eq!(
            jobs.status(id(&third)).await.unwrap()["cleanup"]["child_reaped"],
            true
        );
    }

    #[tokio::test]
    async fn exited_leader_remains_unreaped_while_a_detached_child_holds_output() {
        let jobs = JobManager::new();
        let mut request = shell("");
        request.program =
            which::which("python3").expect("python3 is required for session ownership regression");
        request.args = vec!["-c".into(),
            "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time; time.sleep(1); print(\"detached-finished\")'],start_new_session=True)".into()];
        let start = jobs.start(request).await.unwrap();
        let pid = start["pid"].as_u64().unwrap() as libc::id_t;
        // WNOWAIT proves this exact leader still belongs to us and its numeric
        // PID/SID cannot be recycled while a separate session retains the pipe.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid,
                        &mut information,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                assert_eq!(
                    result,
                    0,
                    "leader was reaped before output drain: {}",
                    std::io::Error::last_os_error()
                );
                if unsafe { information.si_pid() } != 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(jobs.status(id(&start)).await.unwrap()["state"], "running");
        let done = jobs.wait(id(&start)).await.unwrap();
        assert_eq!(done["state"], "completed");
        assert_eq!(done["cleanup"]["child_reaped"], true);
        assert_eq!(
            done["cleanup"]["session_identity_anchor"],
            "unreaped_leader_until_cleanup_disarmed"
        );
        let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut information,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
