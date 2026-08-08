//! End-to-end coverage for explicitly autonomous hosted execution.
//!
//! These tests use externally observable files so they exercise the compiled
//! `O` CLI, the admitted HGraph, the persistent worker pool, and Python shim
//! callbacks together rather than merely inspecting a static schedule.

use std::fs;
use std::io::{BufReader, BufWriter, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use o_lang::value::{OValue, OWireCommand, OWireResponse};
use o_lang::wire;

struct RunOutcome {
    output: Output,
    workdir: tempfile::TempDir,
    pid: u32,
    trace_path: PathBuf,
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_graph_bounded(source: &str, workers: usize) -> RunOutcome {
    run_graph_bounded_with_operation_timeout(source, workers, None)
}

fn run_graph_bounded_with_operation_timeout(
    source: &str,
    workers: usize,
    operation_timeout: Option<Duration>,
) -> RunOutcome {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workdir = tempfile::tempdir().expect("create isolated test directory");
    let program = workdir.path().join("program.O");
    fs::write(&program, source).expect("write test program");

    let trace_path = std::env::var_os("O_LIFECYCLE_TRACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| workdir.path().join("lifecycle.log"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_O"));
    command
        .env_remove("O_EXECUTOR")
        .env("O_LIFECYCLE_TRACE", &trace_path)
        .arg("--executor")
        .arg("graph")
        .arg("--workers")
        .arg(workers.to_string())
        .arg(&program)
        .arg(root.join("backends"))
        .current_dir(workdir.path())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("O_TEST_WORKDIR", workdir.path())
        .env("O_BACKEND_SHUTDOWN_TIMEOUT_MS", "2000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(timeout) = operation_timeout {
        command.env(
            "O_BACKEND_OPERATION_TIMEOUT_MS",
            timeout.as_millis().to_string(),
        );
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().expect("start O CLI");
    let pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll O CLI") {
            break status;
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            terminate_traced_backend_groups(&trace_path, pid);
            #[cfg(unix)]
            if let Ok(group) = i32::try_from(pid) {
                // SAFETY: the test created this child as its own process-group
                // leader, so a negative PID targets only the owned test tree.
                let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = wait_for_child(&mut child, Duration::from_secs(1));
            panic!("O CLI exceeded the 20-second integration-test deadline");
        }
        thread::sleep(Duration::from_millis(20));
    };

    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("captured stdout")
        .read_to_end(&mut stdout)
        .expect("read O stdout");
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .expect("captured stderr")
        .read_to_end(&mut stderr)
        .expect("read O stderr");

    RunOutcome {
        output: Output {
            status,
            stdout,
            stderr,
        },
        workdir,
        pid,
        trace_path,
    }
}

const INTERVAL_BATCH: &str = r#"autonomous(batch(
python^(
from pathlib import Path
import os
import time
start = time.monotonic_ns()
time.sleep(0.75)
end = time.monotonic_ns()
(Path(os.environ["O_TEST_WORKDIR"]) / "left.interval").write_text(f"{start} {end}\n", encoding="utf-8")
__oval_result__ = "left"
)_python,
python^(
from pathlib import Path
import os
import time
start = time.monotonic_ns()
time.sleep(0.75)
end = time.monotonic_ns()
(Path(os.environ["O_TEST_WORKDIR"]) / "right.interval").write_text(f"{start} {end}\n", encoding="utf-8")
__oval_result__ = "right"
)_python
))
"#;

fn assert_success(run: &RunOutcome, context: &str) {
    assert!(
        run.output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr),
    );
}

