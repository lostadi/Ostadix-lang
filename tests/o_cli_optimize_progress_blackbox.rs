//! Black-box acceptance boundaries for `o optimize --progress`.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;

const PRIVATE_STDOUT: &str = "PRIVATE_CANDIDATE_STDOUT_42891";
const PRIVATE_STDERR: &str = "PRIVATE_CANDIDATE_STDERR_42891";

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
    let stdout = std::str::from_utf8(&output.stdout).expect("JSON stdout must be UTF-8");
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

fn manifest(marker: Option<&Path>) -> String {
    let marker_prefix = marker.map_or_else(String::new, |path| {
        format!("printf executed >> '{}'; ", path.display())
    });
    format!(
        r#"[project]
name = "optimize-progress"

[[routes]]
id = "reference"
command = ["sh", "-c", "{marker_prefix}sleep 0.12; printf {PRIVATE_STDOUT}; printf {PRIVATE_STDERR} >&2"]

[[routes]]
id = "fast"
command = ["sh", "-c", "{marker_prefix}sleep 0.01; printf {PRIVATE_STDOUT}; printf {PRIVATE_STDERR} >&2"]

[[routes]]
id = "divergent"
command = ["sh", "-c", "{marker_prefix}printf DIVERGENT_PRIVATE_OUTPUT_42891; printf {PRIVATE_STDERR} >&2"]

[[route_sets]]
provides = "main"
alternatives = ["reference", "fast", "divergent"]
policy = "all"
"#,
    )
}

fn decoded_retained(stream: &Value) -> Vec<u8> {
    STANDARD
        .decode(
            stream["retained"]
                .as_str()
                .expect("captured stream must retain base64 bytes"),
        )
        .expect("retained stream must be valid base64")
}

fn assert_progress_is_not_recorded(record: &Value) {
    let top_level_stderr = decoded_retained(&record["stderr"]);
    assert!(
        !String::from_utf8_lossy(&top_level_stderr).contains("o optimize:"),
        "live progress entered the run-level retained stderr",
    );
    for route in record["route_results"]
        .as_array()
        .expect("optimization record must retain route results")
    {
        let route_stderr = decoded_retained(&route["stderr"]);
        assert!(
            !String::from_utf8_lossy(&route_stderr).contains("o optimize:"),
            "live progress entered retained stderr for route {}",
            route["route_id"],
        );
    }
}

#[test]
fn optimize_progress_is_safe_json_clean_and_rejected_before_json_execution() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let shell = which::which("sh").expect("test host must provide sh");
    let shell_bin = shell.parent().expect("sh must have a parent directory");

    let human_root = root.join("human");
    let human_home = human_root.join("home");
    let human_state = human_root.join("state");
    let human_project = human_root.join("project");
    write(&human_project.join("olang.project.toml"), manifest(None));

    let human = run(o_cli(&human_home, &human_state, Some(shell_bin)).args([
        "optimize",
        human_project.to_str().unwrap(),
        "--route",
        "main",
        "--progress",
        "always",
    ]));
    assert!(
        human.status.success(),
        "human optimize failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&human.stdout),
        String::from_utf8_lossy(&human.stderr),
    );

    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("Ostadix optimization evidence"));
    assert!(stdout.contains("Selected route: fast"));
    assert!(
        !stdout.contains("o optimize:"),
        "progress escaped onto final-evidence stdout: {stdout}",
    );
    assert!(!stdout.contains(PRIVATE_STDOUT));
    assert!(!stdout.contains(PRIVATE_STDERR));

    let stderr = String::from_utf8(human.stderr).unwrap();
    let lines = stderr.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.first().copied(),
        Some("o optimize: measuring 3 candidates concurrently; reference=\"reference\"")
    );
    assert_eq!(lines.len(), 5, "unexpected progress protocol:\n{stderr}");
    for (offset, line) in lines[1..4].iter().enumerate() {
        assert!(
            line.starts_with(&format!("o optimize: {}/3 settled \"", offset + 1)),
            "settlement line was not ordered and presentation-safe: {line}",
        );
        assert!(
            line.ends_with("complete branch (exit 0)"),
            "settlement line omitted its typed outcome: {line}",
        );
    }
    for route_id in ["reference", "fast", "divergent"] {
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains(&format!("settled \"{route_id}\" -")))
                .count(),
            1,
            "route {route_id:?} did not produce exactly one safe settlement line:\n{stderr}",
        );
    }
    assert_eq!(
        lines.last().copied(),
        Some("o optimize: validating declared outputs")
    );
    assert!(!stderr.contains(PRIVATE_STDOUT));
    assert!(!stderr.contains(PRIVATE_STDERR));
    assert!(!stderr.contains("DIVERGENT_PRIVATE_OUTPUT_42891"));

    let inspection =
        run(o_cli(&human_home, &human_state, Some(shell_bin)).args(["inspect", "last-run"]));
    assert!(
        inspection.status.success(),
        "could not inspect progress run: {}",
        String::from_utf8_lossy(&inspection.stderr),
    );
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_progress_is_not_recorded(&inspection["record"]);

    let json_root = root.join("json-default");
    let json_home = json_root.join("home");
    let json_state = json_root.join("state");
    let json_project = json_root.join("project");
    write(&json_project.join("olang.project.toml"), manifest(None));
    let json = run(o_cli(&json_home, &json_state, Some(shell_bin)).args([
        "optimize",
        json_project.to_str().unwrap(),
        "--route",
        "main",
        "--json",
    ]));
    assert!(
        json.status.success(),
        "JSON optimize failed: {}",
        String::from_utf8_lossy(&json.stderr),
    );
    let envelope = single_json(&json);
    assert_eq!(envelope["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(envelope["run"]["disposition"], "succeeded");
    assert!(
        !String::from_utf8_lossy(&json.stderr).contains("o optimize:"),
        "JSON mode emitted progress: {}",
        String::from_utf8_lossy(&json.stderr),
    );

    let rejected_root = root.join("json-progress-rejected");
    let rejected_home = rejected_root.join("home");
    let rejected_state = rejected_root.join("state");
    let rejected_project = rejected_root.join("project");
    let marker = rejected_root.join("must-not-execute");
    write(
        &rejected_project.join("olang.project.toml"),
        manifest(Some(&marker)),
    );
    let rejected = run(
        o_cli(&rejected_home, &rejected_state, Some(shell_bin)).args([
            "optimize",
            rejected_project.to_str().unwrap(),
            "--route",
            "main",
            "--json",
            "--progress",
            "always",
        ]),
    );
    assert!(!rejected.status.success());
    let rejection = single_json(&rejected);
    assert_eq!(rejection["schema"], "ostadix.optimize-summary/v1");
    assert_eq!(rejection["run"]["disposition"], "preflight_failed");
    assert_eq!(rejection["run"]["run_id"], Value::Null);
    assert!(
        !marker.exists(),
        "--json --progress always executed a candidate before rejection",
    );
    assert!(
        !rejected_state.exists(),
        "preflight rejection unexpectedly created durable run state",
    );
}
