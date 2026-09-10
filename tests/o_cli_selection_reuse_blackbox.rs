//! Compiled-binary acceptance boundaries for bundle-bound selected-route reuse.
//!
//! The cases are intentionally one serial test: a successful optimization run
//! becomes the immutable evidence source for successful reuse, a postcondition
//! failure, and two pre-dispatch rejection checks.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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
    let stdout = std::str::from_utf8(&output.stdout).expect("o-cli JSON stdout is UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count(),
        1,
        "expected exactly one compact JSON envelope\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not one JSON object: {error}\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        )
    })
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

fn remove_if_present(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("failed to clear {}: {error}", path.display()),
    }
}

fn clear_markers(markers: &[&Path]) {
    for marker in markers {
        remove_if_present(marker);
    }
}

fn assert_selected_branch_only(
    reference: &Path,
    prepare: &Path,
    selected: &Path,
    divergent: &Path,
) {
    assert_eq!(
        fs::read(prepare).unwrap(),
        b"prepare",
        "the selected route's declared prerequisite did not execute exactly once",
    );
    assert_eq!(
        fs::read(selected).unwrap(),
        b"fast",
        "the selected top-level route did not execute exactly once",
    );
    assert!(
        !reference.exists(),
        "selection reuse dispatched the reference branch"
    );
    assert!(
        !divergent.exists(),
        "selection reuse dispatched a rejected alternative branch"
    );
}

fn manifest(
    reference_marker: &Path,
    prepare_marker: &Path,
    selected_marker: &Path,
    divergent_marker: &Path,
    drift_toggle: &Path,
) -> String {
    format!(
        r#"[project]
name = "selection-reuse-blackbox"

[[routes]]
id = "reference"
command = ["sh", "-c", 'printf reference >> "$MARKER"; sleep 0.60; printf stable']
env = {{ MARKER = "{}" }}
pure = true

[[routes]]
id = "prepare-fast"
command = ["sh", "-c", 'printf prepare >> "$MARKER"']
env = {{ MARKER = "{}" }}
pure = true

[[routes]]
id = "fast"
command = ["sh", "-c", 'printf fast >> "$MARKER"; if [ -e "$DRIFT_TOGGLE" ]; then printf changed; else printf stable; fi']
env = {{ MARKER = "{}", DRIFT_TOGGLE = "{}" }}
depends_on = ["prepare-fast"]
pure = true

[[routes]]
id = "divergent"
command = ["sh", "-c", 'printf divergent >> "$MARKER"; printf divergent']
env = {{ MARKER = "{}" }}
pure = true

[[route_sets]]
provides = "main"
alternatives = ["reference", "fast", "divergent"]
policy = "benchmark_validate_and_select"
"#,
        reference_marker.display(),
        prepare_marker.display(),
        selected_marker.display(),
        drift_toggle.display(),
        divergent_marker.display(),
    )
}

