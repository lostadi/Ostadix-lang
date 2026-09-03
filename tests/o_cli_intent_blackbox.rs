//! Compiled-binary acceptance boundaries for the unified intent front door.
//!
//! This file intentionally contains one test so its stateful subprocess cases
//! execute serially without adding a test-serialization dependency. Every
//! child receives a closed, deterministic environment rooted in its fixture.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::{engine::general_purpose::STANDARD, Engine};
use o_lang::project::ValidatedSelectionReceiptV1;
use serde_json::Value;
use sha2::{Digest, Sha256};

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

#[derive(Debug, Eq, PartialEq)]
enum TreeSnapshot {
    Missing,
    Present(BTreeMap<PathBuf, SnapshotEntry>),
}

#[derive(Debug, Eq, PartialEq)]
struct SnapshotEntry {
    kind: &'static str,
    mode: u32,
    bytes: Vec<u8>,
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn deterministic_path(extra: Option<&Path>) -> OsString {
    let mut entries = Vec::new();
    if let Some(extra) = extra {
        entries.push(extra.to_path_buf());
    }
    entries.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    std::env::join_paths(entries).unwrap()
}

fn o_cli(home: &Path, state: &Path, extra_path: Option<&Path>) -> Command {
    let temporary = home.join("tmp");
    fs::create_dir_all(home).unwrap();
    fs::create_dir_all(&temporary).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_STATE_HOME", state)
        .env(
            "O_BACKENDS_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
        )
        .env("PATH", deterministic_path(extra_path))
        .env("TMPDIR", temporary)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    command
}

fn run(command: &mut Command) -> Output {
    command.output().expect("launch compiled o-cli")
}

fn single_json(output: &Output) -> Value {
    let text = std::str::from_utf8(&output.stdout).expect("o-cli JSON stdout is UTF-8");
    assert_eq!(
        text.lines().filter(|line| !line.trim().is_empty()).count(),
        1,
        "expected exactly one compact JSON envelope, got stdout:\n{text}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not exactly one JSON value: {error}\nstdout:\n{text}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_summary_schema(summary: &Value) {
    assert_eq!(
        summary.get("schema").and_then(Value::as_str),
        Some("ostadix.run-summary/v1")
    );
}

fn snapshot_tree(root: &Path) -> TreeSnapshot {
    if !root.exists() {
        return TreeSnapshot::Missing;
    }
    let mut entries = BTreeMap::new();
    snapshot_entry(root, root, &mut entries);
    TreeSnapshot::Present(entries)
}

fn snapshot_entry(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, SnapshotEntry>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let relative = path.strip_prefix(root).unwrap().to_path_buf();
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode()
    };
    #[cfg(not(unix))]
    let mode = 0;

    if metadata.file_type().is_symlink() {
        entries.insert(
            relative,
            SnapshotEntry {
                kind: "symlink",
                mode,
                bytes: fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes(),
            },
        );
        return;
    }
    if metadata.is_file() {
        entries.insert(
            relative,
            SnapshotEntry {
                kind: "file",
                mode,
                bytes: fs::read(path).unwrap(),
            },
        );
        return;
    }

    entries.insert(
        relative,
        SnapshotEntry {
            kind: "directory",
            mode,
            bytes: Vec::new(),
        },
    );
    let mut children = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        snapshot_entry(root, &child, entries);
    }
}

fn ordinary_auto_no_record_is_local_only(root: &Path) {
    let home = root.join("home");
    let poisoned_state = root.join("state-is-a-regular-file");
    write(
        &poisoned_state,
        b"recording and discovery must not touch this",
    );
    let state_before = snapshot_tree(&poisoned_state);

    let poison_bin = root.join("poison-bin");
    let poison_node = poison_bin.join("o-node");
    let node_marker = root.join("o-node-was-invoked");
    write(
        &poison_node,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 97\n",
            node_marker.display()
        ),
    );
    make_executable(&poison_node);

    let program = root.join("ordinary-auto.O");
    write(&program, b"text^(local-auto-ok)_text\n");
    let output = run(o_cli(&home, &poisoned_state, Some(&poison_bin))
        .env("O_LANG_NODE_BIN", &poison_node)
        .args([
            "run",
            program.to_str().unwrap(),
            "--parallel",
            "auto",
            "--workers",
            "2",
            "--no-record",
            "--json",
        ]));
    assert!(
        output.status.success(),
        "ordinary local auto run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary = single_json(&output);
    assert_summary_schema(&summary);
    assert_eq!(summary["disposition"], "succeeded");
    assert_eq!(summary["recording"]["status"], "disabled");
    assert_eq!(summary["run_id"], Value::Null);
    assert!(!node_marker.exists(), "ordinary auto run invoked o-node");
    assert_eq!(
        snapshot_tree(&poisoned_state),
        state_before,
        "ordinary --no-record auto run changed poisoned XDG state"
    );
}

fn json_help_is_informational_not_a_failure_envelope(root: &Path) {
    for command in ["run", "optimize"] {
        let output = run(o_cli(&root.join("home"), &root.join("state"), None)
            .args([command, "--json", "--help"]));
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Usage:"),
            "{command} help was absent: {stdout}"
        );
        assert!(
            stdout.contains(&format!("Usage: o {command}")),
            "{command} help named the internal binary instead of the public front door: {stdout}"
        );
        if command == "optimize" {
            for boundary in [
                "executes the reference and every candidate",
                "requires durable run recording",
                "evidence-gathering invocation is not accelerated",
                "o run TARGET --selection-run RUN_ID",
            ] {
                assert!(
                    stdout.contains(boundary),
                    "optimize help omitted {boundary:?}: {stdout}"
                );
            }
        }
        assert!(
            !stdout.contains("ostadix.run-summary/v1")
                && !stdout.contains("ostadix.optimize-summary/v1"),
            "{command} help was incorrectly prefixed with a failure envelope: {stdout}"
        );
    }
    assert_eq!(snapshot_tree(&root.join("state")), TreeSnapshot::Missing);
}

