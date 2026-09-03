//! End-to-end tests for default literal execution and explicit safe projects.

use std::fs;
use std::path::Path;
use std::process::Command;

use o_lang::project::lower::extract_bundle_from_o;
use o_lang::project::ValidatedSelectionReceiptV1;

mod support;

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
fn olink_writes_validated_selection_receipt() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "olang.project.toml",
        br#"[project]
name = "olink-validated-selection"

[[routes]]
id = "reference"
command = ["sh", "-c", "sleep 0.2; printf same"]

[[routes]]
id = "fast"
command = ["sh", "-c", "printf same"]

[[route_sets]]
provides = "service"
alternatives = ["reference", "fast"]
policy = "benchmark_validate_and_select"
"#,
    );
    let output_dir = tempfile::tempdir().unwrap();
    let receipt_path = output_dir.path().join("selection.json");

    let output = olink()
        .arg("--project")
        .arg(project.path())
        .arg("--run")
        .args([
            "--route",
            "service",
            "--selection-receipt-out",
            receipt_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "o-link validated selection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: ValidatedSelectionReceiptV1 =
        serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
    receipt.validate().unwrap();
    assert_eq!(receipt.reference_route_id, "reference");
    assert_eq!(receipt.selected_route_id, "fast");
}

#[test]
fn olink_evidence_outputs_cannot_alias_each_other_or_project_inputs() {
    let project = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let marker = external.path().join("must-not-execute");
    let manifest_path = project.path().join("olang.project.toml");
    let manifest = format!(
        r#"[project]
name = "olink-output-safety"

[[routes]]
id = "reference"
command = ["sh", "-c", "printf ran > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[routes]]
id = "candidate"
command = ["sh", "-c", "printf ran > \"$MARKER\"; printf same"]
env = {{ MARKER = "{}" }}

[[route_sets]]
provides = "service"
alternatives = ["reference", "candidate"]
policy = "benchmark_validate_and_select"
"#,
        marker.display(),
        marker.display()
    );
    fs::write(&manifest_path, &manifest).unwrap();

    let input_alias = olink()
        .arg("--project")
        .arg(project.path())
        .arg("--run")
        .args(["--route", "service", "--selection-receipt-out"])
        .arg(&manifest_path)
        .output()
        .unwrap();
    assert!(!input_alias.status.success());
    assert_eq!(fs::read_to_string(&manifest_path).unwrap(), manifest);
    assert!(
        !marker.exists(),
        "route executed before input-alias rejection"
    );

    let lifted = external.path().join("lifted.O");
    let lift = olink()
        .arg("--project")
        .arg(project.path())
        .arg("-o")
        .arg(&lifted)
        .output()
        .unwrap();
    assert!(
        lift.status.success(),
        "could not create lifted safety fixture: {}",
        String::from_utf8_lossy(&lift.stderr)
    );
    let lifted_before = fs::read(&lifted).unwrap();
    let lifted_alias = olink()
        .arg("--project")
        .arg(&lifted)
        .arg("--run")
        .args(["--route", "service", "--selection-receipt-out"])
        .arg(&lifted)
        .output()
        .unwrap();
    assert!(!lifted_alias.status.success());
    assert_eq!(fs::read(&lifted).unwrap(), lifted_before);
    assert!(
        !marker.exists(),
        "lifted route executed before alias rejection"
    );

    let output_dir = tempfile::tempdir().unwrap();
    let alias = output_dir.path().join("same.json");
    let output_alias = olink()
        .current_dir(output_dir.path())
        .arg("--project")
        .arg(project.path())
        .arg("--run")
        .args([
            "--route",
            "service",
            "--mesh=prefer",
            "--mesh-trace-out",
            "same.json",
            "--selection-receipt-out",
        ])
        .arg(&alias)
        .output()
        .unwrap();
    assert!(!output_alias.status.success());
    assert!(String::from_utf8_lossy(&output_alias.stderr).contains("same output path"));
    assert!(!alias.exists());
    assert!(
        !marker.exists(),
        "route executed before output-alias rejection"
    );
}

#[test]
fn olink_rejects_project_trace_with_mesh_before_execution() {
    let project = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let marker = external.path().join("mesh-project-trace-must-not-run");
    let manifest = format!(
        r#"[project]
name = "olink-mesh-project-trace-preflight"

[[routes]]
id = "main"
command = ["sh", "-c", "printf ran > \"$MARKER\""]
env = {{ MARKER = "{}" }}
default = true
"#,
        marker.display()
    );
    fs::write(project.path().join("olang.project.toml"), manifest).unwrap();
    let trace = external.path().join("wrong-trace.json");

    let output = olink()
        .arg("--project")
        .arg(project.path())
        .arg("--run")
        .arg("--mesh=prefer")
        .arg("--project-trace-out")
        .arg(&trace)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("project-trace-out") && stderr.contains("mesh"),
        "stderr: {stderr}"
    );
    assert!(!marker.exists(), "route ran before trace-mode rejection");
    assert!(!trace.exists());
}

#[cfg(unix)]
#[test]
fn olink_evidence_publication_replaces_symlinks_without_touching_their_targets() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "olang.project.toml",
        br#"[project]
name = "olink-atomic-receipt"

[[routes]]
id = "reference"
command = ["sh", "-c", "printf same"]

[[routes]]
id = "candidate"
command = ["sh", "-c", "printf same"]

[[route_sets]]
provides = "service"
alternatives = ["reference", "candidate"]
policy = "benchmark_validate_and_select"
"#,
    );
    let output_dir = tempfile::tempdir().unwrap();
    let victim = output_dir.path().join("victim.txt");
    let receipt_path = output_dir.path().join("selection.json");
    fs::write(&victim, b"do not replace").unwrap();
    symlink(&victim, &receipt_path).unwrap();

    let output = olink()
        .arg("--project")
        .arg(project.path())
        .arg("--run")
        .args(["--route", "service", "--selection-receipt-out"])
        .arg(&receipt_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "o-link receipt publication failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&victim).unwrap(), b"do not replace");
    assert!(!fs::symlink_metadata(&receipt_path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::metadata(&receipt_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let receipt: ValidatedSelectionReceiptV1 =
        serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
    receipt.validate().unwrap();

    let trace_victim = output_dir.path().join("trace-victim.txt");
    let trace_path = output_dir.path().join("attempt.json");
    fs::write(&trace_victim, b"also do not replace").unwrap();
    symlink(&trace_victim, &trace_path).unwrap();
    let trace_output = olink()
        .arg("--project")
        .arg(project.path())
        .arg("--run")
        .args([
            "--route",
            "reference",
            "--project-trace-out",
            trace_path.to_str().unwrap(),
        ])
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .output()
        .unwrap();
    assert!(
        trace_output.status.success(),
        "o-link trace publication failed: {}",
        String::from_utf8_lossy(&trace_output.stderr)
    );
    assert_eq!(fs::read(&trace_victim).unwrap(), b"also do not replace");
    assert!(!fs::symlink_metadata(&trace_path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::metadata(&trace_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(read_project_trace(&trace_path)["format_version"], 6);
}

#[test]
fn olink_rejects_unknown_project_policy_before_execution() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph");
    let output = olink()
        .args([
            "--project",
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
fn olink_bare_single_directory_defaults_to_literal_and_run() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    write(&source, "main.O", b"text^(BARE_DIRECTORY_EXECUTED)_text\n");

    let out = olink()
        .current_dir(temp.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("defaults to --literal --run"),
        "stderr:\n{stderr}"
    );
    assert!(stderr.contains("o-link scan:"), "stderr:\n{stderr}");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("BARE_DIRECTORY_EXECUTED"),
        "stdout:\n{stdout}"
    );

    let combined = fs::read_to_string(temp.path().join("combined.O")).unwrap();
    assert!(combined.starts_with("# Linked by o-link"));
    assert!(combined.contains("text^(BARE_DIRECTORY_EXECUTED)_text"));
    assert!(!combined.contains("O-PROJECT-BUNDLE-V1"));
}

#[test]
fn olink_bare_directory_rejects_stdout_mixed_with_implicit_execution() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "main.O", b"text^(SHOULD_NOT_EXECUTE)_text\n");

    let out = olink().arg(dir.path()).arg("--stdout").output().unwrap();
    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "stdout must not contain mixed source/output"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("runs by default"), "stderr:\n{stderr}");
    assert!(stderr.contains("add --literal"), "stderr:\n{stderr}");
    assert!(stderr.contains("--project"), "stderr:\n{stderr}");
}

#[test]
fn explicit_project_document_and_direct_evaluation_are_inert() {
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
        .arg("--project")
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
    assert!(
        !marker.exists(),
        "project linking unexpectedly ran a script"
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
            .arg("--project")
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
    assert!(fs::read_to_string(&out_file)
        .unwrap()
        .contains("O-PROJECT-BUNDLE-V1"));

    // A lifted project document remains self-identifying on later commands.
    let listed = olink()
        .arg("--list-routes")
        .arg(&out_file)
        .output()
        .unwrap();
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("py-main"));
}

#[test]
fn olink_explicit_project_run_uses_default_route() {
    if !support::require_runtime("python3") {
        return;
    }
    let dir = python_project();
    let out = olink()
        .arg("--project")
        .arg(dir.path())
        .arg("--run")
        .output()
        .unwrap();
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
    if !support::require_runtime("python3") {
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
    let out = olink()
        .arg("--project")
        .arg(dir.path())
        .arg("--run")
        .output()
        .unwrap();
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
        .arg("--project")
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
        .arg("--project")
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
    let marker = dir.path().join("EXECUTED_DURING_LITERAL_LINK");
    write(
        dir.path(),
        "bootstrap.sh",
        format!("printf ran > '{}'\n", marker.display()).as_bytes(),
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
    assert!(source.contains("bash[*]^("));
    assert!(source.contains("python[*]^("));
    assert!(!source.contains("bash[0]^("));
    assert!(!source.contains("python[0]^("));
    assert!(!source.contains("O-PROJECT-BUNDLE-V1"));
    assert!(
        !marker.exists(),
        "explicit --literal/--execute-all unexpectedly inferred --run"
    );
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
        .arg("--project")
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
