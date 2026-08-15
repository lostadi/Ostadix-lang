use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use num_bigint::BigInt;
use serde_json::Value;

#[path = "backend_state.rs"]
pub mod state;

use self::state::{
    BackendCheckpointV1, BackendRestoreReceiptV1, BackendStateCapabilitiesV1, BackendStateErrorV1,
    BackendStateReasonV1, BackendStateTierV1, BackendWireCommandV2, BackendWireResponseV2,
    SQL_CLI_CODEC_V1,
};
use crate::ir::{BackendAdapterKind, BackendRegistry};
use crate::runtime_exec::BackendToolchain;
use crate::value::{FloatFormat, ONumber, OValue};
use crate::wire;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
const NATIVE_BACKEND_HANDLERS: &[&str] = &[
    "bash",
    "shell",
    "javascript",
    "ruby",
    "rust",
    "c",
    "cpp",
    "java",
    "nix",
    "nix_expr",
    "nix_store",
    "sql",
    "haskell",
    "ocaml",
    "racket",
    "lisp",
    "common_lisp",
    "csharp",
    "matlab",
    "mathematica",
    "webassembly",
];

pub fn run_backend_from_env_args() -> Result<bool> {
    let mut args = std::env::args();
    let _program = args.next();
    if args.next().as_deref() != Some("--o-backend") {
        return Ok(false);
    }

    let lang = args
        .next()
        .context("--o-backend requires a language name")?;
    if let Some(extra) = args.next() {
        bail!("unexpected argument after --o-backend {lang}: {extra}");
    }

    run_backend(&lang)?;
    Ok(true)
}

pub fn has_native_backend(lang: &str) -> bool {
    BackendRegistry::global().adapter_for(lang) == BackendAdapterKind::NativeRust
}

pub fn run_backend(lang: &str) -> Result<()> {
    let tools = BackendToolchain::from_env(lang)?;
    tools.verify_all()?;
    if !has_native_backend(lang) {
        return proxy_legacy_backend(lang, &tools);
    }

    let mut backend = RustBackend::new(tools);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    while let Some(command) = wire::read_frame::<_, BackendWireCommandV2>(&mut reader)? {
        if matches!(&command, BackendWireCommandV2::Shutdown) {
            match backend.shutdown() {
                Ok(()) => {
                    wire::write_frame(&mut writer, &BackendWireResponseV2::ok(OValue::Null))?;
                    crate::process::lifecycle_trace(
                        "backend.shutdown_acknowledged",
                        format!("language={lang}"),
                    );
                    break;
                }
                Err(error) => {
                    wire::write_frame(
                        &mut writer,
                        &BackendWireResponseV2::err(format!("backend shutdown failed: {error:#}")),
                    )?;
                    return Err(error).context("backend shutdown failed");
                }
            }
        }
        let response = match command {
            BackendWireCommandV2::Exec { code, bindings } => {
                match backend.exec(lang, &code, bindings) {
                    Ok(value) => BackendWireResponseV2::ok(value),
                    Err(error) => BackendWireResponseV2::err(format!("{error:#}")),
                }
            }
            BackendWireCommandV2::Cleanup => match backend.cleanup() {
                Ok(()) => BackendWireResponseV2::ok(OValue::Null),
                Err(error) => BackendWireResponseV2::err(format!("{error:#}")),
            },
            BackendWireCommandV2::Shutdown => unreachable!("shutdown handled before dispatch"),
            BackendWireCommandV2::Ping => BackendWireResponseV2::ok(OValue::Null),
            BackendWireCommandV2::EvalResult { .. } => BackendWireResponseV2::err(
                "backend received eval_result without a pending eval request",
            ),
            BackendWireCommandV2::StateCapabilitiesV1 => {
                BackendWireResponseV2::StateCapabilitiesV1 {
                    capabilities: backend.state_capabilities(lang),
                }
            }
            BackendWireCommandV2::CheckpointV1 { max_bytes } => {
                backend.checkpoint_response(lang, max_bytes)
            }
            BackendWireCommandV2::RestoreV1 { checkpoint } => {
                backend.restore_response(lang, checkpoint)
            }
        };
        wire::write_frame(&mut writer, &response)?;
    }

    Ok(())
}

struct RustBackend {
    sql: Option<SqlState>,
    tools: BackendToolchain,
}

/// Persistent `sqlite3` CLI session.
///
/// Earlier this backend spawned a fresh `sqlite3 -json <db> <sql>` process per
/// block. That correctly preserved `CREATE TABLE` (file-backed) but dropped
/// connection-local state such as `ATTACH DATABASE … AS alias` between blocks.
/// Keep one interactive session for the lifetime of the backend process so
/// multi-block `sql[0]^(…)_sql[0]` programs match the Python shim semantics.
struct SqlState {
    _dir: TempDir,
    db_path: PathBuf,
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: BufReader<ChildStdout>,
    stderr_rx: Receiver<String>,
    checkpoint_safe: bool,
}

impl RustBackend {
    fn new(tools: BackendToolchain) -> Self {
        Self { sql: None, tools }
    }

