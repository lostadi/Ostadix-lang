//! Explicit Linux qualification: namespace permission is a test prerequisite.
#![cfg(target_os = "linux")]

use std::fs;
use std::process::{Command, Output};

fn successful(output: Output) -> Output {
    assert!(
        output.status.success(),
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
#[ignore = "requires Linux namespaces; OSTADIX_ROOTFS_TEST_SUDO=1 explicitly permits VM-local sudo"]
fn one_binary_runs_python_bash_and_multicall_inside_immutable_private_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let python = successful(Command::new("python3").args(["-c", "import json,sys,sysconfig; print(json.dumps([sys.executable,sysconfig.get_path('stdlib')]))"]).output().unwrap());
    let python: Vec<String> = serde_json::from_slice(&python.stdout).unwrap();
    let spec = root.join("closure.json");
    fs::write(
        &spec,
        serde_json::to_vec(&serde_json::json!({
            "schema":"ostadix.runtime-rootfs-closure/v1",
            "commands":{"bash":"/bin/bash","python3":python[0]},
            "paths":[python[1]], "runner":env!("CARGO_BIN_EXE_olangc")
        }))
        .unwrap(),
    )
    .unwrap();
    let bundle = root.join("image");
    successful(
        Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/collect_runtime_rootfs.py"
            ))
            .arg(&spec)
            .arg("--output")
            .arg(&bundle)
            .output()
            .unwrap(),
    );
    let program = root.join("rootfs.O");
    fs::write(
        &program,
        r#"python^(
import os, errno, socket, subprocess, threading, ctypes, glob, time, sys
assert os.environ.get('ROOTFS_HOST_SECRET') is None
assert os.environ['PATH'] == '/bin'
assert socket.gethostname() == 'ostadix'
assert not os.path.exists('/home/ubuntu')
assert not os.path.exists('/proc/1/root/home/ubuntu')
assert os.path.isdir('/proc/self/fd')
assert os.path.isdir('/dev/shm')
with open('/tmp/scratch', 'w') as f: f.write('scratch')
assert open('/tmp/scratch').read() == 'scratch'
try:
    open('/runtime.json', 'w')
    raise AssertionError('payload remained writable')
except OSError as e:
    assert e.errno == errno.EROFS
libc = ctypes.CDLL(None, use_errno=True)
assert libc.mount(None, b'/', None, 0, None) == -1
assert ctypes.get_errno() == errno.EPERM
values = []
threads = [threading.Thread(target=lambda: values.append(1)) for _ in range(4)]
for t in threads: t.start()
for t in threads: t.join()
assert len(values) == 4
assert subprocess.check_output(['bash', '-c', 'printf 7']) == b'7'
for _ in range(8):
    subprocess.run([sys.executable, '-c', 'import os; os.fork(); os._exit(0)'], check=True)
def zombies():
    result = []
    for path in glob.glob('/proc/[0-9]*/stat'):
        try:
            if open(path).read().split(') ', 1)[1].startswith('Z '): result.append(path)
        except FileNotFoundError:
            pass
    return result
for _ in range(100):
    if not zombies(): break
    time.sleep(0.01)
assert not zombies(), zombies()
server = socket.socket()
server.bind(('127.0.0.1', 0)); server.listen(1)
client = socket.socket(); client.connect(server.getsockname())
peer, _ = server.accept(); client.sendall(b'x'); assert peer.recv(1) == b'x'
client.close(); peer.close(); server.close()
external = socket.socket(); external.settimeout(1)
try:
    external.connect(('192.0.2.1', 9))
    raise AssertionError('external route exists')
except OSError as e:
    assert e.errno in (errno.ENETUNREACH, errno.EHOSTUNREACH)
finally:
    external.close()
result = O.eval('bash^(printf 42)_bash')
assert int(result) == 42, repr(result)
42
)_python
"#,
    )
    .unwrap();
    let generated = root.join("generated");
    successful(
        Command::new(env!("CARGO_BIN_EXE_olangc"))
            .arg(&program)
            .args(["-o", "rootfs-program", "--materialize-only"])
            .arg(&generated)
            .arg("--runtime-bundle")
            .arg(&bundle)
            .output()
            .unwrap(),
    );
    fs::remove_dir_all(&bundle).unwrap();
    fs::remove_file(&program).unwrap();
    successful(
        Command::new(env!("CARGO"))
            .args(["build", "--locked", "--offline"])
            .current_dir(&generated)
            .env("CARGO_TARGET_DIR", generated.join("target"))
            .output()
            .unwrap(),
    );
    let executable = generated.join("target/debug/rootfs-program");
    let extractions = root.join("extractions");
    fs::create_dir(&extractions).unwrap();
    let sudo = std::env::var("OSTADIX_ROOTFS_TEST_SUDO").as_deref() == Ok("1");
    let mut command = if sudo {
        let mut command = Command::new("sudo");
        command
            .args(["-n", "env"])
            .arg(format!("TMPDIR={}", extractions.display()))
            .arg("ROOTFS_HOST_SECRET=must-not-cross")
            .arg("PATH=/host-path-does-not-exist")
            .arg(&executable);
        command
    } else {
        let mut command = Command::new(&executable);
        command
            .env("ROOTFS_HOST_SECRET", "must-not-cross")
            .env("PATH", "/host-path-does-not-exist")
            .env("TMPDIR", &extractions);
        command
    };
    let output = successful(command.output().unwrap());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
    assert!(
        fs::read_dir(&extractions).unwrap().next().is_none(),
        "image extraction survived successful exit"
    );

    // An activation marker alone must never bypass the namespace boundary.
    let forged = Command::new(&executable)
        .env("O_EMBEDDED_ROOTFS_IMAGE", "forged")
        .env("TMPDIR", &extractions)
        .output()
        .unwrap();
    assert!(!forged.status.success());
    assert!(
        forged.stdout.is_empty(),
        "program body ran before activation validation"
    );
    assert!(fs::read_dir(&extractions).unwrap().next().is_none());

    if sudo {
        // On a VM whose policy denies unprivileged namespace creation, failure
        // must occur before evaluation and cleanup must still run.
        let denied = Command::new(&executable)
            .env("TMPDIR", &extractions)
            .output()
            .unwrap();
        if !denied.status.success() {
            assert!(denied.stdout.is_empty());
            assert!(String::from_utf8_lossy(&denied.stderr).contains("namespace permission"));
            assert!(fs::read_dir(&extractions).unwrap().next().is_none());
        }
    }

    // Kill the namespace runner with a real signal, then observe its outcome
    // outside the launcher. PID1 must forward the primary wait status exactly.
    fs::write(
        generated.join("src/program.O"),
        "python^(import os, signal\nos.kill(2, signal.SIGTERM))_python\n",
    )
    .unwrap();
    successful(
        Command::new(env!("CARGO"))
            .args(["build", "--locked", "--offline"])
            .current_dir(&generated)
            .env("CARGO_TARGET_DIR", generated.join("target"))
            .output()
            .unwrap(),
    );
    let mut probe = if sudo {
        let mut command = Command::new("sudo");
        command.args(["-n", "python3"]);
        command
    } else {
        Command::new("python3")
    };
    let observed = successful(probe.args(["-c", "import os,subprocess,sys; p=subprocess.run([sys.argv[1]], env={**os.environ,'TMPDIR':sys.argv[2]}, capture_output=True); assert p.returncode == -15, (p.returncode,p.stderr); print('signal-preserved')"])
        .arg(&executable).arg(&extractions).output().unwrap());
    assert_eq!(
        String::from_utf8_lossy(&observed.stdout).trim(),
        "signal-preserved"
    );
    assert!(
        fs::read_dir(&extractions).unwrap().next().is_none(),
        "image survived child signal"
    );
}