#[test]
fn exact_selection_reuse_is_single_branch_bound_and_fail_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let reference_marker = root.join("reference-ran");
    let prepare_marker = root.join("prepare-ran");
    let selected_marker = root.join("fast-ran");
    let divergent_marker = root.join("divergent-ran");
    let drift_toggle = root.join("ambient-output-drift");
    let markers = [
        reference_marker.as_path(),
        prepare_marker.as_path(),
        selected_marker.as_path(),
        divergent_marker.as_path(),
    ];
    let shell = which::which("sh").expect("test host must provide sh");
    let shell_bin = shell.parent().expect("sh must have a parent directory");

    write(
        &project.join("olang.project.toml"),
        manifest(
            &reference_marker,
            &prepare_marker,
            &selected_marker,
            &divergent_marker,
            &drift_toggle,
        ),
    );

    let optimized = run(o_cli(&home, &state, Some(shell_bin)).args([
        "optimize",
        project.to_str().unwrap(),
        "--route",
        "main",
        "--progress",
        "never",
        "--json",
    ]));
    assert!(
        optimized.status.success(),
        "source optimization failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&optimized.stdout),
        String::from_utf8_lossy(&optimized.stderr),
    );
    let optimized = single_json(&optimized);
    assert_eq!(optimized["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(optimized["run"]["disposition"], "succeeded");
    assert_eq!(optimized["receipt"]["selected_route_id"], "fast");
    let source_run_id = optimized["run"]["run_id"]
        .as_str()
        .expect("optimization did not retain an exact source run ID")
        .to_string();
    assert_eq!(source_run_id.len(), 64);
    for marker in markers {
        assert!(
            marker.exists(),
            "optimization did not execute candidate instrumentation {}",
            marker.display(),
        );
    }

    clear_markers(&markers);
    let reused = run(o_cli(&home, &state, Some(shell_bin)).args([
        "run",
        project.to_str().unwrap(),
        "--selection-run",
        &source_run_id,
        "--json",
    ]));
    assert!(
        reused.status.success(),
        "exact selected-route reuse failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&reused.stdout),
        String::from_utf8_lossy(&reused.stderr),
    );
    let reused = single_json(&reused);
    assert_eq!(reused["schema"], "ostadix.run-summary/v1");
    assert_eq!(reused["disposition"], "succeeded");
    assert_eq!(reused["recording"]["status"], "recorded");
    assert_eq!(
        reused["selection_reuse"]["schema"],
        "ostadix.project-selection-reuse-observation/v1"
    );
    assert_eq!(reused["selection_reuse"]["source_run_id"], source_run_id);
    assert_eq!(reused["selection_reuse"]["selected_route_id"], "fast");
    assert_eq!(
        reused["selection_reuse"]["output_check"]["status"],
        "matched"
    );
    assert_eq!(
        reused["selection_reuse"]["output_check"]["observed_declared_output_sha256"],
        reused["selection_reuse"]["output_check"]["expected_declared_output_sha256"],
    );
    let reused_run_id = reused["run_id"]
        .as_str()
        .expect("successful reuse was not recorded")
        .to_string();
    assert_ne!(reused_run_id, source_run_id);
    assert_selected_branch_only(
        &reference_marker,
        &prepare_marker,
        &selected_marker,
        &divergent_marker,
    );

    let inspected = run(o_cli(&home, &state, Some(shell_bin)).args(["inspect", &reused_run_id]));
    assert!(
        inspected.status.success(),
        "could not inspect successful reuse record: {}",
        String::from_utf8_lossy(&inspected.stderr),
    );
    let inspected: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let record = &inspected["record"];
    assert_eq!(record["run_id"], reused_run_id);
    assert_eq!(record["disposition"], "succeeded");
    assert_eq!(record["validated_selection_receipt"], Value::Null);
    assert_eq!(
        record["intent"]["selection_reuse"]["schema"],
        "ostadix.project-selection-reuse-binding/v1"
    );
    assert_eq!(
        record["intent"]["selection_reuse"]["source_run_id"],
        source_run_id
    );
    assert_eq!(
        record["intent"]["selection_reuse"]["contract"]["effect_boundary"],
        "declared_pure_transitive_routes/v1"
    );
    assert_eq!(record["selection_reuse"], reused["selection_reuse"]);
    let route_results = record["route_results"]
        .as_array()
        .expect("reuse record must retain its selected result");
    assert_eq!(route_results.len(), 1);
    assert_eq!(route_results[0]["route_id"], "fast");

    clear_markers(&markers);
    let before_last_run_rejection = snapshot_tree(&state);
    let mutable_selector = run(o_cli(&home, &state, Some(shell_bin)).args([
        "run",
        project.to_str().unwrap(),
        "--selection-run",
        "last-run",
        "--json",
    ]));
    assert!(
        !mutable_selector.status.success(),
        "mutable last-run selector was accepted for reuse"
    );
    let mutable_selector = single_json(&mutable_selector);
    assert_eq!(mutable_selector["disposition"], "preflight_failed");
    assert_eq!(mutable_selector["run_id"], Value::Null);
    assert!(mutable_selector["failure"]["message"]
        .as_str()
        .unwrap()
        .contains("exact 64-character run ID"));
    assert_eq!(snapshot_tree(&state), before_last_run_rejection);
    for marker in markers {
        assert!(!marker.exists(), "last-run rejection dispatched a route");
    }

    write(&drift_toggle, b"change the selected route's ambient output");
    let drifted = run(o_cli(&home, &state, Some(shell_bin)).args([
        "run",
        project.to_str().unwrap(),
        "--selection-run",
        &source_run_id,
        "--json",
    ]));
    assert!(
        !drifted.status.success(),
        "changed declared output unexpectedly passed the reuse postcondition"
    );
    let drifted = single_json(&drifted);
    assert_eq!(drifted["disposition"], "execution_failed");
    assert_eq!(drifted["recording"]["status"], "recorded");
    assert_eq!(drifted["failure"]["stage"], "selection_reuse_postcondition");
    assert_eq!(drifted["selection_reuse"]["source_run_id"], source_run_id);
    assert_eq!(
        drifted["selection_reuse"]["output_check"]["status"],
        "declared_output_mismatch"
    );
    assert_ne!(
        drifted["selection_reuse"]["output_check"]["observed_declared_output_sha256"],
        drifted["selection_reuse"]["output_check"]["expected_declared_output_sha256"],
    );
    let drifted_run_id = drifted["run_id"]
        .as_str()
        .expect("postcondition failure was not durably recorded")
        .to_string();
    assert_selected_branch_only(
        &reference_marker,
        &prepare_marker,
        &selected_marker,
        &divergent_marker,
    );

    let inspected_drift =
        run(o_cli(&home, &state, Some(shell_bin)).args(["inspect", &drifted_run_id]));
    assert!(inspected_drift.status.success());
    let inspected_drift: Value = serde_json::from_slice(&inspected_drift.stdout).unwrap();
    let drift_record = &inspected_drift["record"];
    assert_eq!(drift_record["run_id"], drifted_run_id);
    assert_eq!(drift_record["disposition"], "execution_failed");
    assert_eq!(
        drift_record["failure"]["stage"],
        "selection_reuse_postcondition"
    );
    assert_eq!(drift_record["selection_reuse"], drifted["selection_reuse"]);
    assert_eq!(drift_record["validated_selection_receipt"], Value::Null);
    let drift_results = drift_record["route_results"]
        .as_array()
        .expect("failed reuse must retain the selected route result");
    assert_eq!(drift_results.len(), 1);
    assert_eq!(drift_results[0]["route_id"], "fast");
    assert_eq!(drift_results[0]["exit_code"], 0);

    remove_if_present(&drift_toggle);
    clear_markers(&markers);
    write(
        &project.join("bundle-change.txt"),
        b"the source run did not bind this file\n",
    );
    let before_bundle_rejection = snapshot_tree(&state);
    let changed_bundle = run(o_cli(&home, &state, Some(shell_bin)).args([
        "run",
        project.to_str().unwrap(),
        "--selection-run",
        &source_run_id,
        "--json",
    ]));
    assert!(
        !changed_bundle.status.success(),
        "changed project bundle was admitted for selected-route reuse"
    );
    let changed_bundle = single_json(&changed_bundle);
    assert_eq!(changed_bundle["disposition"], "preflight_failed");
    assert_eq!(changed_bundle["run_id"], Value::Null);
    assert_eq!(changed_bundle["recording"]["status"], "not_started");
    assert!(changed_bundle["failure"]["message"]
        .as_str()
        .unwrap()
        .contains("does not exactly match"));
    assert_eq!(
        snapshot_tree(&state),
        before_bundle_rejection,
        "preflight bundle rejection mutated durable run state",
    );
    for marker in markers {
        assert!(!marker.exists(), "bundle rejection dispatched a route");
    }
}