    fn exec(
        &mut self,
        lang: &str,
        code: &str,
        bindings: HashMap<String, OValue>,
    ) -> Result<OValue> {
        match lang {
            "bash" => run_shell(
                &self.tools,
                "bash",
                &["-c", code],
                Some(scalar_env(bindings)),
            ),
            "shell" => run_shell(&self.tools, "sh", &["-c", code], Some(scalar_env(bindings))),
            "javascript" => run_script(
                &self.tools,
                "javascript",
                "node",
                "js",
                &javascript_preamble(&bindings),
                code,
            ),
            "ruby" => run_script(
                &self.tools,
                "ruby",
                "ruby",
                "rb",
                &ruby_preamble(&bindings),
                code,
            ),
            "rust" => run_rust(&self.tools, code),
            "c" => run_c(&self.tools, code),
            "cpp" => run_cpp(&self.tools, code),
            "java" => run_java(&self.tools, code),
            "nix" | "nix_expr" => run_nix(&self.tools, code),
            "nix_store" => run_nix_store(&self.tools, code),
            "sql" => self.run_sql(code),
            "haskell" => run_haskell(&self.tools, code),
            "ocaml" => run_ocaml(&self.tools, code),
            "racket" => run_file_command(&self.tools, "racket", "racket", "rkt", code, &["{file}"]),
            "lisp" | "common_lisp" => run_common_lisp(&self.tools, code),
            "csharp" => run_csharp(&self.tools, code),
            "matlab" => run_matlab(&self.tools, code),
            "mathematica" => run_mathematica(&self.tools, code),
            "webassembly" => run_webassembly(&self.tools, code),
            other => bail!("backend `{other}` is not implemented by the Rust backend runner"),
        }
    }

    fn run_sql(&mut self, code: &str) -> Result<OValue> {
        let code = code.trim();
        if code.is_empty() {
            return Ok(OValue::Null);
        }

        let state = self.sql_state()?;
        state.exec(code)
    }

    fn sql_state(&mut self) -> Result<&mut SqlState> {
        if self.sql.is_none() {
            self.sql = Some(SqlState::spawn(&self.tools)?);
        }
        Ok(self.sql.as_mut().expect("sql state was just initialized"))
    }

    fn cleanup(&mut self) -> Result<()> {
        if let Some(mut sql) = self.sql.take() {
            sql.shutdown(crate::process::backend_shutdown_timeout())?;
        }
        Ok(())
    }

    fn state_capabilities(&self, lang: &str) -> BackendStateCapabilitiesV1 {
        if lang == "sql" {
            BackendStateCapabilitiesV1::new(
                lang,
                BackendStateTierV1::SemanticSnapshot,
                SQL_CLI_CODEC_V1,
                true,
            )
        } else {
            state::empty_state_capabilities(lang)
        }
    }

    fn checkpoint_response(&mut self, lang: &str, max_bytes: u64) -> BackendWireResponseV2 {
        let checkpoint = if lang == "sql" {
            self.sql_checkpoint()
        } else {
            state::empty_checkpoint(lang, self.tools.executable_set_sha256())
        };
        match checkpoint {
            Ok(checkpoint) => match state::ensure_checkpoint_bound(&checkpoint, max_bytes) {
                Ok(()) => BackendWireResponseV2::CheckpointV1 { checkpoint },
                Err(error) => BackendWireResponseV2::StateErrorV1 {
                    error: BackendStateErrorV1::new(
                        lang,
                        "state.checkpoint-too-large",
                        format!("{error:#}"),
                    ),
                },
            },
            Err(error) => {
                if let Some(pin) = error.downcast_ref::<BackendStatePinRequired>() {
                    BackendWireResponseV2::StatePinRequiredV1 {
                        reason: BackendStateReasonV1::pin_required(
                            lang,
                            pin.path.clone(),
                            pin.message.clone(),
                        ),
                    }
                } else {
                    BackendWireResponseV2::StateErrorV1 {
                        error: BackendStateErrorV1::new(
                            lang,
                            "state.checkpoint-failed",
                            format!("{error:#}"),
                        ),
                    }
                }
            }
        }
    }

    fn restore_response(
        &mut self,
        lang: &str,
        checkpoint: BackendCheckpointV1,
    ) -> BackendWireResponseV2 {
        let result = if lang == "sql" {
            self.restore_sql(&checkpoint)
        } else {
            state::validate_empty_restore(lang, self.tools.executable_set_sha256(), &checkpoint)
        };
        match result.and_then(|()| BackendRestoreReceiptV1::restored(lang, &checkpoint)) {
            Ok(receipt) => BackendWireResponseV2::RestoreV1 { receipt },
            Err(error) => BackendWireResponseV2::StateErrorV1 {
                error: BackendStateErrorV1::new(
                    lang,
                    "state.restore-incompatible",
                    format!("{error:#}"),
                ),
            },
        }
    }

    fn sql_checkpoint(&mut self) -> Result<BackendCheckpointV1> {
        let runtime_binding = self.tools.executable_set_sha256().to_string();
        let sql = self.sql_state()?;
        if !sql.checkpoint_safe {
            return Err(anyhow::Error::new(BackendStatePinRequired {
                path: "$sql.connection".to_string(),
                message: "SQL history used transaction-, attachment-, TEMP-, PRAGMA-, extension-, or connection-local state outside the constrained main-database codec"
                    .to_string(),
            }));
        }
        let database = fs::read(&sql.db_path).with_context(|| {
            format!(
                "failed to read SQL state database {}",
                sql.db_path.display()
            )
        })?;
        BackendCheckpointV1::new(
            "sql",
            BackendStateTierV1::SemanticSnapshot,
            SQL_CLI_CODEC_V1,
            runtime_binding,
            serde_json::json!({
                "profile": "autocommit-main-only",
                "database_b64": BASE64_STANDARD.encode(database),
            }),
            Vec::new(),
        )
    }

