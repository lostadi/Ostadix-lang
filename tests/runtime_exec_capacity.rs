//! Capacity and compatibility invariants for executable-bound hosted runs.
//!
//! These gates intentionally protect execution capability rather than adding
//! another authority policy. The direct-launch evidence layer must not turn a
//! persistent actor into one process per message or pull the MCP async stack
//! into the root interpreter and generated AOT runtime.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod support;

fn backends_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("backends")
}

#[cfg(unix)]
fn wait_bounded(mut child: std::process::Child, deadline: Duration) -> Output {
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if started.elapsed() >= deadline {
            if let Ok(group) = i32::try_from(child.id()) {
                // SAFETY: the test starts the private O process as the leader
                // of a new, test-owned process group.
                let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "100-message persistent actor exceeded {} seconds",
                deadline.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn persistent_python_actor_reuses_one_process_for_100_messages() {
    if which::which("python3").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 is unavailable");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("persistent-100.O");
    let trace = temp.path().join("lifecycle.trace");
    let private_o = temp.path().join("O-under-test");
    fs::copy(env!("CARGO_BIN_EXE_O"), &private_o).unwrap();
    let mut permissions = fs::metadata(&private_o).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&private_o, permissions).unwrap();

    let mut source = String::new();
    for _ in 0..100 {
        source.push_str(
            r#"python[0]^(
try:
    o_capacity_counter += 1
except NameError:
    o_capacity_counter = 1
__oval_result__ = o_capacity_counter
)_python[0]
"#,
        );
    }
    fs::write(&program, source).unwrap();

    let mut command = Command::new(&private_o);
    command
        .arg("--executor")
        .arg("graph")
        .arg("--workers")
        .arg("4")
        .arg(&program)
        .arg(backends_dir())
        .env("O_LIFECYCLE_TRACE", &trace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let started = Instant::now();
    let output = wait_bounded(
        support::spawn_private_executable(&mut command).unwrap(),
        Duration::from_secs(15),
    );
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "100-message persistent actor failed after {elapsed:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "[number] 100"
    );

    let events = fs::read_to_string(&trace).unwrap();
    let backend_spawns = events
        .lines()
        .filter(|line| {
            line.contains("event=worker.backend_spawned") && line.contains("language=python")
        })
        .count();
    let shim_spawns = events
        .lines()
        .filter(|line| {
            line.contains("event=proxy.shim_spawned") && line.contains("language=python")
        })
        .count();
    assert_eq!(
        backend_spawns, 1,
        "persistent actor spawned {backend_spawns} backend processes\n{events}"
    );
    assert_eq!(
        shim_spawns, 1,
        "persistent actor spawned {shim_spawns} Python shims\n{events}"
    );
}

#[test]
fn deferred_nix_request_does_not_require_nix_at_admission() {
    let temp = tempfile::tempdir().unwrap();
    let empty_path = temp.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_O"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/lazy_request_basic.O"))
        .arg(backends_dir())
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "constructing an unforced Request incorrectly required Nix\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("unresolved Request"),
        "the lazy Request example did not retain its deferred result"
    );
}

#[test]
fn forced_nix_request_without_nix_fails_before_perform_effect() {
    let temp = tempfile::tempdir().unwrap();
    let empty_path = temp.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();
    let program = temp.path().join("forced-nix.O");
    fs::write(&program, "instantiate(nix_expr^(pkgs.hello)_nix_expr)\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_O"))
        .arg(&program)
        .arg(backends_dir())
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a forced Request unexpectedly succeeded without Nix"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nix")
            && (stderr.contains("executable")
                || stderr.contains("runtime")
                || stderr.contains("spawn")),
        "forced Request failure did not clearly identify missing Nix: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn autonomous_request_authority_preserves_parallel_nix_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let work = temp.path().join("request-work");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&work).unwrap();
    let fake_nix = bin.join("nix");
    fs::write(
        &fake_nix,
        r#"#!/bin/sh
case " $* " in
  *" derivation show "*)
    printf '%s' '{"/nix/store/ostadix-capacity.drv":{"outputs":{"out":{}}}}'
    exit 0
    ;;
