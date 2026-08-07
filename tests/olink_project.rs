//! End-to-end tests for safe project-mode defaults and explicit literal mode.

use std::fs;
use std::path::Path;
use std::process::Command;

use o_lang::project::lower::extract_bundle_from_o;

fn olink() -> Command {
    Command::new(env!("CARGO_BIN_EXE_o-link"))
}

fn ounlink() -> Command {
    Command::new(env!("CARGO_BIN_EXE_o-unlink"))
}

fn o_interpreter() -> Command {
    Command::new(env!("CARGO_BIN_EXE_O"))
}

fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn read_project_trace(path: &Path) -> serde_json::Value {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("failed to read trace {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("trace {} is not valid JSON: {error}", path.display()))
}

fn python_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app.py",
        b"if __name__ == \"__main__\":\n    print('hello from app')\n",
    );
    dir
}

#[test]
fn olink_rejects_unknown_project_policy_before_execution() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph");
    let output = olink()
        .args([
            "--run",
            "--route",
            "main",
            "--routes-policy",
            "definitely-not-a-policy",
        ])
        .arg(fixture)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown route policy"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn olink_list_routes_for_directory() {
    let dir = python_project();
    let out = olink()
        .arg("--list-routes")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("py-main"), "route table:\n{stdout}");
    // Listing must not execute anything.
    assert!(!stdout.contains("hello from app"));
}

#[test]
fn olink_directory_defaults_to_safe_project_document() {
    let dir = python_project();
    let out_file = dir.path().join("lifted.O");
    let out = olink()
        .arg(dir.path())
        .arg("-o")
        .arg(&out_file)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("safe project mode"), "stderr:\n{stderr}");

    let lifted = fs::read_to_string(&out_file).unwrap();
    assert!(lifted.contains("O-PROJECT-BUNDLE-V1"), "sentinel missing");
    assert!(lifted.contains("No project route was executed"));
    // Source files are data in one payload, never per-file executable blocks.
    assert!(!lifted.contains("python[0]^("));
    assert!(!lifted.contains("bash[0]^("));

    // Auto-detection also works when reading the lifted file back.
    let listed = olink()
        .arg("--list-routes")
        .arg(&out_file)
        .output()
        .unwrap();
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("py-main"));
}

#[test]
fn direct_evaluation_of_directory_bundle_is_inert() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("DANGEROUS_SCRIPT_RAN");
    write(
        dir.path(),
        "bootstrap.sh",
        format!("#!/bin/sh\nprintf ran > {}\n", marker.display()).as_bytes(),
    );
    write(
        dir.path(),
        "app.py",
        br#"if __name__ == "__main__":
    print('project route ran')
"#,
    );
    let lifted = dir.path().join("lifted.O");

    let linked = olink()
        .arg(dir.path())
        .arg("-o")
        .arg(&lifted)
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&linked.stderr)
    );

    let evaluated = o_interpreter().arg(&lifted).output().unwrap();
    assert!(
        evaluated.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&evaluated.stderr)
    );
    let stdout = String::from_utf8_lossy(&evaluated.stdout);
    assert!(
        stdout.contains("No project route was executed"),
        "stdout:\n{stdout}"
    );
    assert!(!stdout.contains("project route ran"));
    assert!(!marker.exists(), "bootstrap script unexpectedly executed");
}

#[test]
fn project_output_is_not_recaptured_on_rerun() {
    let dir = python_project();
    let lifted = dir.path().join("lifted.O");

    for _ in 0..2 {
        let out = olink()
            .arg(dir.path())
            .arg("-o")
            .arg(&lifted)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let source = fs::read_to_string(&lifted).unwrap();
    let bundle = extract_bundle_from_o(&source).unwrap();
    assert!(bundle.files.iter().all(|file| file.path != "lifted.O"));
}

#[test]
fn olink_explicit_project_flag_remains_compatible() {
    let dir = python_project();
    let out_file = dir.path().join("explicit.O");
    let out = olink()
        .arg("--project")
        .arg(dir.path())
        .arg("-o")
        .arg(&out_file)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fs::read_to_string(out_file)
        .unwrap()
        .contains("O-PROJECT-BUNDLE-V1"));
}

#[test]
fn olink_directory_run_uses_project_default_route() {
    // Requires python3 on PATH; skip cleanly if unavailable.
    if which_python3().is_none() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let dir = python_project();
    let out = olink().arg(dir.path()).arg("--run").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("hello from app"), "output:\n{combined}");
}