    fn restore_sql(&mut self, checkpoint: &BackendCheckpointV1) -> Result<()> {
        checkpoint.validate()?;
        if self.sql.is_some() {
            bail!("state.restore-conflict: SQL actor already owns an open session");
        }
        if checkpoint.backend != "sql"
            || checkpoint.tier != BackendStateTierV1::SemanticSnapshot
            || checkpoint.codec != SQL_CLI_CODEC_V1
            || checkpoint.runtime_binding_sha256 != self.tools.executable_set_sha256()
            || !checkpoint.external_resources.is_empty()
        {
            bail!("SQL checkpoint is incompatible with this backend implementation");
        }
        let object = checkpoint
            .payload
            .as_object()
            .context("SQL checkpoint payload is not an object")?;
        if object.get("profile").and_then(Value::as_str) != Some("autocommit-main-only") {
            bail!("SQL checkpoint has an unsupported connection profile");
        }
        let encoded = object
            .get("database_b64")
            .and_then(Value::as_str)
            .context("SQL checkpoint omitted database_b64")?;
        let database = BASE64_STANDARD
            .decode(encoded)
            .context("SQL checkpoint database is not valid base64")?;
        let mut replacement = SqlState::spawn_with_database(&self.tools, Some(&database))?;
        let integrity = replacement.exec_untracked("PRAGMA integrity_check;")?;
        match integrity {
            OValue::Text { v } if v.utf8 == "ok" => {}
            other => bail!("SQL checkpoint failed integrity_check: {other}"),
        }
        self.sql = Some(replacement);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        self.cleanup()
    }
}

#[derive(Debug)]
struct BackendStatePinRequired {
    path: String,
    message: String,
}

impl std::fmt::Display for BackendStatePinRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for BackendStatePinRequired {}

impl SqlState {
    fn spawn(tools: &BackendToolchain) -> Result<Self> {
        Self::spawn_with_database(tools, None)
    }

    fn spawn_with_database(tools: &BackendToolchain, database: Option<&[u8]>) -> Result<Self> {
        let dir = TempDir::new("o-backend-sql")?;
        let db_path = dir.path().join("state.sqlite3");
        match database {
            Some(database) => fs::write(&db_path, database)
                .with_context(|| format!("failed to restore sql state db {}", db_path.display()))?,
            None => {
                // Ensure the file exists so sqlite3 opens a durable on-disk DB.
                fs::File::create(&db_path).with_context(|| {
                    format!("failed to create sql state db {}", db_path.display())
                })?;
            }
        }

        let mut command = tools.command("sqlite3")?;
        let mut child = command
            .arg("-batch")
            .arg(&db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to launch admitted sqlite3 executable")?;

        let stdin = BufWriter::new(
            child
                .stdin
                .take()
                .context("sqlite3 session did not provide stdin")?,
        );
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .context("sqlite3 session did not provide stdout")?,
        );
        let stderr = BufReader::new(
            child
                .stderr
                .take()
                .context("sqlite3 session did not provide stderr")?,
        );

        let (stderr_tx, stderr_rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        thread::spawn(move || {
            let mut stderr = stderr;
            let mut line = String::new();
            while stderr.read_line(&mut line).ok().is_some_and(|n| n > 0) {
                let _ = stderr_tx.send(std::mem::take(&mut line));
            }
        });

        let mut state = Self {
            _dir: dir,
            db_path,
            child,
            stdin: Some(stdin),
            stdout,
            stderr_rx,
            checkpoint_safe: true,
        };
        // JSON row output for SELECT/WITH/PRAGMA, matching the old -json flag.
        state.write_raw(".mode json\n")?;
        // Drain any mode-switch chatter (usually none).
        let _ = state.drain_stderr();
        Ok(state)
    }

    fn write_raw(&mut self, text: &str) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("sqlite3 session command pipe is closed"))?;
        stdin
            .write_all(text.as_bytes())
            .context("failed to write to sqlite3 session")?;
        stdin.flush().context("failed to flush sqlite3 session")?;
        Ok(())
    }

    fn shutdown(&mut self, timeout: Duration) -> Result<()> {
        self.stdin.take();
        let status = reap_legacy_child(&mut self.child, timeout)?;
        if status.success() {
            Ok(())
        } else {
            bail!("sqlite3 session exited with status {status} during shutdown")
        }
    }

    fn drain_stderr(&self) -> String {
        // Give the stderr collector a brief window after stdout completes.
        thread::sleep(Duration::from_millis(5));
        let mut acc = String::new();
        while let Ok(chunk) = self.stderr_rx.try_recv() {
            acc.push_str(&chunk);
        }
        acc
    }

    fn exec(&mut self, code: &str) -> Result<OValue> {
        if !sql_checkpoint_profile_accepts(code) {
            // This is a monotone downgrade for the current actor generation.
            // Later SQL cannot prove that an attachment, open transaction, or
            // connection-local mutation was completely undone.
            self.checkpoint_safe = false;
        }
        self.exec_untracked(code)
    }

    fn exec_untracked(&mut self, code: &str) -> Result<OValue> {
        // Clear stale stderr from prior statements.
        let _ = self.drain_stderr();

        // Feed the full block, then a sentinel print so we can delimit replies
        // without closing the session (which would lose ATTACH state).
        let mut payload = code.to_string();
        if !payload.trim_end().ends_with(';') {
            payload.push(';');
        }
        payload.push('\n');
        payload.push_str(".print __O_SQL_DONE__\n");
        self.write_raw(&payload)?;

        let mut out = String::new();
        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .context("failed to read sqlite3 session stdout")?;
            if n == 0 {
                let err = self.drain_stderr();
                bail!(
                    "sqlite3 session closed unexpectedly{}",
                    if err.is_empty() {
                        String::new()
                    } else {
                        format!(": {err}")
                    }
                );
            }
            if line.trim_end_matches(['\r', '\n']) == "__O_SQL_DONE__" {
                break;
            }
            out.push_str(&line);
        }

        let err = self.drain_stderr();
        if sql_stderr_is_error(&err) {
            // Preserve the historical error envelope from the one-shot CLI path
            // so existing triage / test string matches keep working.
            bail!("sqlite3 execution failed (code 1)\n{}", err.trim_end());
        }

        let trimmed = out.trim();
        if trimmed.is_empty() {
            if sql_has_query_result(code) {
                return Ok(OValue::list(Vec::new()));
            }
            return Ok(OValue::str_("Statement executed successfully"));
        }

        let json: Value = serde_json::from_str(trimmed).context("sqlite3 returned non-JSON")?;
        sqlite_json_to_ovalue(json)
    }
}

