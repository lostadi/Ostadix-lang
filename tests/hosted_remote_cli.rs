//! User-facing loopback gate for the bounded hosted-node preview.

use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod support;

struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn client_command(action: &str, address: &str, pki: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_octl"));
    command
        .arg("node")
        .arg(action)
        .arg("--address")
        .arg(address)
        .arg("--server-name")
        .arg("localhost")
        .arg("--ca")
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"));
    command
}

fn diagnostic(output: &Output, server_stderr: &Path) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}\nserver stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(server_stderr).unwrap_or_else(|error| error.to_string())
    )
}

#[test]
fn provision_profile_doctor_and_run_are_usable_end_to_end() {
    if !support::require_runtimes(&["openssl", "python3"]) {
        return;
    }

    // Referencing the Cargo-provided O binary also makes the default o-node
    // sibling resolver part of this integration gate.
    assert!(Path::new(env!("CARGO_BIN_EXE_O")).is_file());
    let root = tempfile::tempdir().unwrap();
    let pki = root.path().join("pki");
    let server_stderr = root.path().join("server.stderr");
    let provision = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args(["pki", "init", "--directory"])
        .arg(&pki)
        .args(["--server-name", "localhost"])
        .output()
        .unwrap();
    assert!(
        provision.status.success(),
        "PKI provisioning failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provision.stdout),
        String::from_utf8_lossy(&provision.stderr)
    );

    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    let stderr_file = fs::File::create(&server_stderr).unwrap();
    let server = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .arg("serve")
        .arg("--node-id")
        .arg("hosted-cli-test")
        .arg("--shim-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--bind")
        .arg(&address)
        .arg("--cert")
        .arg(pki.join("node-cert.pem"))
        .arg("--key")
        .arg(pki.join("node-key.pem"))
        .arg("--client-ca")
        .arg(pki.join("ca.pem"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap();
    let mut server = ServerGuard(server);

    let deadline = Instant::now() + Duration::from_secs(10);
    let profile = loop {
        let output = client_command("profile", &address, &pki).output().unwrap();
        if output.status.success() {
            break output;
        }
        if let Some(status) = server.0.try_wait().unwrap() {
            panic!(
                "o-node exited before profile with {status}\n{}",
                diagnostic(&output, &server_stderr)
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for o-node\n{}",
            diagnostic(&output, &server_stderr)
        );
        thread::sleep(Duration::from_millis(50));
    };
    let profile: serde_json::Value = serde_json::from_slice(&profile.stdout).unwrap();
    assert_eq!(profile["node_id"], "hosted-cli-test");
    assert_eq!(profile["transport"], "tcp+tls1.3+mutual-x509");

    let doctor = client_command("doctor", &address, &pki).output().unwrap();
    assert!(
        doctor.status.success(),
        "{}",
        diagnostic(&doctor, &server_stderr)
    );
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["ready"], true);
    assert!(doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| { check["name"] == "native-runtime-image-valid" && check["ok"] == true }));

    let mut run = client_command("run", &address, &pki);
    run.args(["--task-id", "hosted-cli-task"])
        .args(["--attempt-id", "hosted-cli-attempt"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hello.O"));
    let receipt = run.output().unwrap();
    assert!(
        receipt.status.success(),
        "{}",
        diagnostic(&receipt, &server_stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&receipt.stdout).unwrap();
    assert_eq!(receipt["task_id"], "hosted-cli-task");
    assert_eq!(receipt["attempt_id"], "hosted-cli-attempt");
    assert_eq!(receipt["outcome"]["status"], "succeeded");
    assert_eq!(receipt["outcome"]["value"]["t"], "number");
    assert_eq!(receipt["outcome"]["value"]["v"]["v"], "2");
    assert_eq!(receipt["receipt_sha256"].as_str().unwrap().len(), 64);
}
