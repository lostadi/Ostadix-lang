use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::capability::BackendSandboxPolicy;
use crate::value::{OValue, OWireCommand, OWireResponse};

const PYTHON_POLICY_BOOTSTRAP: &str = r#"
import json, os, runpy, sys
_o_permissions = frozenset(json.loads(os.environ.pop("O_BACKEND_AUTHORITIES", "[]")))
_o_runtime_candidates = json.loads(os.environ.pop("O_BACKEND_RUNTIME_ROOTS", "[]"))
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
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

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

fn direct_shim_command(shim_path: &Path, sandbox: &BackendSandboxPolicy) -> Command {
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
    fn new(shim_path: &Path, sandbox: &BackendSandboxPolicy) -> Result<Self> {
        if !shim_path.exists() {
            return Err(anyhow!("backend shim not found: {}", shim_path.display()));
        }

        let mut command = if shim_path.extension().and_then(|s| s.to_str()) == Some("py") {
            python_shim_command(shim_path, sandbox)?
        } else {
            direct_shim_command(shim_path, sandbox)
        };

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to spawn backend shim: {}", shim_path.display()))?;

        let stdin = child
            .stdin
            .take()
            .context("backend process did not provide stdin")?;

        let stdout = child
            .stdout
            .take()
            .context("backend process did not provide stdout")?;

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
        })
    }

    fn send_command(&mut self, command: &OWireCommand) -> Result<()> {
        let line = serde_json::to_string(command).context("failed to serialize OWireCommand")?;
        writeln!(self.stdin, "{line}").context("failed to write command to backend stdin")?;
        self.stdin
            .flush()
            .context("failed to flush backend stdin")?;
        Ok(())
    }

    fn recv_step(&mut self) -> Result<ExecStep> {
        let mut response_line = String::new();
        let bytes_read = self
            .stdout
            .read_line(&mut response_line)
            .context("failed to read response from backend stdout")?;

        if bytes_read == 0 {
            return Err(anyhow!("backend process closed stdout unexpectedly"));
        }

        let response: OWireResponse = serde_json::from_str(&response_line)
            .with_context(|| format!("failed to parse backend response: {response_line:?}"))?;

        match response {
            OWireResponse::Ok { value } => Ok(ExecStep::Done(value)),
            OWireResponse::Err { message } => Err(anyhow!("{}", message)),
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

    fn cleanup(&mut self) -> Result<()> {
        let send_result = self.send_command(&OWireCommand::Cleanup);
        let _ = self.child.kill();
        let _ = self.child.wait();
        send_result
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
            let process = BackendProcess::new(shim_path, sandbox)
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
            .with_context(|| format!("failed to send Exec to backend `{lang}`"))
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
        step.with_context(|| format!("backend `{lang}[{env_id}]` recv_step failed"))
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
            let process = BackendProcess::new(shim_path, sandbox)
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
                process.cleanup()?;
            }
        }
        Ok(())
    }

    pub fn cleanup_all(&mut self) {
        let processes: Vec<_> = self.registry.drain().map(|(_, process)| process).collect();

        for mut process in processes {
            let _ = process.cleanup();
        }
    }
}

impl Drop for ProcessRegistry {
    fn drop(&mut self) {
        self.cleanup_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_shim_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("backends/python_shim.py")
    }

    fn spawn_python_shim() -> Result<BackendProcess> {
        BackendProcess::new(&python_shim_path(), &BackendSandboxPolicy::none())
    }

    fn spawn_python_shim_with(
        permissions: impl IntoIterator<Item = crate::value::BackendAuthority>,
    ) -> Result<BackendProcess> {
        BackendProcess::new(&python_shim_path(), &BackendSandboxPolicy::new(permissions))
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
        process.cleanup()?;
        Ok(())
    }

    #[test]
    fn exec_without_bindings_returns_int_result() -> Result<()> {
        let mut process = spawn_python_shim()?;

        let value = process.exec("__oval_result__ = 42", HashMap::new())?;

        assert_eq!(value, OValue::int(42));
        process.cleanup()?;
        Ok(())
    }

    #[test]
    fn exec_with_string_binding_round_trips_through_shim() -> Result<()> {
        let mut process = spawn_python_shim()?;
        let bindings = HashMap::from([("msg".to_string(), OValue::str_("hello"))]);

        let value = process.exec("__oval_result__ = msg.upper()", bindings)?;

        assert_eq!(value, OValue::str_("HELLO"));
        process.cleanup()?;
        Ok(())
    }

    #[test]
    fn exec_reports_backend_errors_without_panicking() -> Result<()> {
        let mut process = spawn_python_shim()?;

        let err = process
            .exec("raise RuntimeError('boom from shim')", HashMap::new())
            .unwrap_err();

        assert!(err.to_string().contains("boom from shim"));
        process.cleanup()?;
        Ok(())
    }

    #[test]
    fn cleanup_command_returns_ok_null() -> Result<()> {
        let mut process = spawn_python_shim()?;

        process.send_command(&OWireCommand::Cleanup)?;
        let value = expect_done(process.recv_step()?);

        assert_eq!(value, OValue::Null);
        process.cleanup()?;
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
        process.cleanup()?;
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
        process.cleanup()?;
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
        process.cleanup()?;
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
        process.cleanup()?;
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
        process.cleanup()?;
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
        process.cleanup()?;
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