/// Conservative profile accepted by the first SQL semantic codec.
///
/// False positives only pin a session. Missing a stateful construct would lose
/// state, so unfamiliar connection control is rejected by broad token checks.
fn sql_checkpoint_profile_accepts(code: &str) -> bool {
    let lower = code.to_ascii_lowercase();
    const PINNING_TOKENS: &[&str] = &[
        "attach",
        "detach",
        "begin",
        "commit",
        "rollback",
        "savepoint",
        "release",
        "pragma",
        " temp ",
        "temporary",
        "load_extension",
        ".load",
        ".open",
        ".restore",
        ".backup",
        "create virtual table",
        "last_insert_rowid",
        "changes(",
        "total_changes(",
        "random(",
        "randomblob(",
    ];
    !PINNING_TOKENS.iter().any(|token| lower.contains(token))
        && !lower.trim_start().starts_with('.')
        && !lower.contains("\n.")
}

impl Drop for SqlState {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = reap_legacy_child(&mut self.child, Duration::from_millis(250));
    }
}

fn sql_stderr_is_error(err: &str) -> bool {
    let trimmed = err.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("parse error")
        || lower.contains("incomplete sql")
        || lower.starts_with("usage:")
}

fn proxy_legacy_backend(lang: &str, tools: &BackendToolchain) -> Result<()> {
    let shim = std::env::var_os("O_BACKEND_LEGACY_SHIM")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("backend `{lang}` has no Rust adapter and no legacy shim path"))?;
    if !shim.exists() {
        bail!(
            "backend `{lang}` has no Rust adapter and legacy shim does not exist: {}",
            shim.display()
        );
    }

    let mut command = tools.command("python3")?;
    let mut child = command
        .arg(&shim)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch admitted Python executable for legacy backend shim: {}",
                shim.display()
            )
        })?;
    crate::process::lifecycle_trace(
        "proxy.shim_spawned",
        format!("language={lang} shim_pid={}", child.id()),
    );

    let child_stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let cleanup = reap_legacy_child(&mut child, Duration::from_millis(250));
            return match cleanup {
                Ok(_) => Err(anyhow!("legacy backend did not provide stdin")),
                Err(cleanup) => Err(anyhow!(
                    "legacy backend did not provide stdin; cleanup also failed: {cleanup:#}"
                )),
            };
        }
    };
    let child_stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            drop(child_stdin);
            let cleanup = reap_legacy_child(&mut child, Duration::from_millis(250));
            return match cleanup {
                Ok(_) => Err(anyhow!("legacy backend did not provide stdout")),
                Err(cleanup) => Err(anyhow!(
                    "legacy backend did not provide stdout; cleanup also failed: {cleanup:#}"
                )),
            };
        }
    };
    let mut child_stdin = Some(BufWriter::new(child_stdin));
    let mut child_stdout = BufReader::new(child_stdout);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    while let Some(command) = wire::read_frame::<_, BackendWireCommandV2>(&mut reader)? {
        if matches!(&command, BackendWireCommandV2::Shutdown) {
            crate::process::lifecycle_trace(
                "proxy.shutdown_received",
                format!("language={lang} shim_pid={}", child.id()),
            );
            // Legacy shims already exit on command-channel EOF. The proxy owns
            // that compatibility translation, so every shim gets one uniform
            // terminal command without duplicating protocol branches.
            child_stdin.take();
            let status = reap_legacy_child(&mut child, crate::process::backend_shutdown_timeout())?;
            if !status.success() {
                wire::write_frame(
                    &mut writer,
                    &BackendWireResponseV2::err(format!(
                        "legacy backend shim exited with status {status} during shutdown"
                    )),
                )?;
                bail!("legacy backend shim exited with status {status} during shutdown");
            }
            crate::process::lifecycle_trace(
                "proxy.shim_reaped",
                format!("language={lang} shim_pid={}", child.id()),
            );
            wire::write_frame(&mut writer, &BackendWireResponseV2::ok(OValue::Null))?;
            crate::process::lifecycle_trace(
                "proxy.shutdown_acknowledged",
                format!("language={lang}"),
            );
            return Ok(());
        }

        wire::write_frame(
            child_stdin
                .as_mut()
                .context("legacy backend command pipe is closed")?,
            &command,
        )?;
        let response = wire::read_frame::<_, BackendWireResponseV2>(&mut child_stdout)?
            .ok_or_else(|| anyhow!("legacy backend shim closed stdout unexpectedly"))?;
        wire::write_frame(&mut writer, &response)?;
    }

    child_stdin.take();
    let status = reap_legacy_child(&mut child, Duration::from_millis(250))?;
    if status.success() {
        Ok(())
    } else {
        bail!("legacy backend shim exited with status {status}")
    }
}