esac

case "$*" in
  *pkgs.one*) tag=one ;;
  *pkgs.two*) tag=two ;;
  *) printf '%s\n' "unexpected fake nix arguments: $*" >&2; exit 64 ;;
esac
: > "$O_REQUEST_TEST_WORK/started-$tag"
attempt=0
while [ ! -f "$O_REQUEST_TEST_WORK/started-one" ] || [ ! -f "$O_REQUEST_TEST_WORK/started-two" ]; do
  attempt=$((attempt + 1))
  if [ "$attempt" -gt 500 ]; then
    printf '%s\n' 'independent Nix requests did not overlap' >&2
    exit 70
  fi
  sleep 0.01
done
printf '%s' /nix/store/ostadix-capacity.drv
"#,
    )
    .unwrap();
    fs::set_permissions(&fake_nix, fs::Permissions::from_mode(0o755)).unwrap();

    let private_o = temp.path().join("O-under-test");
    fs::copy(env!("CARGO_BIN_EXE_O"), &private_o).unwrap();
    let mut permissions = fs::metadata(&private_o).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&private_o, permissions).unwrap();
    let program = temp.path().join("parallel-requests.O");
    fs::write(
        &program,
        r#"let e1 = nix_expr^(pkgs.one)_nix_expr
let e2 = nix_expr^(pkgs.two)_nix_expr
autonomous(batch(instantiate($e1), instantiate($e2)))
"#,
    )
    .unwrap();

    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&inherited)))
            .unwrap();
    let mut command = Command::new(&private_o);
    command
        .arg(&program)
        .arg(backends_dir())
        .env("PATH", path)
        .env("O_REQUEST_TEST_WORK", &work)
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let output = wait_bounded(
        support::spawn_private_executable(&mut command).unwrap(),
        Duration::from_secs(15),
    );
    assert!(
        output.status.success(),
        "parallel Request execution failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(work.join("started-one").is_file());
    assert!(work.join("started-two").is_file());
}

#[test]
fn root_default_and_generated_runtimes_exclude_the_mcp_async_stack() {
    let root_manifest: toml::Value = toml::from_str(
        &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap(),
    )
    .unwrap();
    let root_dependencies = root_manifest["dependencies"].as_table().unwrap();
    assert!(
        !root_dependencies.contains_key("rmcp"),
        "rmcp must remain outside the root interpreter dependency graph"
    );
    for dependency in ["tokio", "axum"] {
        if let Some(entry) = root_dependencies.get(dependency) {
            assert_eq!(
                entry
                    .as_table()
                    .and_then(|entry| entry.get("optional"))
                    .and_then(toml::Value::as_bool),
                Some(true),
                "{dependency} must remain opt-in for the root interpreter"
            );
        }
    }
    let defaults = root_manifest["features"]["default"].as_array().unwrap();
    assert!(
        defaults
            .iter()
            .filter_map(toml::Value::as_str)
            .all(|feature| feature != "notebook"),
        "the async notebook stack must not enter default O execution"
    );

    let mcp_manifest: toml::Value = toml::from_str(
        &fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("mcp/ostadix_lang_mcp_server/Cargo.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    let mcp_dependencies = mcp_manifest["dependencies"].as_table().unwrap();
    assert!(mcp_dependencies.contains_key("rmcp"));
    assert!(mcp_dependencies.contains_key("tokio"));

    let compiler_source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/olangc.rs"))
            .unwrap();
    let generated_manifest_source = compiler_source
        .split_once("fn generate_cargo_toml(")
        .expect("olangc retains its generated Cargo manifest function")
        .1
        .split_once("\n#[cfg(test)]")
        .expect("generated Cargo manifest function precedes the test module")
        .0;
    for dependency in ["rmcp", "tokio", "axum"] {
        assert!(
            !generated_manifest_source.contains(dependency),
            "generated AOT runtimes must not depend on {dependency}"
        );
    }
}