fn wait_for_child(child: &mut std::process::Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll child process") {
            return status;
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            if let Ok(group) = i32::try_from(child.id()) {
                // SAFETY: callers create the child as an owned process-group
                // leader before using this bounded wait helper.
                let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            let _ = child.kill();
            let forced_deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if child.try_wait().expect("poll terminated child").is_some() {
                    panic!("child process exceeded {} ms", timeout.as_millis());
                }
                assert!(
                    Instant::now() < forced_deadline,
                    "child process remained active after forced termination"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn field_pid(line: &str, field: &str) -> Option<u32> {
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(field))
        .and_then(|value| value.parse().ok())
}

#[cfg(unix)]
fn terminate_traced_backend_groups(trace_path: &Path, parent_pid: u32) {
    let Ok(trace) = fs::read_to_string(trace_path) else {
        return;
    };
    let prefix = format!("pid={parent_pid} ");
    for backend_pid in trace
        .lines()
        .filter(|line| line.contains(&prefix) && line.contains("event=worker.backend_spawned"))
        .filter_map(|line| field_pid(line, "backend_pid="))
    {
        if let Ok(group) = i32::try_from(backend_pid) {
            // SAFETY: lifecycle traces contain only groups created and owned
            // by this exact O child; the negative PID targets that group.
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    }
}

#[cfg(target_os = "linux")]
fn process_or_group_exists(pid: u32) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries.flatten() {
        let Some(candidate) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            return true;
        };
        let mut fields = stat[close + 1..].split_whitespace();
        let state = fields.next();
        let _parent = fields.next();
        let group = fields.next().and_then(|field| field.parse::<u32>().ok());
        if (candidate == pid || group == Some(pid)) && !matches!(state, Some("Z" | "X" | "x")) {
            return true;
        }
    }
    false
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_or_group_exists(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    // SAFETY: signal zero performs existence/permission checks only.
    let process = unsafe { libc::kill(pid, 0) };
    if process == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
        return true;
    }
    // SAFETY: the negative PID addresses the process group and signal zero
    // does not mutate it.
    let group = unsafe { libc::kill(-pid, 0) };
    group == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn assert_traced_backend_groups_quiescent(run: &RunOutcome) {
    let trace = fs::read_to_string(&run.trace_path).unwrap_or_else(|error| {
        panic!("read lifecycle trace {}: {error}", run.trace_path.display())
    });
    let prefix = format!("pid={} ", run.pid);
    let backend_pids = trace
        .lines()
        .filter(|line| line.contains(&prefix) && line.contains("event=worker.backend_spawned"))
        .filter_map(|line| field_pid(line, "backend_pid="))
        .collect::<Vec<_>>();
    assert!(
        !backend_pids.is_empty(),
        "no backend process was recorded for O pid {}\ntrace:\n{trace}",
        run.pid
    );
    for backend_pid in backend_pids {
        assert!(
            !process_or_group_exists(backend_pid),
            "backend process group {backend_pid} remained after O pid {} exited",
            run.pid
        );
    }
}

fn read_interval(path: &Path) -> (u128, u128) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read interval {}: {error}", path.display()));
    let values = text
        .split_whitespace()
        .map(|value| {
            value
                .parse::<u128>()
                .unwrap_or_else(|error| panic!("parse interval value {value:?}: {error}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2, "malformed interval in {}", path.display());
    assert!(values[0] < values[1], "non-positive interval in {text:?}");
    (values[0], values[1])
}

#[test]
fn explicit_autonomous_ephemeral_python_blocks_overlap() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    for workers in [2, 4] {
        for repetition in 1..=2 {
            let run = run_graph_bounded(INTERVAL_BATCH, workers);
            assert_success(
                &run,
                &format!("autonomous Python batch, workers={workers}, repetition={repetition}"),
            );

            let left = read_interval(&run.workdir.path().join("left.interval"));
            let right = read_interval(&run.workdir.path().join("right.interval"));
            for (name, (start, end)) in [("left", left), ("right", right)] {
                let elapsed = end - start;
                assert!(
                    (500_000_000..=5_000_000_000).contains(&elapsed),
                    "{name} interval was outside the bounded sleep window: {elapsed} ns"
                );
            }

            let overlap_start = left.0.max(right.0);
            let overlap_end = left.1.min(right.1);
            assert!(
                overlap_start < overlap_end,
                "workers={workers}, repetition={repetition} did not overlap: left={left:?}, right={right:?}"
            );
        }
    }
}

#[test]
fn explicit_one_worker_override_serializes_autonomous_blocks() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    for repetition in 1..=2 {
        let run = run_graph_bounded(INTERVAL_BATCH, 1);
        assert_success(
            &run,
            &format!("single-worker autonomous Python batch, repetition={repetition}"),
        );

        let left = read_interval(&run.workdir.path().join("left.interval"));
        let right = read_interval(&run.workdir.path().join("right.interval"));
        assert!(
            left.1 <= right.0 || right.1 <= left.0,
            "one-worker override repetition={repetition} allowed overlap: left={left:?}, right={right:?}"
        );
    }
}

#[test]
fn ordinary_ephemeral_python_preserves_strict_fail_stop() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    let run = run_graph_bounded(
        r#"python^(
raise RuntimeError("strict-stop")
)_python
python^(
from pathlib import Path
import os
(Path(os.environ["O_TEST_WORKDIR"]) / "later-effect").write_text("must not happen", encoding="utf-8")
__oval_result__ = "unexpected"
)_python
"#,
        2,
    );

    assert!(
        !run.output.status.success(),
        "the first ordinary hosted failure unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&run.output.stderr).contains("strict-stop"),
        "the selected error did not report the first failure\nstderr:\n{}",
        String::from_utf8_lossy(&run.output.stderr),
    );
    assert!(
        !run.workdir.path().join("later-effect").exists(),
        "strict fail-stop execution published a later hosted effect"
    );
}

#[test]
fn autonomous_worker_o_eval_sees_lexical_scope() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    let run = run_graph_bounded(
        r#"let lexical = text^(lexical-scope-ok)_text
let quoted = quote^(text^($lexical)_text)_quote
autonomous(batch(
python^(
from pathlib import Path
import os
value = O.eval(quoted)
(Path(os.environ["O_TEST_WORKDIR"]) / "callback.result").write_text(value, encoding="utf-8")
__oval_result__ = value
)_python
))
"#,
        2,
    );
    assert_success(&run, "autonomous worker O.eval callback");

    let observed = fs::read_to_string(run.workdir.path().join("callback.result"))
        .expect("autonomous callback result file");
    assert_eq!(observed, "lexical-scope-ok");
    let trace = fs::read_to_string(&run.trace_path).expect("read successful callback trace");
    for event in [
        "coordinator.task_prepared",
        "coordinator.task_submitted",
        "worker.task_received",
        "worker.backend_spawned",
        "worker.exec_sent",
        "worker.done_received",
        "worker.shutdown_sent",
        "proxy.shutdown_received",
        "proxy.shim_reaped",
        "proxy.shutdown_acknowledged",
        "worker.shutdown_acknowledged",
        "worker.backend_wait_returned",
        "worker.completion_emitted",
        "coordinator.completion_received",
        "coordinator.result_buffered",
        "coordinator.result_settled",
        "pool.submission_channel_closed",
        "pool.worker_joined",
    ] {
        assert!(
            trace.contains(&format!("event={event}")),
            "{event}\n{trace}"
        );
    }
    #[cfg(unix)]
    assert_traced_backend_groups_quiescent(&run);
}

