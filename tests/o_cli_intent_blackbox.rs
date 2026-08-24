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

fn json_run_help_is_informational_not_a_failure_envelope(root: &Path) {
    let output =
        run(o_cli(&root.join("home"), &root.join("state"), None).args(["run", "--json", "--help"]));
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"), "run help was absent: {stdout}");
    assert!(
        !stdout.contains("ostadix.run-summary/v1"),
        "help was incorrectly prefixed with a preflight-failure envelope: {stdout}"
    );
    assert_eq!(snapshot_tree(&root.join("state")), TreeSnapshot::Missing);
}

fn ordinary_auto_overlaps_independent_oir_operations(root: &Path) {
    let home = root.join("home");
    let state = root.join("state");
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    let program = work.join("parallel.O");
    write(&program, INTERVAL_BATCH);
    let output = run(o_cli(&home, &state, None)
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

    let output = run(o_cli(&home, &state, None).args([
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

    let inspection = run(o_cli(&home, &state, None).args(["inspect", "last-run", "--trace"]));
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
    json_run_help_is_informational_not_a_failure_envelope(&root.path().join("json-help"));
    ordinary_auto_overlaps_independent_oir_operations(&root.path().join("local-overlap"));
    recorded_json_survives_source_deletion(&root.path().join("recorded"));
    inherited_backend_stderr_is_fully_bound_in_the_record(&root.path().join("stderr-capture"));
    inherited_runtime_stdout_cannot_prefix_json(&root.path().join("stdout-capture"));
    ordinary_rendered_output_over_one_mib_is_recorded_losslessly(&root.path().join("large-output"));
    semantic_failure_is_one_nonzero_json_envelope(&root.path().join("semantic-failure"));
    failed_route_command_arguments_are_not_persisted(&root.path().join("credential-redaction"));
    no_record_preserves_absent_and_existing_state(&root.path().join("no-record"));
    missing_default_state_base_never_writes_history_into_cwd(
        &root.path().join("missing-state-base"),
    );
    require_record_begin_failure_prevents_execution(&root.path().join("require-record"));
}
