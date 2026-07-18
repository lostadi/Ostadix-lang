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
    write(dir.path(), "bootstrap.sh", b"echo should-not-run-during-link\n");
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

    let out = olink()
        .arg(left.path())
        .arg(right.path())
        .output()
        .unwrap();
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
