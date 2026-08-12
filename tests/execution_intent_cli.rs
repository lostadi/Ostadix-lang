use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use o_lang::evidence::ExecutionIntentV1;

fn backends_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("backends")
}

fn write_program(path: &Path, marker: &Path, suffix: &str) {
    fs::write(
        path,
        format!(
            "bash^(\nprintf '%s' touched > '{}'\nprintf '%s' ok\n)_bash\n{suffix}",
            marker.display()
        ),
    )
    .unwrap();
}

fn analyze_intent(program: &Path) -> (ExecutionIntentV1, Output) {
    let output = Command::new(env!("CARGO_BIN_EXE_olangc"))
        .arg(program)
        .args(["--target", "ir", "--execution-intent-json", "--shim-dir"])
        .arg(backends_dir())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "olangc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let intent: ExecutionIntentV1 = serde_json::from_slice(&output.stdout).unwrap();
    intent.validate().unwrap();
    (intent, output)
}

fn run_required(
    program: &Path,
    source_sha256: &str,
    execution_intent_sha256: &str,
    executor: &str,
) -> Output {
    Command::new(env!("CARGO_BIN_EXE_O"))
        .args(["--executor", executor, "--require-source-sha256"])
        .arg(source_sha256)
        .arg("--require-execution-intent-sha256")
        .arg(execution_intent_sha256)
        .arg(program)
        .arg(backends_dir())
        .output()
        .unwrap()
}

fn admission_sha256(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .lines()
        .find_map(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("admission-sha256="))
        })
        .expect("schedule explanation omitted admission-sha256")
        .to_string()
}

#[test]
fn stable_intent_repeats_across_processes_while_live_admission_does_not() {
    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("stable.O");
    fs::write(&program, "text^(stable)_text\n").unwrap();

    let (first, first_output) = analyze_intent(&program);
    let (second, second_output) = analyze_intent(&program);
    assert_eq!(first, second);
    assert_eq!(first_output.stdout, second_output.stdout);

    let explain = || {
        Command::new(env!("CARGO_BIN_EXE_olangc"))
            .arg(&program)
            .args(["--target", "ir", "--explain-schedule", "--shim-dir"])
            .arg(backends_dir())
            .output()
            .unwrap()
    };
    let first_admission = explain();
    let second_admission = explain();
    assert!(first_admission.status.success());
    assert!(second_admission.status.success());
    assert_ne!(
        admission_sha256(&first_admission.stdout),
        admission_sha256(&second_admission.stdout),
        "process-bound live admissions must remain distinct from stable intent"
    );
}

#[test]
fn stale_or_serial_required_intent_is_rejected_before_backend_effects() {
    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("effect.O");
    let marker = temp.path().join("effect.marker");
    write_program(&program, &marker, "");
    let (original, _) = analyze_intent(&program);

    // A byte-only source change keeps the lowered program equivalent but must
    // still invalidate the exact source identity before Bash is dispatched.
    write_program(&program, &marker, "\n");
    let stale_source = run_required(
        &program,
        &original.source_sha256,
        &original.execution_intent_sha256,
        "graph",
    );
    assert!(!stale_source.status.success());
    assert!(
        String::from_utf8_lossy(&stale_source.stderr).contains("required source SHA-256 mismatch")
    );
    assert!(!marker.exists(), "stale source dispatched its Bash effect");

    let (current, _) = analyze_intent(&program);
    let stale_intent = run_required(
        &program,
        &current.source_sha256,
        &original.execution_intent_sha256,
        "graph",
    );
    assert!(!stale_intent.status.success());
    assert!(String::from_utf8_lossy(&stale_intent.stderr)
        .contains("required execution-intent SHA-256 mismatch"));
    assert!(!marker.exists(), "stale intent dispatched its Bash effect");

    let serial = run_required(
        &program,
        &current.source_sha256,
        &current.execution_intent_sha256,
        "serial",
    );
    assert!(!serial.status.success());
    assert!(String::from_utf8_lossy(&serial.stderr)
        .contains("required execution-intent gating is available only for graph execution"));
    assert!(!marker.exists(), "serial gate dispatched its Bash effect");

    let accepted = run_required(
        &program,
        &current.source_sha256,
        &current.execution_intent_sha256,
        "graph",
    );
    assert!(
        accepted.status.success(),
        "matching intent failed: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert_eq!(fs::read(&marker).unwrap(), b"touched");
}