fn reap_legacy_child(child: &mut Child, grace: Duration) -> Result<std::process::ExitStatus> {
    let deadline = std::time::Instant::now()
        .checked_add(grace)
        .ok_or_else(|| anyhow!("legacy backend shutdown deadline overflowed"))?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let forced_deadline = std::time::Instant::now()
                .checked_add(Duration::from_millis(250))
                .ok_or_else(|| anyhow!("legacy backend forced-reap deadline overflowed"))?;
            loop {
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
                if std::time::Instant::now() >= forced_deadline {
                    bail!(
                        "legacy backend shim {} did not become waitable after termination",
                        child.id()
                    );
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn run_shell(
    tools: &BackendToolchain,
    program: &str,
    args: &[&str],
    env: Option<HashMap<String, String>>,
) -> Result<OValue> {
    let mut command = tools.command(program)?;
    command.args(args);
    if let Some(env) = env {
        command.envs(env);
    }
    output_to_value(
        program,
        command
            .output()
            .with_context(|| format!("failed to launch admitted `{program}` executable"))?,
    )
}

fn run_script(
    tools: &BackendToolchain,
    lang: &str,
    program: &str,
    suffix: &str,
    preamble: &str,
    code: &str,
) -> Result<OValue> {
    let temp = TempDir::new("o-backend-script")?;
    let source = temp.path().join(format!("main.{suffix}"));
    fs::write(&source, format!("{preamble}{code}"))?;
    let mut command = tools.command(program)?;
    output_to_value(
        lang,
        command
            .arg(&source)
            .output()
            .with_context(|| format!("failed to launch admitted `{program}` executable"))?,
    )
}

fn run_file_command(
    tools: &BackendToolchain,
    label: &str,
    program: &str,
    suffix: &str,
    code: &str,
    args: &[&str],
) -> Result<OValue> {
    let temp = TempDir::new("o-backend-file")?;
    let source = temp.path().join(format!("main.{suffix}"));
    fs::write(&source, code)?;
    let source_text = source.to_string_lossy();
    let mut command = tools.command(program)?;
    for arg in args {
        if *arg == "{file}" {
            command.arg(source_text.as_ref());
        } else {
            command.arg(arg);
        }
    }
    output_to_value(
        label,
        command
            .output()
            .with_context(|| format!("failed to launch admitted `{program}` executable"))?,
    )
}

fn run_rust(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-rust")?;
    let source = temp.path().join("main.rs");
    let binary = temp.path().join("main");
    fs::write(&source, code)?;
    let mut compiler = tools.command("rustc")?;
    expect_success(
        "rustc compilation failed",
        compiler
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .context("failed to launch admitted rustc executable")?,
    )?;
    output_to_value(
        "rust program",
        Command::new(&binary)
            .output()
            .context("failed to execute compiled Rust program")?,
    )
}

fn run_c(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-c")?;
    let source = temp.path().join("main.c");
    let binary = temp.path().join("main");
    fs::write(&source, code)?;
    let mut compiler = tools.command("cc")?;
    expect_success(
        "cc compilation failed",
        compiler
            .arg("-std=c17")
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .output()
            .context("failed to launch admitted cc executable")?,
    )?;
    output_to_value(
        "C program",
        Command::new(&binary)
            .output()
            .context("failed to execute compiled C program")?,
    )
}

fn run_cpp(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-cpp")?;
    let source = temp.path().join("main.cpp");
    let binary = temp.path().join("main");
    fs::write(&source, code)?;
    let mut compiler = tools.command("g++")?;
    expect_success(
        "g++ compilation failed",
        compiler
            .arg("-std=c++17")
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .output()
            .context("failed to launch admitted g++ executable")?,
    )?;
    output_to_value(
        "C++ program",
        Command::new(&binary)
            .output()
            .context("failed to execute compiled C++ program")?,
    )
}

fn run_java(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-java")?;
    let class_name = java_class_name(code);
    let source = temp.path().join(format!("{class_name}.java"));
    fs::write(&source, code)?;
    let mut compiler = tools.command("javac")?;
    expect_success(
        "javac compilation failed",
        compiler
            .arg(&source)
            .output()
            .context("failed to launch admitted javac executable")?,
    )?;
    let mut runtime = tools.command("java")?;
    output_to_value(
        "java",
        runtime
            .arg("-cp")
            .arg(temp.path())
            .arg(class_name)
            .output()
            .context("failed to launch admitted java executable")?,
    )
}

fn run_nix(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let mut command = tools.command("nix")?;
    let output = command
        .args([
            "--extra-experimental-features",
            "nix-command",
            "eval",
            "--json",
            "--impure",
            "--expr",
            code,
        ])
        .output()
        .context("failed to launch admitted nix executable")?;
    expect_success("nix eval failed", output.clone())?;
    let json: Value =
        serde_json::from_slice(&output.stdout).context("nix eval returned non-JSON")?;
    json_value_to_ovalue(json)
}

fn run_nix_store(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let mut command = tools.command("nix")?;
    let output = command
        .args([
            "--extra-experimental-features",
            "nix-command",
            "eval",
            "--raw",
            "--impure",
            "--expr",
            code,
        ])
        .output()
        .context("failed to launch admitted nix executable")?;
    expect_success("nix eval --raw failed", output.clone())?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !path.starts_with("/nix/store/") {
        bail!("expression did not evaluate to a Nix store path: {path:?}");
    }
    Ok(OValue::store_path(path))
}

fn run_haskell(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-haskell")?;
    let source = temp.path().join("Main.hs");
    fs::write(&source, code)?;
    if tools.contains("runghc") {
        let mut command = tools.command("runghc")?;
        return output_to_value(
            "Haskell",
            command
                .arg(&source)
                .output()
                .context("failed to launch admitted runghc executable")?,
        );
    }
    if tools.contains("ghc") {
        let binary = temp.path().join("Main");
        let mut compiler = tools.command("ghc")?;
        expect_success(
            "ghc compilation failed",
            compiler
                .arg("-o")
                .arg(&binary)
                .arg(&source)
                .output()
                .context("failed to launch admitted ghc executable")?,
        )?;
        return output_to_value(
            "Haskell",
            Command::new(&binary)
                .output()
                .context("failed to execute compiled Haskell program")?,
        );
    }
    bail!("admitted Haskell runtime alternative contains neither `runghc` nor `ghc`")
}

fn run_ocaml(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-ocaml")?;
    let source = temp.path().join("main.ml");
    fs::write(&source, code)?;
    if tools.contains("ocaml") {
        let mut command = tools.command("ocaml")?;
        return output_to_value(
            "OCaml",
            command
                .arg(&source)
                .output()
                .context("failed to launch admitted ocaml executable")?,
        );
    }
    let compiler = if tools.contains("ocamlopt") {
        Some("ocamlopt")
    } else if tools.contains("ocamlc") {
        Some("ocamlc")
    } else {
        None
    };
    let Some(compiler) = compiler else {
        bail!("admitted OCaml runtime alternative contains no supported executable");
    };
    let binary = temp.path().join("main");
    let mut compiler_command = tools.command(compiler)?;
    expect_success(
        format!("{compiler} compilation failed"),
        compiler_command
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .output()
            .with_context(|| format!("failed to launch admitted {compiler} executable"))?,
    )?;
    output_to_value(
        "OCaml",
        Command::new(&binary)
            .output()
            .context("failed to execute compiled OCaml program")?,
    )
}

fn run_common_lisp(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-lisp")?;
    let source = temp.path().join("main.lisp");
    fs::write(&source, code)?;
    if tools.contains("sbcl") {
        let mut command = tools.command("sbcl")?;
        return output_to_value(
            "Common Lisp",
            command
                .arg("--script")
                .arg(&source)
                .output()
                .context("failed to launch admitted sbcl executable")?,
        );
    }
    if tools.contains("clisp") {
        let mut command = tools.command("clisp")?;
        return output_to_value(
            "Common Lisp",
            command
                .arg(&source)
                .output()
                .context("failed to launch admitted clisp executable")?,
        );
    }
    bail!("admitted Common Lisp runtime alternative contains neither `sbcl` nor `clisp`")
}

fn run_csharp(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-csharp")?;
    if tools.contains("dotnet") {
        let mut project_command = tools.command("dotnet")?;
        expect_success(
            "dotnet project creation failed",
            project_command
                .args(["new", "console", "--force", "-o"])
                .arg(temp.path())
                .output()
                .context("failed to launch admitted dotnet executable")?,
        )?;
        fs::write(temp.path().join("Program.cs"), code)?;
        let mut run_command = tools.command("dotnet")?;
        return output_to_value(
            "C#",
            run_command
                .arg("run")
                .arg("--project")
                .arg(temp.path())
                .output()
                .context("failed to launch admitted dotnet executable for `dotnet run`")?,
        );
    }
    if tools.contains("mcs") && tools.contains("mono") {
        let source = temp.path().join("Program.cs");
        let binary = temp.path().join("Program.exe");
        fs::write(&source, code)?;
        let mut compiler = tools.command("mcs")?;
        expect_success(
            "mcs compilation failed",
            compiler
                .arg(format!("-out:{}", binary.display()))
                .arg(&source)
                .output()
                .context("failed to launch admitted mcs executable")?,
        )?;
        let mut runtime = tools.command("mono")?;
        return output_to_value(
            "C#",
            runtime
                .arg(&binary)
                .output()
                .context("failed to launch admitted mono executable")?,
        );
    }
    bail!("admitted C# runtime alternative is neither `dotnet` nor `mcs + mono`")
}

fn run_matlab(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-matlab")?;
    let source = temp.path().join("script.m");
    fs::write(&source, code)?;
    if tools.contains("octave") {
        let mut command = tools.command("octave")?;
        return output_to_value(
            "MATLAB/Octave",
            command
                .args(["--no-gui", "--norc", "--silent"])
                .arg(&source)
                .output()
                .context("failed to launch admitted octave executable")?,
        );
    }
    if tools.contains("matlab") {
        let script_dir = temp.path().to_string_lossy();
        let mut command = tools.command("matlab")?;
        return output_to_value(
            "MATLAB",
            command
                .arg("-batch")
                .arg(format!("addpath('{script_dir}'); script"))
                .output()
                .context("failed to launch admitted matlab executable")?,
        );
    }
    bail!("admitted MATLAB runtime alternative contains neither `octave` nor `matlab`")
}

fn run_mathematica(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    run_file_command(
        tools,
        "Mathematica",
        "wolframscript",
        "wls",
        code,
        &["-file", "{file}"],
    )
}

fn run_webassembly(tools: &BackendToolchain, code: &str) -> Result<OValue> {
    let temp = TempDir::new("o-backend-wasm")?;
    let wasm = temp.path().join("module.wasm");
    if code.trim_start().starts_with("(module") || code.trim_start().starts_with("(func") {
        let wat = temp.path().join("module.wat");
        fs::write(&wat, code)?;
        let mut converter = tools.command("wat2wasm")?;
        expect_success(
            "wat2wasm failed",
            converter
                .arg(&wat)
                .arg("-o")
                .arg(&wasm)
                .output()
                .context("failed to launch admitted wat2wasm executable")?,
        )?;
    } else {
        fs::write(&wasm, code.as_bytes())?;
    }

    if tools.contains("wasmtime") {
        let mut command = tools.command("wasmtime")?;
        return output_to_value(
            "wasmtime",
            command
                .arg(&wasm)
                .output()
                .context("failed to launch admitted wasmtime executable")?,
        );
    }
    if tools.contains("wasmer") {
        let mut command = tools.command("wasmer")?;
        return output_to_value(
            "wasmer",
            command
                .arg("run")
                .arg(&wasm)
                .output()
                .context("failed to launch admitted wasmer executable")?,
        );
    }
    bail!("admitted WebAssembly runtime alternative contains neither `wasmtime` nor `wasmer`")
}

fn output_to_value(label: &str, output: Output) -> Result<OValue> {
    expect_success(format!("{label} exited with failure"), output.clone())?;
    Ok(stdout_to_ovalue(&String::from_utf8_lossy(&output.stdout)))
}

fn expect_success(label: impl AsRef<str>, output: Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let code = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    if stdout.trim().is_empty() {
        bail!("{} (code {code})\n{}", label.as_ref(), stderr.trim());
    }
    bail!(
        "{} (code {code})\nSTDERR:\n{}\nSTDOUT:\n{}",
        label.as_ref(),
        stderr.trim(),
        stdout.trim()
    )
}

fn stdout_to_ovalue(output: &str) -> OValue {
    let text = trim_stdout(output);
    let stripped = text.trim();
    if !stripped.is_empty() {
        if let Ok(json) = serde_json::from_str::<Value>(stripped) {
            if let Ok(value) = json_value_to_ovalue(json) {
                return value;
            }
        }

        if is_integer_literal(stripped) {
            if let Ok(int) = stripped.parse::<i64>() {
                return OValue::int(int);
            }
            if let Some(big) = BigInt::parse_bytes(stripped.as_bytes(), 10) {
                return OValue::big_int(big);
            }
        }

        if is_float_literal(stripped) {
            if let Ok(float) = stripped.parse::<f64>() {
                return float_to_ovalue(float);
            }
        }
    }
    OValue::str_(text)
}

fn json_value_to_ovalue(value: Value) -> Result<OValue> {
    Ok(match value {
        Value::Null => OValue::Null,
        Value::Bool(v) => OValue::bool_(v),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                OValue::int(value)
            } else if let Some(value) = number.as_u64() {
                match i64::try_from(value) {
                    Ok(value) => OValue::int(value),
                    Err(_) => OValue::big_int(BigInt::from(value)),
                }
            } else if let Some(value) = number.as_f64() {
                float_to_ovalue(value)
            } else {
                OValue::str_(number.to_string())
            }
        }
        Value::String(v) => OValue::str_(v),
        Value::Array(values) => OValue::list(
            values
                .into_iter()
                .map(json_value_to_ovalue)
                .collect::<Result<Vec<_>>>()?,
        ),
        Value::Object(map) => {
            let tagged = map
                .get("t")
                .and_then(Value::as_str)
                .is_some_and(|tag| !tag.is_empty());
            let value = Value::Object(map);
            if tagged {
                serde_json::from_value(value).context("tagged JSON is not a valid OValue")?
            } else if let Value::Object(map) = value {
                OValue::map(
                    map.into_iter()
                        .map(|(key, value)| Ok((key, json_value_to_ovalue(value)?)))
                        .collect::<Result<HashMap<_, _>>>()?,
                )
            } else {
                unreachable!()
            }
        }
    })
}

fn sqlite_json_to_ovalue(value: Value) -> Result<OValue> {
    let Value::Array(rows) = value else {
        return json_value_to_ovalue(value);
    };
    if rows.len() == 1 {
        if let Some(object) = rows[0].as_object() {
            if object.len() == 1 {
                if let Some((_, value)) = object.iter().next() {
                    return json_value_to_ovalue(value.clone());
                }
            }
        }
    }
    json_value_to_ovalue(Value::Array(rows))
}

fn sql_has_query_result(code: &str) -> bool {
    code.split(';')
        .map(str::trim)
        .rfind(|stmt| !stmt.is_empty())
        .is_some_and(|stmt| {
            let upper = stmt
                .chars()
                .take_while(|ch| !ch.is_whitespace() && *ch != '(')
                .collect::<String>()
                .to_ascii_uppercase();
            matches!(upper.as_str(), "SELECT" | "WITH" | "PRAGMA")
        })
}

fn float_to_ovalue(value: f64) -> OValue {
    if value.is_finite() {
        OValue::float(value)
    } else {
        OValue::number(ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits: value.to_bits().to_be_bytes().to_vec(),
        })
    }
}

