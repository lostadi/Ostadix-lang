//! Adversarial gates for runtime-owned adapter support and Request launchers.
//!
//! Each case lets admission finish, keeps the plan alive in an earlier Bash
//! operation, substitutes a later runtime-owned input, and requires rejection
//! before the substituted object can produce an effect.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use o_lang::value::{OValue, OWireResponse};
use o_lang::wire;

fn write_executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn private_o(temp: &Path) -> PathBuf {
    let path = temp.join("O-under-test");
    fs::copy(env!("CARGO_BIN_EXE_O"), &path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn copy_adapter(backends: &Path, name: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("backends")
        .join(name);
    fs::copy(source, backends.join(name)).unwrap();
}

fn start_program(
    temp: &Path,
    source: &str,
    backends: &Path,
    path_prefix: Option<&Path>,
    extra_env: &[(&str, &str)],
) -> (Child, PathBuf) {
    let program = temp.join("race.O");
    fs::write(&program, source).unwrap();
    let trace = temp.join("lifecycle.trace");
    let mut paths = path_prefix
        .into_iter()
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut command = Command::new(private_o(temp));
    command
        .arg(program)
        .arg(backends)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("O_LIFECYCLE_TRACE", &trace)
        .env("O_BACKEND_OPERATION_TIMEOUT_MS", "10000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    wait_for_backend(&mut child, &trace, "bash");
    (child, trace)
}

fn wait_for_backend(child: &mut Child, trace: &Path, backend: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if fs::read_to_string(trace).ok().is_some_and(|text| {
            text.lines().any(|line| {
                line.contains("event=worker.backend_spawned")
                    && line.contains(&format!("language={backend}"))
            })
        }) {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("O exited before the mutation point: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("timed out waiting for the admitted {backend} backend to start");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn finish(child: Child) -> Output {
    child.wait_with_output().unwrap()
}

fn assert_rejected_before_marker(output: &Output, marker: &Path, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "substitution was accepted\n{stderr}"
    );
    assert!(stderr.contains(expected), "{stderr}");
    assert!(
        !marker.exists(),
        "the substituted object produced an effect"
    );
}

#[test]
fn standalone_custom_legacy_shim_does_not_require_bundled_common_support() {
    if which::which("python3").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 is unavailable");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();

    let mut exec_reply = Vec::new();
    wire::write_frame(
        &mut exec_reply,
        &OWireResponse::ok(OValue::str_("standalone-custom-shim")),
    )
    .unwrap();
    let mut shutdown_reply = Vec::new();
    wire::write_frame(&mut shutdown_reply, &OWireResponse::ok(OValue::Null)).unwrap();
    let shim = format!(
        r#"#!/usr/bin/env python3
import sys

EXEC_REPLY = bytes({exec_reply:?})
SHUTDOWN_REPLY = bytes({shutdown_reply:?})

def read_exact(length):
    chunks = []
    while length:
        chunk = sys.stdin.buffer.read(length)
        if not chunk:
            return None
        chunks.append(chunk)
        length -= len(chunk)
    return b''.join(chunks)

while True:
    header = read_exact(4)
    if header is None:
        break
    payload = read_exact(int.from_bytes(header, 'big'))
    if payload is None:
        break
    stopping = b'shutdown' in payload
    sys.stdout.buffer.write(SHUTDOWN_REPLY if stopping else EXEC_REPLY)
    sys.stdout.buffer.flush()
    if stopping:
        break
"#
    );
    write_executable(&backends.join("python_shim.py"), &shim);
    assert!(
        !backends.join("o_shim_common.py").exists(),
        "fixture must remain a genuinely standalone legacy adapter"
    );

    let program = temp.path().join("standalone.O");
    fs::write(
        &program,
        "python^(__oval_result__ = 'ignored by fixture')_python\n",
    )
    .unwrap();
    let output = Command::new(private_o(temp.path()))
        .arg(program)
        .arg(&backends)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "standalone custom legacy shim was coupled to bundled support\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "standalone-custom-shim"
    );
}

#[test]
fn imported_common_shim_mutation_stales_admission_before_python_launch() {
    if which::which("python3").is_err() || which::which("bash").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 and bash are required");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();
    copy_adapter(&backends, "python_shim.py");
    copy_adapter(&backends, "o_shim_common.py");
    let marker = temp.path().join("python.marker");
    let source = format!(
        "bash^(sleep 1.5)_bash\npython^(\nfrom pathlib import Path\nPath({:?}).write_text('unsafe')\n__oval_result__ = 'done'\n)_python\n",
        marker.display().to_string()
    );
    let (child, _) = start_program(temp.path(), &source, &backends, None, &[]);
    fs::write(
        backends.join("o_shim_common.py"),
        b"# substituted after admission\n",
    )
    .unwrap();
    let output = finish(child);
    assert_rejected_before_marker(&output, &marker, "backend artifacts");
}

#[test]
fn multipass_adapter_tool_replacement_is_rejected_before_shim_launch() {
    if which::which("python3").is_err() || which::which("bash").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 and bash are required");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();
    copy_adapter(&backends, "ubuntu_vm_shim.py");
    copy_adapter(&backends, "o_shim_common.py");
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let admitted = bin.join("multipass");
    write_executable(&admitted, "#!/bin/sh\nexit 99\n");
    let marker = temp.path().join("multipass.marker");
    let replacement = bin.join("multipass.replacement");
    write_executable(
        &replacement,
        &format!("#!/bin/sh\nprintf unsafe > {:?}\nexit 0\n", marker),
    );
    let (child, _) = start_program(
        temp.path(),
        "bash^(sleep 1.5)_bash\nubuntu_vm^(echo should-not-run)_ubuntu_vm\n",
        &backends,
        Some(&bin),
        &[],
    );
    fs::rename(replacement, admitted).unwrap();
    let output = finish(child);
    assert_rejected_before_marker(&output, &marker, "executable");
}

#[test]
fn persistent_adapter_revalidates_owned_tool_before_each_subprocess() {
    if which::which("python3").is_err() || which::which("bash").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 and bash are required");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();
    copy_adapter(&backends, "ubuntu_vm_shim.py");
    copy_adapter(&backends, "o_shim_common.py");
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let first_spawned = temp.path().join("multipass-first-spawned");
    let admitted = bin.join("multipass");
    write_executable(
        &admitted,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\ninfo) : > {:?}; sleep 1; printf Running ;;\nexec) cat >/dev/null; printf ok ;;\n*) exit 0 ;;\nesac\n",
            first_spawned
        ),
    );
    let marker = temp.path().join("persistent-multipass.marker");
    let replacement = bin.join("multipass.replacement");
    write_executable(
        &replacement,
        &format!("#!/bin/sh\nprintf unsafe > {:?}\nexit 0\n", marker),
    );
    let program = temp.path().join("persistent-adapter.O");
    fs::write(
        &program,
        "ubuntu_vm[0]^(printf should-not-complete)_ubuntu_vm[0]\n",
    )
    .unwrap();
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&inherited)))
            .unwrap();
    let mut child = Command::new(private_o(temp.path()))
        .arg(program)
        .arg(&backends)
        .env("PATH", path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !first_spawned.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("O exited before the adapter's first admitted subprocess: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the adapter's first admitted subprocess"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fs::rename(replacement, admitted).unwrap();
    let output = finish(child);
    assert_rejected_before_marker(&output, &marker, "adapter tool 'multipass'");
}

#[test]
fn nixos_adapter_tool_replacement_is_rejected_before_shim_launch() {
    if which::which("python3").is_err() || which::which("bash").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 and bash are required");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();
    copy_adapter(&backends, "nixos_test_shim.py");
    copy_adapter(&backends, "o_shim_common.py");
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let admitted = bin.join("nix");
    write_executable(&admitted, "#!/bin/sh\nexit 99\n");
    let marker = temp.path().join("nixos-nix.marker");
    let replacement = bin.join("nix.replacement");
    write_executable(
        &replacement,
        &format!("#!/bin/sh\nprintf unsafe > {:?}\nexit 0\n", marker),
    );
    let (child, _) = start_program(
        temp.path(),
        "bash^(sleep 1.5)_bash\nnixos_test^({ nodes.machine = {}; testScript = \"pass\"; })_nixos_test\n",
        &backends,
        Some(&bin),
        &[("NIXPKGS_ALLOW_UNSUPPORTED_SYSTEM", "1")],
    );
    fs::rename(replacement, admitted).unwrap();
    let output = finish(child);
    assert_rejected_before_marker(&output, &marker, "executable");
}

#[test]
fn unforced_lazy_nix_request_does_not_require_nix_on_path() {
    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();
    let empty_path = temp.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();
    let program = temp.path().join("lazy.O");
    fs::write(
        &program,
        r#"let pending = lazy(instantiate(nix_expr^(
derivation { name = "unforced"; builder = "/bin/sh"; system = builtins.currentSystem; args = [ "-c" "true" ]; }
)_nix_expr))
text^(deferred)_text
"#,
    )
    .unwrap();
    let output = Command::new(private_o(temp.path()))
        .arg(program)
        .arg(backends)
        .env("PATH", &empty_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "an unforced lazy Nix request was incorrectly made host-realizable\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn performed_request_reuses_one_nix_lease_across_its_subprocesses() {
    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let signal = temp.path().join("nix-first-spawned");
    let marker = temp.path().join("nix-replacement.marker");
    let admitted = bin.join("nix");
    let drv = "/nix/store/00000000000000000000000000000000-race.drv";
    write_executable(
        &admitted,
        &format!(
            "#!/bin/sh\ncase \" $* \" in\n*\" eval \"*) : > {:?}; sleep 1; printf '%s' {:?} ;;\n*\" derivation show \"*) printf '%s' {:?} ;;\nesac\n",
            signal,
            drv,
            format!(r#"{{"{drv}":{{"outputs":{{"out":{{}}}}}}}}"#)
        ),
    );
    let replacement = bin.join("nix.replacement");
    write_executable(
        &replacement,
        &format!("#!/bin/sh\nprintf unsafe > {:?}\nexit 0\n", marker),
    );
    let program = temp.path().join("request.O");
    fs::write(
        &program,
        r#"instantiate(nix_expr^(
derivation { name = "race"; builder = "/bin/sh"; system = builtins.currentSystem; args = [ "-c" "true" ]; }
)_nix_expr)
"#,
    )
    .unwrap();
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&inherited)))
            .unwrap();
    let mut child = Command::new(private_o(temp.path()))
        .arg(program)
        .arg(backends)
        .env("PATH", path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !signal.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("O exited before the perform-time mutation point: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the first admitted Nix subprocess"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fs::rename(replacement, admitted).unwrap();
    let output = finish(child);
    assert_rejected_before_marker(&output, &marker, "runtime command `nix`");
}

fn run_mixed_group_without_nix(mode: &str) -> (Output, bool) {
    let temp = tempfile::tempdir().unwrap();
    let backends = temp.path().join("backends");
    fs::create_dir(&backends).unwrap();
    copy_adapter(&backends, "python_shim.py");
    copy_adapter(&backends, "o_shim_common.py");
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(which::which("python3").unwrap(), bin.join("python3")).unwrap();
    let closure = temp.path().join("system");
    fs::create_dir_all(closure.join("bin")).unwrap();
    let marker = temp.path().join("activation.marker");
    write_executable(
        &closure.join("bin/switch-to-configuration"),
        &format!("#!/bin/sh\nprintf activated > {:?}\n", marker),
    );
    let program = temp.path().join("mixed.O");
    let opener = if mode == "autonomous-batch" {
        "autonomous(batch".to_string()
    } else {
        format!("now({mode}")
    };
    fs::write(
        &program,
        format!(
            r#"let system = python^(__oval_result__ = OStorePath(r{closure:?}))_python
{opener}(
instantiate(nix_expr^(derivation {{ name = "missing-nix"; builder = "/bin/sh"; system = builtins.currentSystem; args = [ "-c" "true" ]; }})_nix_expr),
dry_activate($system)
))
"#,
            closure = closure.display().to_string(),
        ),
    )
    .unwrap();
    let output = Command::new(private_o(temp.path()))
        .arg(program)
        .arg(backends)
        .env("PATH", &bin)
        .output()
        .unwrap();
    let activated = marker.exists();
    (output, activated)
}

#[test]
fn batch_scopes_missing_nix_to_nix_member_and_runs_activation_member() {
    if which::which("python3").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 is unavailable");
        return;
    }
    let (output, activated) = run_mixed_group_without_nix("batch");
    assert!(
        output.status.success(),
        "batch should collect the Nix failure\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        activated,
        "missing Nix suppressed the independent activation"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("required runtime command `nix`"));
}

#[test]
fn any_can_succeed_without_nix_when_independent_member_succeeds() {
    if which::which("python3").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 is unavailable");
        return;
    }
    let (output, activated) = run_mixed_group_without_nix("any");
    assert!(
        output.status.success(),
        "any should accept the independent success\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        activated,
        "missing Nix suppressed the successful any member"
    );
}

#[test]
fn autonomous_missing_nix_does_not_suppress_independent_activation() {
    if which::which("python3").is_err() {
        eprintln!("SKIP-OPTIONAL: python3 is unavailable");
        return;
    }
    let (output, activated) = run_mixed_group_without_nix("autonomous-batch");
    assert!(
        !output.status.success(),
        "autonomous Nix failure should remain visible"
    );
    assert!(
        activated,
        "autonomous Nix capture failure suppressed an independent activation"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("required runtime command `nix`"));
}
