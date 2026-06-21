use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::value::{OValue, OWireCommand, OWireResponse};

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

impl BackendProcess {
    fn new(shim_path: &Path) -> Result<Self> {
        let mut command = if shim_path.extension().and_then(|s| s.to_str()) == Some("py") {
            let mut cmd = Command::new("python3");
            cmd.arg(shim_path);
            cmd
        } else {
            Command::new(shim_path)
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

    fn ping(&mut self) -> Result<()> {
        self.send_command(&OWireCommand::Ping)?;
        match self.recv_step()? {
            ExecStep::Done(_) => Ok(()),
            ExecStep::EvalRequest { src, .. } => Err(anyhow!(
                "unexpected eval_request during ping (src: {:?})",
                &src[..src.len().min(40)]
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
    registry: HashMap<(String, u32), BackendProcess>,
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
    pub fn send_exec(
        &mut self,
        lang: &str,
        env_id: u32,
        code: &str,
        bindings: HashMap<String, OValue>,
        shim_path: &Path,
    ) -> Result<()> {
        let key = (lang.to_string(), env_id);
        if !self.registry.contains_key(&key) {
            let mut process = BackendProcess::new(shim_path)
                .with_context(|| format!("failed to start backend for language `{lang}`"))?;
            process
                .ping()
                .with_context(|| format!("backend `{lang}` did not respond to health check"))?;
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
    pub fn recv_exec_step(&mut self, lang: &str, env_id: u32) -> Result<ExecStep> {
        let key = (lang.to_string(), env_id);
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
    pub fn send_eval_result(&mut self, lang: &str, env_id: u32, value: OValue) -> Result<()> {
        let key = (lang.to_string(), env_id);
        self.registry
            .get_mut(&key)
            .ok_or_else(|| anyhow!("no live backend process for `{lang}[{env_id}]`"))?
            .send_eval_result(value)
            .with_context(|| format!("failed to send eval_result to backend `{lang}`"))
    }

    pub fn exec(
        &mut self,
        lang: &str,
        env_id: u32,
        code: &str,
        bindings: HashMap<String, OValue>,
        shim_path: &Path,
    ) -> Result<OValue> {
        let key = (lang.to_string(), env_id);

        if !self.registry.contains_key(&key) {
            let mut process = BackendProcess::new(shim_path)
                .with_context(|| format!("failed to start backend for language `{lang}`"))?;
            process
                .ping()
                .with_context(|| format!("backend `{lang}` did not respond to health check"))?;
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
        let key = (lang.to_string(), env_id);

        if let Some(mut process) = self.registry.remove(&key) {
            process.cleanup()
        } else {
            Ok(())
        }
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
        BackendProcess::new(&python_shim_path())
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
}