#[test]
fn olink_explicit_project_hgraph_run_writes_unsigned_attempt_trace() {
    // The discovered project route invokes python3 directly.
    if which_python3().is_none() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let dir = python_project();
    let external = tempfile::tempdir().unwrap();
    let trace_path = external.path().join("olink-project-attempt.json");

    let output = olink()
        .arg("--project")
        .arg(dir.path())
        .arg("--run")
        .arg("--project-trace-out")
        .arg(&trace_path)
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "o-link HGraph project run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let trace = read_project_trace(&trace_path);
    let root = trace.as_object().expect("trace root must be an object");
    assert_eq!(root.len(), 3, "unexpected trace root fields: {root:?}");
    assert_eq!(trace["format_version"], 6);
    let target = trace["header"]["target"]
        .as_str()
        .expect("trace header must name the selected route");
    assert!(
        target.starts_with("py-main"),
        "unexpected Python route: {target}"
    );
    assert_eq!(trace["header"]["policy"], format!("explicit:{target}"));
    assert_eq!(trace["header"]["logical_graph_schema"], 1);
    assert_eq!(trace["header"]["deployment_plan_schema"], 1);
    for field in [
        "bundle_digest",
        "logical_graph_digest",
        "deployment_plan_digest",
        "execution_attempt_id",
    ] {
        let digest = trace["header"][field]
            .as_str()
            .unwrap_or_else(|| panic!("missing trace header field `{field}`"));
        assert_eq!(digest.len(), 64, "{field} must be a SHA-256-sized id");
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    let events = trace["events"]
        .as_array()
        .expect("trace events must be an array");
    assert!(!events.is_empty());
    let run_label = format!("run-route:{target}");
    assert_eq!(
        events
            .iter()
            .filter(|event| event["operation_label"] == run_label)
            .map(|event| event["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ready", "started", "settled_success"]
    );
    for (ordinal, event) in events.iter().enumerate() {
        assert_eq!(event["coordinator_ordinal"], ordinal as u64);
    }
    let encoded = serde_json::to_string(&trace).unwrap().to_ascii_lowercase();
    for forbidden in ["signature", "signed_receipt", "owreceipt", "attestation"] {
        assert!(
            !encoded.contains(forbidden),
            "unsigned diagnostic trace contains `{forbidden}`"
        );
    }
}

#[test]
fn olink_any_success_preserves_the_successful_attempt_prefix() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "olang.project.toml",
        br#"[project]
name = "olink-any-success-prefix"

[[routes]]
id = "first-failure"
kind = "shell"
command = ["sh", "-c", "exit 5"]
failure_continuation = "declared_idempotent"

[[routes]]
id = "second-success"
kind = "shell"
command = ["sh", "-c", "exit 0"]

[[routes]]
id = "never-started"
kind = "shell"
command = ["sh", "-c", "exit 0"]

[[route_sets]]
provides = "service"
alternatives = ["first-failure", "second-success", "never-started"]
policy = "any_success"
"#,
    );
    let external = tempfile::tempdir().unwrap();
    let trace_path = external.path().join("olink-any-success-prefix.json");

    let output = olink()
        .arg("--project")
        .arg(dir.path())
        .arg("--run")
        .args(["--route", "service"])
        .arg("--project-trace-out")
        .arg(&trace_path)
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "o-link ordered HGraph run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout
        .find("first-failure")
        .unwrap_or_else(|| panic!("first result missing: {stdout}"));
    let second = stdout
        .find("second-success")
        .unwrap_or_else(|| panic!("second result missing: {stdout}"));
    assert!(first < second, "attempt prefix was reordered: {stdout}");
    assert!(
        !stdout.contains("never-started"),
        "unstarted result was printed: {stdout}"
    );

    let trace = read_project_trace(&trace_path);
    assert_eq!(trace["header"]["policy"], "any_success");
    assert!(!trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["branch"] == 2));
}

#[test]
fn olink_project_run_ambiguous_requires_selection() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.py",
        b"if __name__ == \"__main__\":\n    print('a')\n",
    );
    write(
        dir.path(),
        "b.py",
        b"if __name__ == \"__main__\":\n    print('b')\n",
    );
    let out = olink().arg(dir.path()).arg("--run").output().unwrap();
    // No default among multiple candidates -> non-zero exit and guidance.
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("default") || stderr.contains("--route"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn olink_route_selection_requires_run() {
    let dir = python_project();
    let out = olink()
        .arg("--route")
        .arg("py-main")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--run"));
}

#[test]
fn project_mode_rejects_literal_only_flags_instead_of_ignoring_them() {
    let dir = python_project();
    let out = olink()
        .arg(dir.path())
        .arg("--lang")
        .arg("txt=text")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--lang"), "stderr:\n{stderr}");
    assert!(stderr.contains("--literal"), "stderr:\n{stderr}");
}

#[test]
fn olink_execute_all_alias_enters_explicit_legacy_mode() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "bootstrap.sh",
        b"echo should-not-run-during-link\n",
    );
    write(dir.path(), "app.py", b"print('also not run during link')\n");
    let output = dir.path().join("literal.O");

    let out = olink()
        .arg("--execute-all")
        .arg(dir.path())
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--literal/--execute-all directory mode"));
    let source = fs::read_to_string(output).unwrap();
    assert!(source.starts_with("# Linked by o-link"));
    assert!(source.contains("bash[0]^("));
    assert!(source.contains("python[0]^("));
    assert!(!source.contains("O-PROJECT-BUNDLE-V1"));
}

#[test]
fn olink_multiple_directories_require_literal_opt_in() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    write(left.path(), "a.py", b"print('a')\n");
    write(right.path(), "b.py", b"print('b')\n");

    let out = olink().arg(left.path()).arg(right.path()).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--literal"));
}

#[test]
fn ounlink_restores_safe_lifted_project_including_binary_files() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let restored = temp.path().join("restored");
    let lifted = temp.path().join("lifted.O");
    fs::create_dir_all(&source).unwrap();
    write(&source, "app.py", b"print('hello')\n");
    write(&source, "assets/blob.bin", &[0, 1, 2, 3, 255]);

    let linked = olink()
        .arg(&source)
        .arg("-o")
        .arg(&lifted)
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&linked.stderr)
    );

    let unlinked = ounlink()
        .arg(&lifted)
        .arg("-o")
        .arg(&restored)
        .output()
        .unwrap();
    assert!(
        unlinked.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unlinked.stderr)
    );
    assert_eq!(
        fs::read(restored.join("app.py")).unwrap(),
        fs::read(source.join("app.py")).unwrap()
    );
    assert_eq!(
        fs::read(restored.join("assets/blob.bin")).unwrap(),
        fs::read(source.join("assets/blob.bin")).unwrap()
    );
}

fn which_python3() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("python3");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
