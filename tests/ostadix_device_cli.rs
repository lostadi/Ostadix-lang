use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn device() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ostadix-device"))
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn help_and_clap_errors_have_stable_exit_codes() {
    let help = device().arg("--help").output().unwrap();
    assert!(help.status.success(), "{}", combined(&help));
    assert!(String::from_utf8_lossy(&help.stdout).contains("Ostadix-lang on Android"));

    let missing = device().output().unwrap();
    assert_eq!(missing.status.code(), Some(2), "{}", combined(&missing));

    let unknown = device().arg("definitely-not-a-command").output().unwrap();
    assert_eq!(unknown.status.code(), Some(2), "{}", combined(&unknown));
}

#[test]
fn status_json_is_valid_and_explicit_on_every_host() {
    let output = device()
        .args(["--root", env!("CARGO_MANIFEST_DIR"), "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", combined(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "ostadix.device-status/v1");
    assert!(value["android"]["detected"].is_boolean());
    assert!(value["cpu"]["allowed"].is_string());
    assert_eq!(value["privileges"]["su_probed"], false);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::env;
    use std::os::unix::fs::PermissionsExt;

    fn write_tool(directory: &Path, name: &str, body: &str) {
        let path = directory.join(name);
        fs::write(&path, format!("#!/usr/bin/env sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn fake_path(tools: &Path) -> std::ffi::OsString {
        let mut paths = vec![tools.to_path_buf()];
        if let Some(existing) = env::var_os("PATH") {
            paths.extend(env::split_paths(&existing));
        }
        env::join_paths(paths).unwrap()
    }

    struct Fixture {
        _temp: TempDir,
        root: PathBuf,
        tools: PathBuf,
        log: PathBuf,
        marker: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("project ; touch injection-marker");
            let tools = temp.path().join("tools");
            let log = temp.path().join("calls.log");
            let marker = temp.path().join("injection-marker");
            fs::create_dir_all(&root).unwrap();
            fs::create_dir_all(&tools).unwrap();
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname='fixture'\nversion='0.0.0'\n",
            )
            .unwrap();

            write_tool(
                &tools,
                "cargo",
                r#"
printf 'cargo|cwd=%s|jobs=%s|incremental=%s|wrapper=%s|idle=%s|rustflags=%s|encoded=%s|workspace_wrapper=%s|args=' "$PWD" "$CARGO_BUILD_JOBS" "$CARGO_INCREMENTAL" "$RUSTC_WRAPPER" "$SCCACHE_IDLE_TIMEOUT" "$RUSTFLAGS" "$CARGO_ENCODED_RUSTFLAGS" "$RUSTC_WORKSPACE_WRAPPER" >> "$TEST_LOG"
for arg in "$@"; do printf '<%s>' "$arg" >> "$TEST_LOG"; done
printf '\n' >> "$TEST_LOG"
exit "${FAKE_CARGO_EXIT:-0}"
"#,
            );
            write_tool(
                &tools,
                "sccache",
                r#"printf 'sccache|%s\n' "$*" >> "$TEST_LOG""#,
            );
            write_tool(
                &tools,
                "ccache",
                r#"printf 'ccache|%s\n' "$*" >> "$TEST_LOG""#,
            );
            write_tool(
                &tools,
                "make",
                r#"
printf 'make|cwd=%s|base=%s|compiler_check=%s|no_hash_dir=%s|args=' "$PWD" "$CCACHE_BASEDIR" "$CCACHE_COMPILERCHECK" "$CCACHE_NOHASHDIR" >> "$TEST_LOG"
for arg in "$@"; do printf '<%s>' "$arg" >> "$TEST_LOG"; done
printf '\n' >> "$TEST_LOG"
exit "${FAKE_MAKE_EXIT:-0}"
"#,
            );
            write_tool(
                &tools,
                "termux-wake-lock",
                r#"printf 'wake-lock\n' >> "$TEST_LOG""#,
            );
            write_tool(
                &tools,
                "termux-wake-unlock",
                r#"printf 'wake-unlock\n' >> "$TEST_LOG""#,
            );
            Self {
                _temp: temp,
                root,
                tools,
                log,
                marker,
            }
        }

        fn command(&self) -> Command {
            let mut command = device();
            command
                .env("PATH", fake_path(&self.tools))
                .env("TEST_LOG", &self.log)
                .arg("--root")
                .arg(&self.root);
            command
        }

        fn log(&self) -> String {
            fs::read_to_string(&self.log).unwrap_or_default()
        }
    }

    #[test]
    fn build_passes_exact_argv_env_and_cleans_up_wake_lock() {
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .env("RUSTFLAGS", "hostile-caller-rustflags")
            .env("CARGO_ENCODED_RUSTFLAGS", "hostile-encoded-rustflags")
            .env("RUSTC_WORKSPACE_WRAPPER", "hostile-workspace-wrapper")
            .args(["build", "rust", "--jobs", "3"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", combined(&output));
        let log = fixture.log();
        let lines = log.lines().collect::<Vec<_>>();
        assert_eq!(lines.first().copied(), Some("wake-lock"), "{log}");
        assert!(lines[1].contains("cwd="), "{log}");
        assert!(
            lines[1].contains("project ; touch injection-marker"),
            "{log}"
        );
        assert!(lines[1].contains("jobs=3|incremental=0"), "{log}");
        assert!(lines[1].contains("/tools/sccache|idle=600"), "{log}");
        assert!(
            lines[1].contains(
                "rustflags=-C target-cpu=native -C linker=clang -C link-arg=-fuse-ld=lld"
            ),
            "{log}"
        );
        assert!(lines[1].contains("|encoded=|workspace_wrapper=|"), "{log}");
        assert!(!lines[1].contains("hostile"), "{log}");
        assert!(
            lines[1].contains("args=<build><--workspace><--release><--locked><--bins>"),
            "{log}"
        );
        assert_eq!(lines.last().copied(), Some("wake-unlock"), "{log}");
        assert!(
            !fixture.marker.exists(),
            "project path was interpreted as shell syntax"
        );
    }

    #[test]
    fn build_propagates_child_failure_and_still_runs_cleanup() {
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .env("FAKE_CARGO_EXIT", "23")
            .args(["build", "rust", "--jobs", "2"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23), "{}", combined(&output));
        let log = fixture.log();
        assert!(log.contains("cargo|"), "{log}");
        assert!(!log.contains("sccache|--stop-server"), "{log}");
        assert!(log.ends_with("wake-unlock\n"), "{log}");
    }

    #[test]
    fn c17_build_uses_native_lld_flags_and_an_explicit_ccache_policy() {
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .args(["build", "c17", "--jobs", "4", "--no-wake-lock"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", combined(&output));
        let log = fixture.log();
        assert!(log.starts_with("make|cwd="), "{log}");
        assert!(log.contains("project ; touch injection-marker"), "{log}");
        assert!(
            log.contains("compiler_check=content|no_hash_dir=true"),
            "{log}"
        );
        assert!(log.contains("args=<-C><c_cpp><-B><-j4>"), "{log}");
        assert!(log.contains("CC="), "{log}");
        assert!(log.contains("/tools/ccache clang>"), "{log}");
        assert!(log.contains("-O3 -mcpu=native -flto=thin"), "{log}");
        assert!(
            log.contains("<LDFLAGS=-pthread -fuse-ld=lld -flto=thin><all>"),
            "{log}"
        );
        assert!(!fixture.marker.exists(), "project path became shell syntax");
    }

    #[test]
    fn no_wake_lock_skips_both_termux_api_commands() {
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .args(["build", "rust", "--jobs", "1", "--no-wake-lock"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", combined(&output));
        let log = fixture.log();
        assert!(!log.contains("wake-lock"), "{log}");
        assert!(!log.contains("wake-unlock"), "{log}");
    }

    #[test]
    fn explicit_root_run_shell_quotes_every_argument() {
        let fixture = Fixture::new();
        write_tool(
            &fixture.tools,
            "su",
            r#"
printf 'su|args=' >> "$TEST_LOG"
for arg in "$@"; do printf '<%s>' "$arg" >> "$TEST_LOG"; done
printf '\n' >> "$TEST_LOG"
"#,
        );
        let output = fixture
            .command()
            .args([
                "root",
                "run",
                "--",
                "/system/bin/id",
                "a; touch injection-marker",
                "single'quote",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", combined(&output));
        let log = fixture.log();
        assert!(
            log.contains(
                "<exec '/system/bin/id' 'a; touch injection-marker' 'single'\"'\"'quote'>"
            ),
            "{log}"
        );
        assert!(
            !fixture.marker.exists(),
            "root argv was interpreted by the caller's shell"
        );
    }
}
