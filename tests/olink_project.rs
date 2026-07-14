//! End-to-end tests that spawn the `o-link` binary in project mode.

use std::fs;
use std::path::Path;
use std::process::Command;

fn olink() -> Command {
    Command::new(env!("CARGO_BIN_EXE_o-link"))
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
fn olink_project_lifts_to_o_document() {
    let dir = python_project();
    let out_file = dir.path().join("lifted.O");
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

    let lifted = fs::read_to_string(&out_file).unwrap();
    assert!(lifted.contains("O-PROJECT-BUNDLE-V1"), "sentinel missing");
    // The lifted document embeds the bundle in a single payload block, not a
    // per-file sequential wrapper.
    assert_eq!(lifted.matches("^(").count(), 1);

    // --list-routes can read the lifted file back.
    let listed = olink()
        .arg("--list-routes")
        .arg(&out_file)
        .output()
        .unwrap();
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("py-main"));
}

#[test]
fn olink_project_run_default_route() {
    // Requires python3 on PATH; skip cleanly if unavailable.
    if which_python3().is_none() {
        eprintln!("skipping: python3 not available");
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
    // No default among multiple candidates → non-zero exit and guidance.
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("default") || stderr.contains("--route"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn olink_route_flag_requires_project() {
    let dir = python_project();
    let out = olink()
        .arg("--route")
        .arg("py-main")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--project"));
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
