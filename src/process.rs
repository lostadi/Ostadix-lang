use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::value::{OValue, OWireCommand, OWireResponse};

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

    fn send_command(&mut self, command: &OWireCommand) -> Result<OWireResponse> {
        let line = serde_json::to_string(command)
            .context("failed to serialize OWireCommand")?;

        writeln!(self.stdin, "{line}")
            .context("failed to write command to backend stdin")?;

        self.stdin
            .flush()
            .context("failed to flush backend stdin")?;

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

        Ok(response)
    }

    fn exec(&mut self, code: &str, bindings: HashMap<String, OValue>) -> Result<OValue> {
        let response = self.send_command(&OWireCommand::Exec {
            code: code.to_string(),
            bindings,
        })?;

        response.into_result()
    }

    fn ping(&mut self) -> Result<()> {
        let response = self.send_command(&OWireCommand::Ping)?;
        response.into_result().map(|_| ())
    }

    fn cleanup(&mut self) -> Result<()> {
        let response = self.send_command(&OWireCommand::Cleanup);
        let _ = self.child.kill();
        let _ = self.child.wait();

        match response {
            Ok(r) => r.into_result().map(|_| ()),
            Err(e) => Err(e),
        }
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
            let process = BackendProcess::new(shim_path)
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