fn ordinary_auto_overlaps_independent_oir_operations(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    let program = work.join("parallel.O");
    write(&program, INTERVAL_BATCH);
    let python = which::which("python3").expect("test host must provide python3");
    let python_bin = python
        .parent()
        .expect("python3 executable must have a parent directory");
    let output = run(o_cli(&home, &state, Some(python_bin))
        .current_dir(&work)
        .env("O_TEST_WORKDIR", &work)
        .args([
            "run",
            program.to_str().unwrap(),
            "--parallel",
            "auto",
            "--workers",
            "2",
            "--no-record",
        ]));
    assert!(
        output.status.success(),
        "parallel ordinary run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let interval = |name: &str| {
        let values = fs::read_to_string(work.join(name))
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse::<u128>().unwrap())
            .collect::<Vec<_>>();
        (values[0], values[1])
    };
    let left = interval("left.interval");
    let right = interval("right.interval");
    assert!(
        left.0 < right.1 && right.0 < left.1,
        "ordinary --parallel auto did not overlap independent operations: left={left:?}, right={right:?}"
    );
}

fn recorded_json_survives_source_deletion(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let program = root.join("recorded.O");
    write(&program, b"text^(recorded-ok)_text\n");
    let canonical_program = fs::canonicalize(&program).unwrap();

    let output = run(o_cli(&home, &state, None).args([
        "run",
        program.to_str().unwrap(),
        "--workers",
        "3",
        "--json",
    ]));
    assert!(
        output.status.success(),
        "recorded run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary = single_json(&output);
    assert_summary_schema(&summary);
    assert_eq!(summary["disposition"], "succeeded");
    assert_eq!(summary["recording"]["status"], "recorded");
    let run_id = summary["run_id"]
        .as_str()
        .expect("recorded summary omitted run id");
    assert_eq!(run_id.len(), 64);

    fs::remove_file(&program).unwrap();
    let inspection = run(o_cli(&home, &state, None).args(["inspect", "last-run", "--trace"]));
    assert!(
        inspection.status.success(),
        "read-only trace inspection failed after source deletion:\n{}",
        String::from_utf8_lossy(&inspection.stderr)
    );
    let observation: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(observation["state"], "terminal");
    assert_eq!(observation["record"]["run_id"], run_id);
    assert_eq!(observation["record"]["intent"]["local_worker_limit"], 3);
    assert_eq!(
        observation["record"]["input"]["path"],
        canonical_program.to_string_lossy().as_ref()
    );
    assert_eq!(observation["trace"]["schema"], "ostadix.run-trace/v1");
    assert!(
        !program.exists(),
        "inspection unexpectedly recreated the deleted source"
    );

    let foreign = root.join("unsupported.py");
    write(&foreign, b"print('bundle me first')\n");
    let preflight =
        run(o_cli(&home, &state, None).args(["run", foreign.to_str().unwrap(), "--json"]));
    assert!(!preflight.status.success());
    let preflight_summary = single_json(&preflight);
    assert_eq!(preflight_summary["disposition"], "preflight_failed");
    assert_eq!(preflight_summary["run_id"], Value::Null);
    assert_eq!(preflight_summary["input"], Value::Null);
    assert_eq!(preflight_summary["plan"], Value::Null);
    assert_eq!(preflight_summary["recording"]["status"], "not_started");

    let invalid_flag = run(o_cli(&home, &state, None).args([
        "run",
        foreign.to_str().unwrap(),
        "--definitely-invalid",
        "--json",
    ]));
    assert!(!invalid_flag.status.success());
    assert_eq!(
        single_json(&invalid_flag)["disposition"],
        "preflight_failed"
    );

    let still_last = run(o_cli(&home, &state, None).args(["inspect", "last-run"]));
    assert!(still_last.status.success());
    let still_last: Value = serde_json::from_slice(&still_last.stdout).unwrap();
    assert_eq!(
        still_last["record"]["run_id"], run_id,
        "preflight failure changed last-run"
    );
}

fn inherited_backend_stderr_is_fully_bound_in_the_record(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let shims = root.join("empty-shims");
    fs::create_dir_all(&shims).unwrap();
    let program = root.join("missing-python-shim.O");
    write(&program, b"python^(\nprint(2)\n)_python\n");
    let python = which::which("python3").expect("test host must provide python3");
    let python_bin = python
        .parent()
        .expect("python3 executable must have a parent directory");

    let output = run(o_cli(&home, &state, Some(python_bin)).args([
        "run",
        program.to_str().unwrap(),
        "--shim-dir",
        shims.to_str().unwrap(),
        "--json",
    ]));
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("legacy shim path"),
        "backend's inherited stderr was absent: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    single_json(&output);

    let inspection =
        run(o_cli(&home, &state, Some(python_bin)).args(["inspect", "last-run", "--trace"]));
    assert!(inspection.status.success());
    let observation: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    let stderr = &observation["record"]["stderr"];
    let retained = STANDARD
        .decode(stderr["retained"].as_str().unwrap())
        .unwrap();
    assert_eq!(retained, output.stderr);
    assert_eq!(
        stderr["capture"]["total_observed_bytes"],
        output.stderr.len() as u64
    );
    assert_eq!(stderr["capture"]["truncated"], false);
    assert_eq!(
        stderr["capture"]["sha256"],
        hex::encode(Sha256::digest(&output.stderr))
    );
}

fn inherited_runtime_stdout_cannot_prefix_json(root: &Path) {
    let Ok(python) = which::which("python3") else {
        eprintln!("SKIP-OPTIONAL: python3 is unavailable");
        return;
    };
    let home = root.join("home");
    let state = root.join("state");
    let closure = root.join("system");
    fs::create_dir_all(closure.join("bin")).unwrap();
    let sentinel = "INHERITED_STDOUT_MUST_BE_CAPTURED";
    let switch = closure.join("bin/switch-to-configuration");
    write(&switch, format!("#!/bin/sh\nprintf '%s\\n' '{sentinel}'\n"));
    make_executable(&switch);
    let program = root.join("activation.O");
    write(
        &program,
        format!(
            "let system = python^(__oval_result__ = OStorePath(r{closure:?}))_python\ndry_activate($system)\n",
            closure = closure.display().to_string(),
        ),
    );
    let python_bin = python.parent().unwrap();

    let output = run(o_cli(&home, &state, Some(python_bin)).args([
        "run",
        program.to_str().unwrap(),
        "--executor",
        "serial",
        "--json",
    ]));
    assert!(
        output.status.success(),
        "activation run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let summary = single_json(&output);
    assert_eq!(summary["disposition"], "succeeded");
    assert!(
        !output
            .stdout
            .windows(sentinel.len())
            .any(|bytes| bytes == sentinel.as_bytes()),
        "inherited runtime stdout prefixed the JSON envelope"
    );

    let inspection = run(o_cli(&home, &state, Some(python_bin)).args(["inspect", "last-run"]));
    assert!(inspection.status.success());
    let observation: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    let retained = STANDARD
        .decode(
            observation["record"]["stdout"]["retained"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    assert!(
        retained
            .windows(sentinel.len())
            .any(|bytes| bytes == sentinel.as_bytes()),
        "inherited stdout was suppressed from the durable execution observation"
    );

    let state_before = snapshot_tree(&state);
    let no_record = run(o_cli(&home, &state, Some(python_bin)).args([
        "run",
        program.to_str().unwrap(),
        "--executor",
        "serial",
        "--no-record",
        "--json",
    ]));
    assert!(no_record.status.success());
    assert_eq!(single_json(&no_record)["recording"]["status"], "disabled");
    assert!(
        !no_record
            .stdout
            .windows(sentinel.len())
            .any(|bytes| bytes == sentinel.as_bytes()),
        "--no-record JSON was prefixed by inherited runtime stdout"
    );
    assert_eq!(snapshot_tree(&state), state_before);
}

fn ordinary_rendered_output_over_one_mib_is_recorded_losslessly(root: &Path) {
    let Ok(python) = which::which("python3") else {
        eprintln!("SKIP-OPTIONAL: python3 is unavailable");
        return;
    };
    let home = root.join("home");
    let state = root.join("state");
    let program = root.join("large-text.O");
    let payload_len = 1024 * 1024 + 8192;
    let payload = vec![b'x'; payload_len];
    write(
        &program,
        format!("python^(__oval_result__ = 'x' * {payload_len})_python\n"),
    );

    let output = run(o_cli(&home, &state, python.parent()).args([
        "run",
        program.to_str().unwrap(),
        "--json",
    ]));
    assert!(
        output.status.success(),
        "large ordinary output failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(single_json(&output)["recording"]["status"], "recorded");

    let inspection = run(o_cli(&home, &state, None).args(["inspect", "last-run"]));
    assert!(inspection.status.success());
    let observation: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    let stdout = &observation["record"]["stdout"];
    let retained = STANDARD
        .decode(stdout["retained"].as_str().unwrap())
        .unwrap();
    assert_eq!(retained, payload, "recording added a second truncation");
    assert_eq!(stdout["capture"]["retained_bytes"], payload.len() as u64);
    assert_eq!(
        stdout["capture"]["total_observed_bytes"],
        payload.len() as u64
    );
    assert_eq!(stdout["capture"]["truncated"], false);
    assert_eq!(
        stdout["capture"]["sha256"],
        hex::encode(Sha256::digest(&payload))
    );
}

fn semantic_failure_is_one_nonzero_json_envelope(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let program = root.join("semantic-failure.O");
    write(&program, b"$definitely_missing_binding\n");

    let output = run(o_cli(&home, &state, None).args(["run", program.to_str().unwrap(), "--json"]));
    assert!(
        !output.status.success(),
        "semantic evaluator failure unexpectedly exited zero"
    );
    let summary = single_json(&output);
    assert_summary_schema(&summary);
    assert_eq!(summary["disposition"], "execution_failed");
    assert_eq!(summary["recording"]["status"], "recorded");
}

fn failed_route_command_arguments_are_not_persisted(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let secret = "TOKEN_super_secret_987";
    fs::create_dir_all(&project).unwrap();
    write(
        &project.join("olang.project.toml"),
        format!(
            r#"
[project]
name = "credential-safe-diagnostics"
default_route = "main"

[[routes]]
id = "main"
command = ["definitely-missing-{secret}", "--credential", "{secret}"]
default = true
"#
        ),
    );

    let output = run(o_cli(&home, &state, None)
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .args(["run", project.to_str().unwrap(), "--json"]));
    assert!(!output.status.success());
    assert!(
        !output
            .stderr
            .windows(secret.len())
            .any(|bytes| bytes == secret.as_bytes()),
        "failed route argv leaked through emitted diagnostics: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to spawn route `main` command"),
        "sanitized diagnostic lost the failure stage: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let inspection = run(o_cli(&home, &state, None).args(["inspect", "last-run", "--trace"]));
    assert!(inspection.status.success());
    assert!(
        !inspection
            .stdout
            .windows(secret.len())
            .any(|bytes| bytes == secret.as_bytes()),
        "credential appeared in verified JSON inspection"
    );
    let state_snapshot = snapshot_tree(&state);
    let TreeSnapshot::Present(entries) = state_snapshot else {
        panic!("recorded failure did not create a run store")
    };
    assert!(
        entries.values().all(|entry| {
            !entry
                .bytes
                .windows(secret.len())
                .any(|bytes| bytes == secret.as_bytes())
        }),
        "credential appeared in durable run-store bytes"
    );
}

fn validated_selection_receipt_is_bound_and_durable(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let receipt_path = root.join("validated-selection.json");
    let secret = "TOKEN_receipt_secret_987";
    let shell = which::which("sh").expect("test host must provide sh");
    let shell_bin = shell.parent().expect("sh must have a parent directory");
    write(
        &project.join("olang.project.toml"),
        format!(
            r#"
[project]
name = "validated-selection-cli"

[[routes]]
id = "reference"
command = ["sh", "-c", "sleep 0.3; printf same", "{secret}"]

[[routes]]
id = "safe"
command = ["sh", "-c", "sleep 0.1; printf same", "{secret}"]

[[routes]]
id = "divergent-json"
command = ["sh", "-c", "printf '{{\"wrong\":true}}'", "{secret}"]
result_codec = "json"

[[route_sets]]
provides = "main"
alternatives = ["reference", "safe", "divergent-json"]
policy = "benchmark_validate_and_select"
"#
        ),
    );

    let invoke = |no_record: bool| {
        let mut command = o_cli(&home, &state, Some(shell_bin));
        command.args([
            "run",
            project.to_str().unwrap(),
            "--project",
            "--route",
            "main",
            "--routes-policy",
            "benchmark_validate_and_select",
            "--selection-receipt-out",
            receipt_path.to_str().unwrap(),
            "--json",
        ]);
        if no_record {
            command.arg("--no-record");
        }
        run(&mut command)
    };

    let first = invoke(false);
    assert!(
        first.status.success(),
        "validated selection CLI run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let summary = single_json(&first);
    assert_summary_schema(&summary);
    assert_eq!(summary["disposition"], "succeeded");
    assert_eq!(summary["recording"]["status"], "recorded");

    let receipt_bytes = fs::read(&receipt_path).expect("selection receipt was not written");
    assert!(
        !receipt_bytes
            .windows(secret.len())
            .any(|window| window == secret.as_bytes()),
        "route argv leaked into the validated-selection receipt"
    );
    let receipt: ValidatedSelectionReceiptV1 = serde_json::from_slice(&receipt_bytes).unwrap();
    receipt.validate().unwrap();
    assert_eq!(receipt.reference_route_id, "reference");
    assert_eq!(receipt.selected_route_id, "safe");
    assert_eq!(
        receipt
            .candidates
            .iter()
            .map(|candidate| candidate.route_id.as_str())
            .collect::<Vec<_>>(),
        ["reference", "safe", "divergent-json"]
    );
    assert_eq!(
        receipt.sha256().unwrap(),
        hex::encode(Sha256::digest(&receipt_bytes)),
        "reported receipt identity must equal sha256sum of emitted bytes"
    );
    assert_eq!(
        receipt.candidates[2].disposition,
        o_lang::project::ValidatedSelectionDispositionV1::RejectedOutput {
            mismatch: o_lang::project::ValidatedSelectionMismatchV1::ResultCodec,
        }
    );

    let inspection = run(o_cli(&home, &state, Some(shell_bin)).args(["inspect", "last-run"]));
    assert!(
        inspection.status.success(),
        "could not inspect validated-selection run: {}",
        String::from_utf8_lossy(&inspection.stderr)
    );
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(
        inspection["record"]["validated_selection_receipt"],
        serde_json::to_value(&receipt).unwrap()
    );
    let recorded_routes = inspection["record"]["route_results"]
        .as_array()
        .expect("validated selection must retain route results");
    for candidate in &receipt.candidates {
        let recorded = recorded_routes
            .iter()
            .find(|result| result["route_id"] == candidate.route_id)
            .unwrap_or_else(|| panic!("missing recorded candidate {}", candidate.route_id));
        assert_eq!(
            recorded["result_codec"],
            serde_json::to_value(candidate.observation.result_codec).unwrap()
        );
        assert_eq!(recorded["duration_ns"], candidate.terminal_elapsed_ns);
        assert_eq!(recorded["branch_elapsed_ns"], candidate.branch_elapsed_ns);
    }
    assert_eq!(
        inspection["record"]["route_results"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["route_id"],
        "safe"
    );
    assert_eq!(inspection["record"]["decoded_value"], Value::Null);

    let first_bundle = receipt.bundle_sha256.clone();
    let second = invoke(true);
    assert!(
        second.status.success(),
        "second validated selection CLI run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(single_json(&second)["recording"]["status"], "disabled");
    let second_receipt: ValidatedSelectionReceiptV1 =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(
        second_receipt.bundle_sha256, first_bundle,
        "an excluded receipt output changed the next bundle identity"
    );
}

fn optimize_command_is_readable_structured_and_durable(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let receipt_path = root.join("optimized-selection.json");
    let shell = which::which("sh").expect("test host must provide sh");
    let shell_bin = shell.parent().expect("sh must have a parent directory");
    write(
        &project.join("olang.project.toml"),
        r#"
[project]
name = "optimize-cli"

[[routes]]
id = "reference"
command = ["sh", "-c", "sleep 0.80; printf RAW_OPTIMIZE_MATCH_9137"]

[[routes]]
id = "fast"
command = ["sh", "-c", "sleep 0.01; printf RAW_OPTIMIZE_MATCH_9137"]

[[routes]]
id = "divergent"
command = ["sh", "-c", "printf RAW_OPTIMIZE_DIVERGENT_9137"]

[[route_sets]]
provides = "main"
alternatives = ["reference", "fast", "divergent"]
policy = "all"
"#,
    );

    let human = run(o_cli(&home, &state, Some(shell_bin)).args([
        "optimize",
        project.to_str().unwrap(),
        "--route",
        "main",
        "--receipt",
        receipt_path.to_str().unwrap(),
    ]));
    assert!(
        human.status.success(),
        "human optimize failed: {}",
        String::from_utf8_lossy(&human.stderr)
    );
    let rendered = String::from_utf8(human.stdout).unwrap();
    let reference = rendered.find("- reference [reference]").unwrap();
    let fast = rendered.find("- fast [selected]").unwrap();
    let divergent = rendered
        .find("- divergent - rejected: complete stdout differs")
        .unwrap();
    assert!(reference < fast && fast < divergent);
    assert!(rendered.contains("Ostadix optimization evidence"));
    assert!(rendered.contains("Selected route: fast"));
    assert!(rendered.contains("Measured complete-branch ratio versus reference:"));
    assert!(rendered.contains("Declared-output contract:"));
    assert!(rendered.contains("Durable evidence: o inspect "));
    assert!(rendered.contains("Receipt export path:"));
    assert!(rendered.contains("every candidate ran"));
    assert!(rendered.contains("was not accelerated"));
    assert!(
        !rendered.contains("RAW_OPTIMIZE_MATCH_9137")
            && !rendered.contains("RAW_OPTIMIZE_DIVERGENT_9137"),
        "raw candidate output escaped into the optimize UI: {rendered}"
    );

    let receipt_bytes = fs::read(&receipt_path).expect("optimize receipt was not exported");
    let receipt: ValidatedSelectionReceiptV1 = serde_json::from_slice(&receipt_bytes).unwrap();
    receipt.validate().unwrap();
    assert_eq!(receipt.reference_route_id, "reference");
    assert_eq!(receipt.selected_route_id, "fast");
    let receipt_sha256 = hex::encode(Sha256::digest(&receipt_bytes));
    assert!(rendered.contains(&format!("Receipt SHA-256: {receipt_sha256}")));

    let inspection = run(o_cli(&home, &state, Some(shell_bin)).args(["inspect", "last-run"]));
    assert!(
        inspection.status.success(),
        "could not inspect optimize evidence: {}",
        String::from_utf8_lossy(&inspection.stderr)
    );
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(
        inspection["record"]["validated_selection_receipt"],
        serde_json::to_value(&receipt).unwrap()
    );

    let json = run(o_cli(&home, &state, Some(shell_bin)).args([
        "optimize",
        project.to_str().unwrap(),
        "--route",
        "main",
        "--json",
    ]));
    assert!(
        json.status.success(),
        "JSON optimize failed: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let envelope = single_json(&json);
    assert_eq!(envelope["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(envelope["run"]["schema"], "ostadix.run-summary/v1");
    assert_eq!(envelope["run"]["disposition"], "succeeded");
    assert_eq!(envelope["run"]["recording"]["status"], "recorded");
    assert_eq!(envelope["receipt"]["reference_route_id"], "reference");
    assert_eq!(envelope["receipt"]["selected_route_id"], "fast");
    assert_eq!(envelope["receipt_export_path"], Value::Null);
    let embedded_receipt: ValidatedSelectionReceiptV1 =
        serde_json::from_value(envelope["receipt"].clone()).unwrap();
    embedded_receipt.validate().unwrap();
    assert_eq!(
        envelope["receipt_sha256"],
        embedded_receipt.sha256().unwrap()
    );

    let impossible_export = root.join("receipt-path-is-a-directory");
    fs::create_dir(&impossible_export).unwrap();
    let export_failure = run(o_cli(&home, &state, Some(shell_bin)).args([
        "optimize",
        project.to_str().unwrap(),
        "--route",
        "main",
        "--receipt",
        impossible_export.to_str().unwrap(),
        "--json",
    ]));
    assert!(!export_failure.status.success());
    let export_failure = single_json(&export_failure);
    assert_eq!(
        export_failure["run"]["disposition"],
        "infrastructure_failed"
    );
    assert_eq!(export_failure["run"]["recording"]["status"], "recorded");
    assert!(export_failure["receipt"].is_object());
    assert!(export_failure["receipt_sha256"].is_string());
    assert_eq!(export_failure["receipt_export_path"], Value::Null);
    assert!(impossible_export.is_dir());

    let malformed = run(o_cli(&home, &state, Some(shell_bin)).args([
        "optimize",
        project.to_str().unwrap(),
        "--json",
    ]));
    assert!(!malformed.status.success());
    let failure = single_json(&malformed);
    assert_eq!(failure["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(failure["run"]["disposition"], "preflight_failed");
    assert_eq!(failure["run"]["run_id"], Value::Null);
    assert_eq!(failure["run"]["input"], Value::Null);
    assert_eq!(failure["run"]["plan"], Value::Null);
    assert_eq!(failure["run"]["recording"]["status"], "not_started");
    assert_eq!(failure["receipt"], Value::Null);
    assert_eq!(failure["receipt_sha256"], Value::Null);
    assert_eq!(failure["receipt_export_path"], Value::Null);
}

#[cfg(unix)]
fn optimize_rejects_non_utf8_target_without_execution_or_state(root: &Path) {
    use std::os::unix::ffi::OsStringExt;

    let home = root.join("home");
    let state = root.join("state");
    let marker = root.join("must-not-execute");
    let project = root.join(OsString::from_vec(b"project-\xff".to_vec()));
    write(
        &project.join("olang.project.toml"),
        format!(
            r#"
[project]
name = "non-utf8-target"

[[routes]]
id = "reference"
command = ["sh", "-c", "printf executed > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[routes]]
id = "candidate"
command = ["sh", "-c", "printf executed > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[route_sets]]
provides = "main"
alternatives = ["reference", "candidate"]
policy = "all"
"#,
            marker.display(),
            marker.display(),
        ),
    );

    let output = run(o_cli(&home, &state, None)
        .arg("optimize")
        .arg(&project)
        .args(["--route", "main", "--json"]));
    assert!(!output.status.success());
    let envelope = single_json(&output);
    assert_eq!(envelope["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(envelope["run"]["disposition"], "preflight_failed");
    assert_eq!(envelope["run"]["run_id"], Value::Null);
    assert_eq!(envelope["run"]["input"], Value::Null);
    assert_eq!(envelope["run"]["plan"], Value::Null);
    assert_eq!(envelope["run"]["recording"]["status"], "not_started");
    assert_eq!(envelope["receipt"], Value::Null);
    assert_eq!(envelope["receipt_sha256"], Value::Null);
    assert_eq!(envelope["receipt_export_path"], Value::Null);
    assert!(!marker.exists(), "non-UTF-8 target unexpectedly executed");
    assert_eq!(snapshot_tree(&state), TreeSnapshot::Missing);
}

#[cfg(not(unix))]
fn optimize_rejects_non_utf8_target_without_execution_or_state(_root: &Path) {}

fn optimize_rejects_a_direct_route_before_execution(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let marker = root.join("must-not-execute");
    write(
        &project.join("olang.project.toml"),
        format!(
            r#"
[project]
name = "optimize-direct-route"

[[routes]]
id = "direct"
command = ["sh", "-c", "printf executed > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}
"#,
            marker.display()
        ),
    );

    let output = run(o_cli(&home, &state, None).args([
        "optimize",
        project.to_str().unwrap(),
        "--route",
        "direct",
        "--json",
    ]));
    assert!(!output.status.success());
    let envelope = single_json(&output);
    assert_eq!(envelope["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(envelope["run"]["disposition"], "preflight_failed");
    assert_eq!(envelope["run"]["recording"]["status"], "not_started");
    assert_eq!(envelope["receipt"], Value::Null);
    assert!(
        !marker.exists(),
        "direct route ran despite failed preflight"
    );
}

fn optimize_reference_failure_is_recorded_without_a_selection(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    write(
        &project.join("olang.project.toml"),
        r#"
[project]
name = "optimize-reference-failure"

[[routes]]
id = "reference"
command = ["sh", "-c", "exit 9"]

[[routes]]
id = "candidate"
command = ["sh", "-c", "printf candidate"]

[[route_sets]]
provides = "main"
alternatives = ["reference", "candidate"]
policy = "all"
"#,
    );

    let output = run(o_cli(&home, &state, None).args([
        "optimize",
        project.to_str().unwrap(),
        "--route",
        "main",
        "--json",
    ]));
    assert!(!output.status.success());
    let envelope = single_json(&output);
    assert_eq!(envelope["schema"], "ostadix.optimize-summary/v1");
    assert_ne!(envelope["run"]["disposition"], "succeeded");
    assert_eq!(envelope["run"]["recording"]["status"], "recorded");
    assert!(envelope["run"]["run_id"].is_string());
    assert_eq!(envelope["receipt"], Value::Null);
    assert_eq!(envelope["receipt_sha256"], Value::Null);

    let inspection = run(o_cli(&home, &state, None).args(["inspect", "last-run"]));
    assert!(inspection.status.success());
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_ne!(inspection["record"]["disposition"], "succeeded");
    assert_eq!(
        inspection["record"]["validated_selection_receipt"],
        Value::Null
    );
}

#[cfg(unix)]
fn optimize_requires_recording_before_execution(root: &Path) {
    use std::os::unix::fs::symlink;

    let home = root.join("home");
    let state = root.join("state");
    let ostadix_state = state.join("ostadix");
    let redirected_store = root.join("redirected-store");
    fs::create_dir_all(&ostadix_state).unwrap();
    fs::create_dir_all(&redirected_store).unwrap();
    symlink(&redirected_store, ostadix_state.join("runs-v1")).unwrap();

    let project = root.join("project");
    let marker = root.join("must-not-execute");
    let receipt_path = root.join("must-not-exist.json");
    write(
        &project.join("olang.project.toml"),
        format!(
            r#"
[project]
name = "optimize-required-record"

[[routes]]
id = "reference"
command = ["sh", "-c", "printf executed > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[routes]]
id = "candidate"
command = ["sh", "-c", "printf executed > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[route_sets]]
provides = "main"
alternatives = ["reference", "candidate"]
policy = "all"
"#,
            marker.display(),
            marker.display(),
        ),
    );

    let output = run(o_cli(&home, &state, None).args([
        "optimize",
        project.to_str().unwrap(),
        "--route",
        "main",
        "--receipt",
        receipt_path.to_str().unwrap(),
        "--json",
    ]));
    assert!(!output.status.success());
    let envelope = single_json(&output);
    assert_eq!(envelope["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(envelope["run"]["disposition"], "infrastructure_failed");
    assert_eq!(envelope["run"]["recording"]["status"], "incomplete");
    assert_eq!(envelope["receipt"], Value::Null);
    assert_eq!(envelope["receipt_sha256"], Value::Null);
    assert_eq!(envelope["receipt_export_path"], Value::Null);
    assert!(
        !marker.exists(),
        "candidate ran before recording was available"
    );
    assert!(!receipt_path.exists());
}

#[cfg(not(unix))]
fn optimize_requires_recording_before_execution(_root: &Path) {}

fn validated_selection_receipt_preflight_is_fail_closed(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let shell = which::which("sh").expect("test host must provide sh");
    let shell_bin = shell.parent().expect("sh must have a parent directory");

    let ordinary = root.join("ordinary.O");
    let ordinary_receipt = root.join("ordinary-receipt.json");
    write(&ordinary, b"text^(ordinary)_text\n");
    let ordinary_output = run(o_cli(&home, &state, Some(shell_bin)).args([
        "run",
        ordinary.to_str().unwrap(),
        "--selection-receipt-out",
        ordinary_receipt.to_str().unwrap(),
        "--no-record",
        "--json",
    ]));
    assert!(!ordinary_output.status.success());
    assert_eq!(
        single_json(&ordinary_output)["disposition"],
        "preflight_failed"
    );
    assert!(!ordinary_receipt.exists());

    let marker = root.join("must-not-execute");
    let project = root.join("project");
    let manifest_path = project.join("olang.project.toml");
    let manifest = format!(
        r#"
[project]
name = "validated-selection-preflight"

[[routes]]
id = "reference"
command = ["sh", "-c", "printf executed > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[routes]]
id = "candidate"
command = ["sh", "-c", "printf executed > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[route_sets]]
provides = "main"
alternatives = ["reference", "candidate"]
policy = "benchmark_validate_and_select"
"#,
        marker.display(),
        marker.display()
    );
    write(&manifest_path, &manifest);

    let input_overwrite = run(o_cli(&home, &state, Some(shell_bin)).args([
        "run",
        project.to_str().unwrap(),
        "--project",
        "--route",
        "main",
        "--selection-receipt-out",
        manifest_path.to_str().unwrap(),
        "--no-record",
        "--json",
    ]));
    assert!(!input_overwrite.status.success());
    assert_eq!(
        single_json(&input_overwrite)["disposition"],
        "preflight_failed"
    );
    assert_eq!(fs::read_to_string(&manifest_path).unwrap(), manifest);
    assert!(!marker.exists());

    let aliased_output = root.join("aliased-output.json");
    let aliased_outputs = run(o_cli(&home, &state, Some(shell_bin))
        .current_dir(root)
        .args([
            "run",
            project.to_str().unwrap(),
            "--project",
            "--route",
            "main",
            "--mesh-trace-out",
            aliased_output.to_str().unwrap(),
            "--selection-receipt-out",
            "aliased-output.json",
            "--no-record",
            "--json",
        ]));
    assert!(!aliased_outputs.status.success());
    assert_eq!(
        single_json(&aliased_outputs)["disposition"],
        "preflight_failed"
    );
    assert!(!aliased_output.exists());
    assert!(!marker.exists());

    let wrong_receipt = root.join("wrong-policy.json");
    let wrong_policy = run(o_cli(&home, &state, Some(shell_bin)).args([
        "run",
        project.to_str().unwrap(),
        "--project",
        "--route",
        "main",
        "--routes-policy",
        "all",
        "--selection-receipt-out",
        wrong_receipt.to_str().unwrap(),
        "--no-record",
        "--json",
    ]));
    assert!(!wrong_policy.status.success());
    assert_eq!(
        single_json(&wrong_policy)["disposition"],
        "preflight_failed"
    );
    assert!(!wrong_receipt.exists());
    assert!(!marker.exists());

    let hgraph_receipt = root.join("hgraph.json");
    let hgraph = run(o_cli(&home, &state, Some(shell_bin))
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .args([
            "run",
            project.to_str().unwrap(),
            "--project",
            "--route",
            "main",
            "--selection-receipt-out",
            hgraph_receipt.to_str().unwrap(),
            "--no-record",
            "--json",
        ]));
    assert!(!hgraph.status.success());
    assert_eq!(single_json(&hgraph)["disposition"], "preflight_failed");
    assert!(!hgraph_receipt.exists());
    assert!(!marker.exists());
}

fn no_record_preserves_absent_and_existing_state(root: &Path) {
    let home = root.join("home");
    let program = root.join("no-record.O");
    write(&program, b"text^(no-record-ok)_text\n");

    let absent = root.join("absent-state");
    assert_eq!(snapshot_tree(&absent), TreeSnapshot::Missing);
    let absent_run = run(o_cli(&home, &absent, None).args([
        "run",
        program.to_str().unwrap(),
        "--no-record",
        "--json",
    ]));
    assert!(
        absent_run.status.success(),
        "no-record run with absent state failed:\n{}",
        String::from_utf8_lossy(&absent_run.stderr)
    );
    assert_eq!(
        snapshot_tree(&absent),
        TreeSnapshot::Missing,
        "--no-record created an absent XDG state root"
    );

    let existing = root.join("existing-state");
    write(
        &existing.join("ostadix/unrelated/opaque.bin"),
        [0_u8, 255, 17, 23, 42],
    );
    write(&existing.join("keep.txt"), b"preserve me exactly\n");
    let before = snapshot_tree(&existing);
    let existing_run = run(o_cli(&home, &existing, None).args([
        "run",
        program.to_str().unwrap(),
        "--no-record",
        "--json",
    ]));
    assert!(
        existing_run.status.success(),
        "no-record run with existing state failed:\n{}",
        String::from_utf8_lossy(&existing_run.stderr)
    );
    assert_eq!(
        snapshot_tree(&existing),
        before,
        "--no-record changed preexisting XDG state bytes or permissions"
    );
}

fn missing_default_state_base_never_writes_history_into_cwd(root: &Path) {
    let cwd = root.join("worktree");
    let temporary = root.join("tmp");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&temporary).unwrap();
    let program = cwd.join("ordinary.O");
    write(&program, b"text^(location-contract)_text\n");

    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    let output = run(command
        .env_clear()
        .current_dir(&cwd)
        .env(
            "O_BACKENDS_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
        )
        .env("PATH", deterministic_path(None))
        .env("TMPDIR", &temporary)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .args([
            "run",
            program.to_str().unwrap(),
            "--require-record",
            "--json",
        ]));
    assert!(!output.status.success());
    let summary = single_json(&output);
    assert_eq!(summary["run_id"], Value::Null);
    assert_eq!(summary["disposition"], "infrastructure_failed");
    assert_eq!(summary["recording"]["status"], "incomplete");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("set XDG_STATE_HOME or HOME"),
        "missing-base diagnostic was not explicit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!cwd.join(".ostadix-state").exists());
    assert!(!cwd.join("ostadix").exists());
}

#[cfg(unix)]
fn require_record_begin_failure_prevents_execution(root: &Path) {
    use std::os::unix::fs::symlink;

    let home = root.join("home");
    let state = root.join("state");
    let ostadix_state = state.join("ostadix");
    let redirected_store = root.join("redirected-store");
    fs::create_dir_all(&ostadix_state).unwrap();
    fs::create_dir_all(&redirected_store).unwrap();
    symlink(&redirected_store, ostadix_state.join("runs-v1")).unwrap();

    let marker = root.join("must-not-execute");
    let program = root.join("marker.O");
    write(
        &program,
        format!(
            "bash^(\nprintf executed > '{}'\nprintf result\n)_bash\n",
            marker.display()
        ),
    );
    let output = run(o_cli(&home, &state, None).args([
        "run",
        program.to_str().unwrap(),
        "--require-record",
        "--json",
    ]));
    assert!(
        !output.status.success(),
        "unsafe required-record store unexpectedly exited zero"
    );
    let summary = single_json(&output);
    assert_summary_schema(&summary);
    assert_eq!(summary["run_id"], Value::Null);
    assert_eq!(summary["disposition"], "infrastructure_failed");
    assert_eq!(summary["recording"]["status"], "incomplete");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no computation was executed"),
        "required-record diagnostic did not state the pre-execution boundary: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !marker.exists(),
        "program executed after required recording failed to begin"
    );
}

#[cfg(not(unix))]
fn require_record_begin_failure_prevents_execution(_root: &Path) {}

#[test]
fn compiled_o_cli_black_box_contracts() {
    let root = tempfile::tempdir().unwrap();

    ordinary_auto_no_record_is_local_only(&root.path().join("local-auto"));
    json_help_is_informational_not_a_failure_envelope(&root.path().join("json-help"));
    ordinary_auto_overlaps_independent_oir_operations(&root.path().join("local-overlap"));
    recorded_json_survives_source_deletion(&root.path().join("recorded"));
    inherited_backend_stderr_is_fully_bound_in_the_record(&root.path().join("stderr-capture"));
    inherited_runtime_stdout_cannot_prefix_json(&root.path().join("stdout-capture"));
    ordinary_rendered_output_over_one_mib_is_recorded_losslessly(&root.path().join("large-output"));
    semantic_failure_is_one_nonzero_json_envelope(&root.path().join("semantic-failure"));
    failed_route_command_arguments_are_not_persisted(&root.path().join("credential-redaction"));
    validated_selection_receipt_is_bound_and_durable(&root.path().join("validated-selection"));
    optimize_command_is_readable_structured_and_durable(&root.path().join("optimize"));
    optimize_rejects_non_utf8_target_without_execution_or_state(
        &root.path().join("optimize-non-utf8"),
    );
    optimize_rejects_a_direct_route_before_execution(&root.path().join("optimize-direct"));
    optimize_reference_failure_is_recorded_without_a_selection(
        &root.path().join("optimize-failure"),
    );
    optimize_requires_recording_before_execution(&root.path().join("optimize-recording"));
    validated_selection_receipt_preflight_is_fail_closed(
        &root.path().join("validated-selection-preflight"),
    );
    no_record_preserves_absent_and_existing_state(&root.path().join("no-record"));
    missing_default_state_base_never_writes_history_into_cwd(
        &root.path().join("missing-state-base"),
    );
    require_record_begin_failure_prevents_execution(&root.path().join("require-record"));
}
