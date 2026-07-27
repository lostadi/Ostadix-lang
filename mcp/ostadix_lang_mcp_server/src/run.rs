//! Subprocess runner (stdout/stderr captured; never pollute MCP stdout).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub async fn run_cmd(
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
        .stderr(Stdio::piped());
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", program.display()))?;
    let out = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
        .await
        .map_err(|_| format!("timeout after {timeout_secs}s"))?
        .map_err(|e| format!("wait: {e}"))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((code, stdout, stderr))
}

pub fn format_run(code: i32, stdout: &str, stderr: &str) -> String {
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

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!(
        "{}\n… truncated {} bytes (showing first {})\n",
        &s[..max],
        s.len() - max,
        max
    )
}
