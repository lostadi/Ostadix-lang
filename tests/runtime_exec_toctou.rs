//! Black-box executable-binding races for the hosted OIR authority.
//!
//! The first operation keeps the admitted plan alive while this test mutates
//! the later Bash entrypoint. Dispatch must reject drift before Bash becomes
//! ready/started, and must never re-resolve another candidate through PATH.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn backends_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("backends")
}

fn write_executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn copy_bash(path: &Path) {
    let bash = which::which("bash").expect("Bash is required for the executable TOCTOU gate");
    fs::copy(bash, path).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn wait_for_python_backend(child: &mut Child, trace: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if fs::read_to_string(trace).ok().is_some_and(|text| {
            text.lines().any(|line| {
                line.contains("event=worker.backend_spawned") && line.contains("language=python")
            })
        }) {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("O exited before the mutation point: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("timed out waiting for the admitted Python backend to start");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn start_race(temp: &Path, admitted_path: &Path, fallback_dir: Option<&Path>) -> Child {
    let program = temp.join("race.O");
    fs::write(
        &program,
        r#"python^(
import time
time.sleep(1.5)
__oval_result__ = "first-settled"
)_python

bash^(
printf '%s' unsafe-backend-ran
)_bash
"#,
    )
    .unwrap();

    let trace = temp.join("lifecycle.trace");
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![admitted_path.parent().unwrap().to_path_buf()];
    if let Some(fallback_dir) = fallback_dir {
        paths.push(fallback_dir.to_path_buf());
    }
    paths.extend(std::env::split_paths(&inherited));
    let path = std::env::join_paths(paths).unwrap();

    let private_o = temp.join("O-under-test");
    fs::copy(env!("CARGO_BIN_EXE_O"), &private_o).unwrap();
    let mut private_o_permissions = fs::metadata(&private_o).unwrap().permissions();
    private_o_permissions.set_mode(0o755);
    fs::set_permissions(&private_o, private_o_permissions).unwrap();

    let mut child = Command::new(&private_o)
        .arg(&program)
        .arg(backends_dir())
        .env("PATH", path)
        .env("O_LIFECYCLE_TRACE", &trace)
        .env("O_BACKEND_OPERATION_TIMEOUT_MS", "10000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_python_backend(&mut child, &trace);
    child
}

fn finish(child: Child) -> Output {
    child.wait_with_output().unwrap()
}

#[test]
fn admitted_rustc_preserves_the_multicall_invocation_path() {
    let Ok(rustc) = which::which("rustc") else {
        eprintln!("SKIP-OPTIONAL: rustc is unavailable");
        return;
    };
    let canonical = rustc.canonicalize().unwrap();
    if canonical == rustc {
        eprintln!("NOTE: rustc is not a symlink on this host; exercising the same admitted path");
    }

    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("rust-multicall.O");
    fs::write(
        &program,
        r#"rust^(
fn main() { println!("rust-admitted-ok"); }
)_rust
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_O"))
        .arg(&program)
        .arg(backends_dir())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "admitted rustc launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "rust-admitted-ok"
    );
}

#[test]
fn atomic_replacement_is_rejected_before_backend_effect() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let admitted = bin.join("bash");
    copy_bash(&admitted);

    let marker = temp.path().join("replacement.marker");
    let replacement = bin.join("bash.replacement");
    write_executable(
        &replacement,
        &format!("#!/bin/sh\nprintf replaced > '{}'\n", marker.display()),
    );

    let child = start_race(temp.path(), &admitted, None);
    fs::rename(&replacement, &admitted).unwrap();
    let output = finish(child);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "replacement was accepted\n{stderr}"
    );
    assert!(
        stderr.contains("executable")
            && (stderr.contains("replaced") || stderr.contains("changed")),
        "{stderr}"
    );
    assert!(!marker.exists(), "the replacement executable was launched");
}

#[test]
fn in_place_mutation_is_rejected_before_backend_effect() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let admitted = bin.join("bash");
    copy_bash(&admitted);

    let marker = temp.path().join("mutation.marker");
    let child = start_race(temp.path(), &admitted, None);
    write_executable(
        &admitted,
        &format!("#!/bin/sh\nprintf mutated > '{}'\n", marker.display()),
    );
    let output = finish(child);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "mutation was accepted\n{stderr}");
    assert!(
        stderr.contains("executable")
            && (stderr.contains("retained handle")
                || stderr.contains("replaced")
                || stderr.contains("changed")),
        "{stderr}"
    );
    assert!(!marker.exists(), "the mutated executable was launched");
}

#[test]
fn missing_admitted_path_does_not_fall_back_to_later_path_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let admitted_bin = temp.path().join("admitted-bin");
    let fallback_bin = temp.path().join("fallback-bin");
    fs::create_dir(&admitted_bin).unwrap();
    fs::create_dir(&fallback_bin).unwrap();
    let admitted = admitted_bin.join("bash");
    copy_bash(&admitted);

    let fallback_marker = temp.path().join("fallback.marker");
    write_executable(
        &fallback_bin.join("bash"),
        &format!(
            "#!/bin/sh\nprintf fallback > '{}'\n",
            fallback_marker.display()
        ),
    );

    let child = start_race(temp.path(), &admitted, Some(&fallback_bin));
    fs::remove_file(&admitted).unwrap();
    let output = finish(child);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "PATH fallback was accepted\n{stderr}"
    );
    assert!(stderr.contains("executable"), "{stderr}");
    assert!(
        !fallback_marker.exists(),
        "dispatch re-resolved Bash through a later PATH candidate"
    );
}