fn trim_stdout(output: &str) -> String {
    let mut text = output.to_string();
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

fn is_integer_literal(value: &str) -> bool {
    let rest = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn is_float_literal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (lower.contains('.') || lower.contains('e')) && lower.parse::<f64>().is_ok()
}

fn scalar_env(bindings: HashMap<String, OValue>) -> HashMap<String, String> {
    bindings
        .into_iter()
        .filter_map(|(name, value)| scalar_string(&value).map(|value| (name, value)))
        .collect()
}

fn scalar_string(value: &OValue) -> Option<String> {
    match value {
        OValue::Text {
            v: crate::value::OText { utf8: v, .. },
        } => Some(v.clone()),
        OValue::Number { v } => number_scalar_string(v),
        OValue::Bool { v } => Some(v.to_string()),
        _ => None,
    }
}

fn number_scalar_string(value: &ONumber) -> Option<String> {
    match value {
        ONumber::Int { v } => Some(v.to_string()),
        ONumber::BinaryFloat {
            format: FloatFormat::F32,
            bits,
        } if bits.len() == 4 => {
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(bits);
            let value = f32::from_bits(u32::from_be_bytes(raw)) as f64;
            value.is_finite().then(|| value.to_string())
        }
        ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits,
        } if bits.len() == 8 => {
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(bits);
            let value = f64::from_bits(u64::from_be_bytes(raw));
            value.is_finite().then(|| value.to_string())
        }
        _ => None,
    }
}

