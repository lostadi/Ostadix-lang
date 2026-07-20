//! Black-box acceptance gate for the explicitly hosted live-system oracle.

use std::fs;
use std::path::Path;
use std::process::Command;

const ORDERED_MARKERS: [&str; 9] = [
    "HOSTED live reference: immutable package CAS PASS",
    "HOSTED live reference: over-broad capability denied",
    "HOSTED live reference: health-gated activation PASS",
    "HOSTED live reference: cross-world OValue composition PASS",
    "HOSTED live reference: failed upgrade rollback PASS",
    "HOSTED live reference: stale service bearer denied",
    "HOSTED live reference: crash isolation and restart PASS",
    "HOSTED live reference: active-set reconstruction PASS",
    "HOSTED live reference: PASS",
];

const FORBIDDEN_OVERCLAIMS: [&str; 4] = [
    "O-core live system: PASS",
    "Milestone 5 complete",
    "Linux personality: PASS",
    "CAPABILITY LEAKED",
];

struct WritableTempDir(tempfile::TempDir);

impl WritableTempDir {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create hosted-live temporary state"))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for WritableTempDir {
    fn drop(&mut self) {
        // Published CAS objects are deliberately read-only. Restore ownership
        // permissions inside this exact TempDir before TempDir removes it.
        restore_owner_permissions(self.0.path());
    }
}

#[test]
fn hosted_live_reference_demo_is_an_honest_ordered_gate() {
    let state = WritableTempDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_o-live-host"))
        .arg("demo")
        .arg("--state")
        .arg(state.path())
        .output()
        .expect("run o-live-host demo");

    let stdout = String::from_utf8(output.stdout).expect("o-live-host stdout is UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("o-live-host stderr is UTF-8");
    assert!(
        output.status.success(),
        "o-live-host failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr
    );

    let mut prior_position = None;
    for marker in ORDERED_MARKERS {
        let positions = stdout
            .match_indices(marker)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        assert_eq!(
            positions.len(),
            1,
            "expected exactly one `{marker}`, found {}\nstdout:\n{stdout}",
            positions.len()
        );
        if let Some(prior) = prior_position {
            assert!(
                positions[0] > prior,
                "marker `{marker}` appeared out of order\nstdout:\n{stdout}"
            );
        }
        prior_position = Some(positions[0]);
    }

    let combined = format!("{stdout}\n{stderr}");
    for forbidden in FORBIDDEN_OVERCLAIMS {
        assert!(
            !combined.contains(forbidden),
            "hosted reference emitted forbidden overclaim `{forbidden}`\n{combined}"
        );
    }
}

fn restore_owner_permissions(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }

    make_owner_writable(path, &metadata);
    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            restore_owner_permissions(&entry.path());
        }
    }
}

#[cfg(unix)]
fn make_owner_writable(path: &Path, metadata: &fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;

    let required = if metadata.is_dir() { 0o700 } else { 0o600 };
    let mode = metadata.permissions().mode() | required;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn make_owner_writable(path: &Path, metadata: &fs::Metadata) {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(false);
    let _ = fs::set_permissions(path, permissions);
}
