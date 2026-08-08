//! End-to-end coverage for explicitly autonomous hosted execution.
//!
//! These tests use externally observable files so they exercise the compiled
//! `O` CLI, the admitted HGraph, the persistent worker pool, and Python shim
//! callbacks together rather than merely inspecting a static schedule.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct RunOutcome {
    output: Output,
    workdir: tempfile::TempDir,
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_graph_bounded(source: &str) -> RunOutcome {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workdir = tempfile::tempdir().expect("create isolated test directory");
    let program = workdir.path().join("program.O");
    fs::write(&program, source).expect("write test program");

    let mut child = Command::new(env!("CARGO_BIN_EXE_O"))
        .env_remove("O_EXECUTOR")
        .arg("--executor")
        .arg("graph")
        .arg("--workers")
        .arg("2")
        .arg(&program)
        .arg(root.join("backends"))
        .current_dir(workdir.path())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("O_TEST_WORKDIR", workdir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start O CLI");

    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll O CLI") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
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
    }
}

fn assert_success(run: &RunOutcome, context: &str) {
    assert!(
        run.output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr),
    );
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

    let run = run_graph_bounded(
        r#"autonomous(batch(
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
"#,
    );
    assert_success(&run, "autonomous Python batch");

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
        "autonomous workers did not overlap: left={left:?}, right={right:?}"
    );
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
    );
    assert_success(&run, "autonomous worker O.eval callback");

    let observed = fs::read_to_string(run.workdir.path().join("callback.result"))
        .expect("autonomous callback result file");
    assert_eq!(observed, "lexical-scope-ok");
}