fn javascript_preamble(bindings: &HashMap<String, OValue>) -> String {
    let mut preamble = String::new();
    for (name, value) in bindings {
        if !is_identifier(name) {
            continue;
        }
        match value {
            OValue::Text { v } => {
                preamble.push_str(&format!(
                    "const {name} = {};\n",
                    serde_json::to_string(&v.utf8).unwrap_or_else(|_| "null".to_string())
                ));
            }
            OValue::Number { v } => {
                if let Some(value) = number_scalar_string(v) {
                    preamble.push_str(&format!("const {name} = {value};\n"));
                }
            }
            OValue::Bool { v } => {
                preamble.push_str(&format!(
                    "const {name} = {};\n",
                    if *v { "true" } else { "false" }
                ));
            }
            OValue::Null => preamble.push_str(&format!("const {name} = null;\n")),
            OValue::List { v } => push_json_const(&mut preamble, name, v),
            OValue::Map { v } => push_json_const(&mut preamble, name, v),
            _ => {}
        }
    }
    preamble
}

fn push_json_const<T: serde::Serialize>(preamble: &mut String, name: &str, value: &T) {
    if let Ok(json) = serde_json::to_string(value) {
        preamble.push_str(&format!("const {name} = {json};\n"));
    }
}

fn ruby_preamble(bindings: &HashMap<String, OValue>) -> String {
    let mut preamble = String::new();
    for (name, value) in bindings {
        if !is_identifier(name) {
            continue;
        }
        match value {
            OValue::Text { v } => {
                preamble.push_str(&format!(
                    "{name} = {}\n",
                    serde_json::to_string(&v.utf8).unwrap_or_else(|_| "nil".to_string())
                ));
            }
            OValue::Number { v } => {
                if let Some(value) = number_scalar_string(v) {
                    preamble.push_str(&format!("{name} = {value}\n"));
                }
            }
            OValue::Bool { v } => {
                preamble.push_str(&format!("{name} = {}\n", if *v { "true" } else { "false" }));
            }
            OValue::Null => preamble.push_str(&format!("{name} = nil\n")),
            _ => {}
        }
    }
    preamble
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn java_class_name(code: &str) -> String {
    find_class_after(code, "public class")
        .or_else(|| find_class_after(code, "class"))
        .unwrap_or_else(|| "Main".to_string())
}

fn find_class_after(code: &str, marker: &str) -> Option<String> {
    let idx = code.find(marker)?;
    let after = &code[idx + marker.len()..];
    let name = after.split_whitespace().next()?;
    let name = name
        .chars()
        .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
        .collect::<String>();
    (!name.is_empty()).then_some(name)
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self> {
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{prefix}-{}-{now}-{counter}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("failed to create backend temp dir"),
            }
        }
        bail!("failed to create unique backend temp dir")
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{has_native_backend, sql_checkpoint_profile_accepts};

    #[test]
    fn sql_checkpoint_profile_is_portable_only_for_main_autocommit_state() {
        assert!(sql_checkpoint_profile_accepts(
            "CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (42);"
        ));
        assert!(sql_checkpoint_profile_accepts(
            "SELECT value FROM items ORDER BY value;"
        ));
        for source in [
            "ATTACH DATABASE 'other.db' AS other;",
            "BEGIN; INSERT INTO items VALUES (1);",
            "CREATE TEMP TABLE transient(value INTEGER);",
            "PRAGMA foreign_keys = ON;",
            "SELECT last_insert_rowid();",
            ".load './extension'",
        ] {
            assert!(!sql_checkpoint_profile_accepts(source), "{source}");
        }
    }

    #[test]
    fn production_native_launches_cannot_reselect_from_ambient_path() {
        let source = include_str!("backend.rs");
        let (production, _) = source
            .rsplit_once("#[cfg(test)]\nmod tests")
            .expect("backend unit-test module remains the final source section");

        assert!(
            !production.contains("which::which"),
            "production backend dispatch must never reselect an executable from PATH"
        );
        for (offset, _) in production.match_indices("Command::new(") {
            let launch = &production[offset..];
            assert!(
                launch.starts_with("Command::new(&binary)"),
                "only freshly compiled absolute temp binaries may bypass the admitted toolchain: {}",
                launch.lines().next().unwrap_or_default()
            );
        }
    }

    #[test]
    fn native_backend_dispatch_is_projected_from_the_canonical_catalog() {
        let catalog_native = crate::ir::BackendRegistry::global()
            .canonical_specs()
            .iter()
            .filter(|spec| spec.adapter == crate::ir::BackendAdapterKind::NativeRust)
            .map(|spec| spec.name)
            .collect::<std::collections::BTreeSet<_>>();
        let implemented = super::NATIVE_BACKEND_HANDLERS
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(catalog_native, implemented);

        for backend in super::NATIVE_BACKEND_HANDLERS {
            assert!(has_native_backend(backend), "{backend}");
        }

        for backend in [
            "O",
            "quote",
            "html",
            "markdown",
            "latex",
            "text",
            "python",
            "py",
            "nixos_test",
            "ubuntu_vm",
            "unknown",
        ] {
            assert!(!has_native_backend(backend), "{backend}");
        }
    }
}