#[test]
fn autonomous_worker_o_eval_failure_terminates_backend() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    let run = run_graph_bounded(
        r#"let quoted = quote^(text^($missing_callback_value)_text)_quote
autonomous(batch(
python^(
__oval_result__ = O.eval(quoted)
)_python
))
"#,
        2,
    );
    assert!(
        !run.output.status.success(),
        "a failed O.eval callback unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&run.output.stderr).contains("missing_callback_value"),
        "callback failure was not reported\nstderr:\n{}",
        String::from_utf8_lossy(&run.output.stderr)
    );
    let trace = fs::read_to_string(&run.trace_path).expect("read callback failure trace");
    assert!(trace.contains("outcome=semantic_failure"), "{trace}");
    #[cfg(unix)]
    assert_traced_backend_groups_quiescent(&run);
}

#[test]
fn autonomous_worker_nested_hosted_callback_inherits_deadline() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    let started = Instant::now();
    let run = run_graph_bounded_with_operation_timeout(
        r#"let quoted = quote^(python^(
import time
time.sleep(60)
__oval_result__ = "unreachable"
)_python)_quote
autonomous(batch(
python^(
__oval_result__ = O.eval(quoted)
)_python
))
"#,
        1,
        Some(Duration::from_millis(300)),
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "nested hosted callback exceeded its inherited deadline"
    );
    assert!(
        !run.output.status.success(),
        "a nonresponsive nested hosted callback unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(
        stderr.contains("inherited callback deadline")
            || stderr.contains("did not answer within")
            || stderr.contains("O.eval callback did not settle")
            || stderr.contains("callback reply channel disconnected"),
        "inherited callback timeout was not reported\nstderr:\n{stderr}"
    );
    let trace = fs::read_to_string(&run.trace_path).expect("read nested callback trace");
    assert!(trace.contains("outcome=infrastructure_failure"), "{trace}");
    #[cfg(unix)]
    assert_traced_backend_groups_quiescent(&run);
}

#[test]
fn autonomous_nonresponsive_backend_fails_within_configured_bound() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    let started = Instant::now();
    let run = run_graph_bounded_with_operation_timeout(
        r#"autonomous(batch(
python^(
import time
time.sleep(60)
__oval_result__ = "unreachable"
)_python
))
"#,
        1,
        Some(Duration::from_millis(200)),
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "nonresponsive backend exceeded the bounded failure window"
    );
    assert!(
        !run.output.status.success(),
        "a nonresponsive autonomous backend unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&run.output.stderr).contains("did not answer within"),
        "bounded timeout was not reported\nstderr:\n{}",
        String::from_utf8_lossy(&run.output.stderr)
    );
    let trace = fs::read_to_string(&run.trace_path).expect("read timeout lifecycle trace");
    assert!(trace.contains("outcome=infrastructure_failure"), "{trace}");
    #[cfg(unix)]
    assert_traced_backend_groups_quiescent(&run);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn autonomous_lingering_same_group_descendant_fails_and_group_becomes_quiescent() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    let started = Instant::now();
    let run = run_graph_bounded(
        r#"autonomous(batch(
python^(
import subprocess
subprocess.Popen(
    ["/bin/sleep", "60"],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
__oval_result__ = "parent-complete"
)_python
))
"#,
        1,
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "lingering backend descendant exceeded the bounded shutdown window"
    );
    assert!(
        !run.output.status.success(),
        "an autonomous backend with a lingering descendant unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(
        stderr.contains("still contains an active descendant")
            && stderr.contains("did not shut down cleanly"),
        "process-group shutdown failure was not reported\nstderr:\n{stderr}"
    );
    let trace = fs::read_to_string(&run.trace_path).expect("read descendant lifecycle trace");
    assert!(trace.contains("event=worker.done_received"), "{trace}");
    assert!(trace.contains("outcome=infrastructure_failure"), "{trace}");
    assert_traced_backend_groups_quiescent(&run);
}

#[test]
fn production_python_backend_proxy_shutdown_reaps_shim() {
    if !python_available() {
        eprintln!("skipping: python3 backend runtime is unavailable");
        return;
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workdir = tempfile::tempdir().expect("create proxy lifecycle directory");
    let trace_path = std::env::var_os("O_LIFECYCLE_TRACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| workdir.path().join("proxy-lifecycle.log"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_O"));
    command
        .arg("--o-backend")
        .arg("python")
        .env(
            "O_BACKEND_LEGACY_SHIM",
            root.join("backends/python_shim.py"),
        )
        .env("O_LIFECYCLE_TRACE", &trace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut proxy = command.spawn().expect("spawn production Python proxy");
    let proxy_pid = proxy.id();
    let stdin = proxy.stdin.take().expect("proxy stdin");
    let stdout = proxy.stdout.take().expect("proxy stdout");
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let interaction = thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            let mut writer = BufWriter::new(stdin);
            let mut reader = BufReader::new(stdout);
            wire::write_frame(
                &mut writer,
                &OWireCommand::Exec {
                    code: "__oval_result__ = O.eval(O.quote('text^(proxy-callback)_text'))"
                        .to_string(),
                    bindings: std::collections::HashMap::new(),
                },
            )?;
            let response =
                wire::read_frame::<_, OWireResponse>(&mut reader)?.expect("proxy Exec response");
            assert!(matches!(
                response,
                OWireResponse::EvalRequest { ref src, scope: None }
                    if src == "text^(proxy-callback)_text"
            ));
            wire::write_frame(
                &mut writer,
                &OWireCommand::EvalResult {
                    value: OValue::str_("proxy-ok"),
                },
            )?;
            let response = wire::read_frame::<_, OWireResponse>(&mut reader)?
                .expect("proxy callback completion response");
            assert!(matches!(
                response,
                OWireResponse::Ok { value } if value == OValue::str_("proxy-ok")
            ));

            wire::write_frame(&mut writer, &OWireCommand::Shutdown)?;
            let response = wire::read_frame::<_, OWireResponse>(&mut reader)?
                .expect("proxy Shutdown response");
            assert!(matches!(
                response,
                OWireResponse::Ok {
                    value: OValue::Null
                }
            ));
            Ok(())
        })()
        .map_err(|error| format!("{error:#}"));
        let _ = completed_tx.send(result);
    });

    let interaction_result = completed_rx.recv_timeout(Duration::from_secs(5));
    if !matches!(&interaction_result, Ok(Ok(()))) {
        #[cfg(unix)]
        if let Ok(group) = i32::try_from(proxy.id()) {
            // SAFETY: the proxy was created as the leader of this test-owned
            // process group.
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = proxy.kill();
    }
    interaction
        .join()
        .expect("proxy interaction thread did not panic");
    interaction_result
        .expect("proxy protocol exceeded five seconds")
        .expect("proxy protocol failed");
    let status = wait_for_child(&mut proxy, Duration::from_secs(5));
    assert!(status.success(), "proxy exited with {status}");

    let trace = fs::read_to_string(&trace_path).expect("read proxy lifecycle trace");
    let shim_pid = trace
        .lines()
        .find(|line| line.contains("event=proxy.shim_spawned"))
        .and_then(|line| field_pid(line, "shim_pid="))
        .unwrap_or_else(|| panic!("proxy trace did not record the shim PID\n{trace}"));
    assert!(
        trace.contains("event=proxy.shutdown_acknowledged"),
        "{trace}"
    );
    #[cfg(unix)]
    assert!(
        !process_or_group_exists(shim_pid),
        "Python shim {shim_pid} remained after proxy shutdown"
    );
    #[cfg(unix)]
    assert!(
        !process_or_group_exists(proxy_pid),
        "backend proxy process group {proxy_pid} remained after shutdown"
    );
}
