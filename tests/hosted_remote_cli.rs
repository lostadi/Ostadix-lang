//! User-facing loopback gate for the bounded hosted-node preview.

use std::fs;
use std::net::TcpListener;
#[cfg(unix)]
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

mod support;

struct ServerGuard(Option<Child>);

impl ServerGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.0
            .as_mut()
            .expect("server guard no longer owns a child")
            .try_wait()
    }

    fn request_graceful_shutdown(&mut self) -> std::io::Result<()> {
        let child = self
            .0
            .as_mut()
            .expect("server guard no longer owns a child");
        #[cfg(unix)]
        {
            // SAFETY: this sends SIGTERM only to the exact child PID still
            // owned by this guard; the child has not been reaped or detached.
            if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
        #[cfg(not(unix))]
        child.kill()?;
        Ok(())
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                self.0.take();
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn shutdown_gracefully(&mut self) -> ExitStatus {
        self.request_graceful_shutdown().unwrap();
        if let Some(status) = self.wait_for_exit(Duration::from_secs(10)).unwrap() {
            return status;
        }
        self.force_kill_and_wait();
        panic!("o-node did not complete graceful shutdown within 10 seconds");
    }

    fn force_kill_and_wait(&mut self) -> ExitStatus {
        let mut child = self.0.take().expect("server child was already reaped");
        let _ = child.kill();
        child.wait().unwrap()
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if self.0.is_none() {
            return;
        }
        let _ = self.request_graceful_shutdown();
        if !matches!(self.wait_for_exit(Duration::from_secs(10)), Ok(Some(_))) {
            let _ = self.force_kill_and_wait();
        }
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
    let mut server = ServerGuard::new(server);

    let deadline = Instant::now() + Duration::from_secs(10);
    let profile = loop {
        let output = client_command("profile", &address, &pki).output().unwrap();
        if output.status.success() {
            break output;
        }
        if let Some(status) = server.try_wait().unwrap() {
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

#[test]
fn durable_v2_open_preserves_precommitted_capability_for_exact_post_send_retry() {
    exercise_durable_v2_dev_flow(false);
}

#[test]
fn durable_v2_dev_mint_open_execute_status_and_close_are_usable_end_to_end() {
    exercise_durable_v2_dev_flow(true);
}

fn exercise_durable_v2_dev_flow(continue_through_execute: bool) {
    if !support::require_runtimes(&["openssl", "python3"]) {
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let pki = root.path().join("pki");
    let state = root.path().join("state");
    let authority = root.path().join("authority");
    let source = root.path().join("persistent.O");
    let open_lease = root.path().join("open.json");
    let execute_lease = root.path().join("execute.json");
    let prepared_operation = root.path().join("operation.json");
    let capability = root.path().join("session.json");
    let wrong_node_receipt_key = root.path().join("wrong-node-signing-public.v2");
    let server_stderr = root.path().join("server-v2.stderr");
    fs::write(
        &source,
        "python[7]^(\n__oval_result__ = 1 + 1\n)_python[7]\n",
    )
    .unwrap();

    let provision = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args(["pki", "init", "--directory"])
        .arg(&pki)
        .args(["--server-name", "localhost"])
        .output()
        .unwrap();
    assert!(
        provision.status.success(),
        "V2 PKI provisioning failed: {}",
        String::from_utf8_lossy(&provision.stderr)
    );
    let identity = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args(["identity", "init", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        identity.status.success(),
        "V2 identity provisioning failed: {}",
        String::from_utf8_lossy(&identity.stderr)
    );
    let authority_init = Command::new(env!("CARGO_BIN_EXE_octl"))
        .args(["node", "authority", "init", "--directory"])
        .arg(&authority)
        .output()
        .unwrap();
    assert!(
        authority_init.status.success(),
        "V2 authority provisioning failed: {}",
        String::from_utf8_lossy(&authority_init.stderr)
    );

    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    let stderr_file = fs::File::create(&server_stderr).unwrap();
    let server = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args(["serve", "--node-id", "hosted-v2-cli-test", "--shim-dir"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--bind")
        .arg(&address)
        .arg("--cert")
        .arg(pki.join("node-cert.pem"))
        .arg("--key")
        .arg(pki.join("node-key.pem"))
        .arg("--client-ca")
        .arg(pki.join("ca.pem"))
        .arg("--v2-state-dir")
        .arg(&state)
        .arg("--v2-authority-public-key")
        .arg(authority.join("placement-public-key.v2"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap();
    let mut server = ServerGuard::new(server);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = client_command("profile", &address, &pki).output().unwrap();
        if output.status.success() {
            break;
        }
        if let Some(status) = server.try_wait().unwrap() {
            panic!(
                "V2 node exited before profile with {status}\n{}",
                diagnostic(&output, &server_stderr)
            );
        }
        assert!(
            Instant::now() < deadline,
            "V2 node readiness timed out\n{}",
            diagnostic(&output, &server_stderr)
        );
        thread::sleep(Duration::from_millis(50));
    }

    let open = Command::new(env!("CARGO_BIN_EXE_octl"))
        .args(["node", "authority", "dev-mint", "open", "--signing-key"])
        .arg(authority.join("placement-signing-key.v2"))
        .arg("--shim-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--source")
        .arg(&source)
        .args([
            "--node-id",
            "hosted-v2-cli-test",
            "--state-tier",
            "checkpoint-restore",
            "--client-cert",
        ])
        .arg(pki.join("client-cert.pem"))
        .arg("--capability-out")
        .arg(&capability)
        .arg("--out")
        .arg(&open_lease)
        .output()
        .unwrap();
    assert!(
        open.status.success(),
        "dev-mint open failed\n{}",
        diagnostic(&open, &server_stderr)
    );

    fs::write(&wrong_node_receipt_key, format!("{}\n", "00".repeat(32))).unwrap();
    let mut ambiguous_open = Command::new(env!("CARGO_BIN_EXE_octl"));
    ambiguous_open
        .args(["node", "session", "open", "--address"])
        .arg(&address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(&wrong_node_receipt_key)
        .arg("--lease")
        .arg(&open_lease)
        .arg("--capability")
        .arg(&capability);
    let ambiguous = ambiguous_open.output().unwrap();
    assert!(
        !ambiguous.status.success(),
        "wrong receipt pin unexpectedly accepted OpenSession\n{}",
        diagnostic(&ambiguous, &server_stderr)
    );
    assert!(
        capability.is_file(),
        "ambiguous Open removed retry capability"
    );
    let ambiguous_stderr = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(
        ambiguous_stderr.contains("capability retained")
            && ambiguous_stderr.contains("retry the exact same lease and capability"),
        "ambiguous Open did not provide exact-retry guidance: {ambiguous_stderr}"
    );

    let mut pre_send_retry = Command::new(env!("CARGO_BIN_EXE_octl"));
    pre_send_retry
        .args(["node", "session", "open", "--address", "127.0.0.1:0"])
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--lease")
        .arg(&open_lease)
        .arg("--capability")
        .arg(&capability);
    let pre_send = pre_send_retry.output().unwrap();
    assert!(!pre_send.status.success());
    assert!(
        capability.is_file(),
        "a pre-send retry failure deleted the bearer for an already-committed session"
    );
    assert!(
        String::from_utf8_lossy(&pre_send.stderr).contains("capability retained"),
        "pre-send retry did not explain capability retention: {}",
        String::from_utf8_lossy(&pre_send.stderr)
    );

    let mut session_open = Command::new(env!("CARGO_BIN_EXE_octl"));
    session_open
        .args(["node", "session", "open", "--address"])
        .arg(&address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--lease")
        .arg(&open_lease)
        .arg("--capability")
        .arg(&capability);
    let opened = session_open.output().unwrap();
    assert!(
        opened.status.success(),
        "session open failed\n{}",
        diagnostic(&opened, &server_stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&capability).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    if !continue_through_execute {
        return;
    }

    let mut mint_execute = Command::new(env!("CARGO_BIN_EXE_octl"));
    mint_execute
        .args(["node", "authority", "dev-mint", "execute", "--signing-key"])
        .arg(authority.join("placement-signing-key.v2"))
        .arg("--shim-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--open-lease")
        .arg(&open_lease)
        .arg("--source")
        .arg(&source)
        .args([
            "--operation-id",
            "cli-v2-op",
            "--task-sha256",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "--address",
        ])
        .arg(&address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--capability")
        .arg(&capability)
        .arg("--operation-out")
        .arg(&prepared_operation)
        .arg("--out")
        .arg(&execute_lease);
    let minted = mint_execute.output().unwrap();
    assert!(
        minted.status.success(),
        "dev-mint execute failed\n{}",
        diagnostic(&minted, &server_stderr)
    );
    let first_execute_lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&execute_lease).unwrap()).unwrap();
    assert!(
        first_execute_lease["command"]["actor_generation"].is_null(),
        "first stateful Execute must let the node establish its physical actor: {first_execute_lease}"
    );

    let mut execute = Command::new(env!("CARGO_BIN_EXE_octl"));
    execute
        .args(["node", "session", "exec", "--address"])
        .arg(&address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--capability")
        .arg(&capability)
        .arg("--prepared-operation")
        .arg(&prepared_operation)
        .arg("--lease")
        .arg(&execute_lease);
    let submitted = execute.output().unwrap();
    assert!(
        submitted.status.success(),
        "session execute failed\n{}",
        diagnostic(&submitted, &server_stderr)
    );

    let status_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut status = Command::new(env!("CARGO_BIN_EXE_octl"));
        status
            .args(["node", "session", "status", "--address"])
            .arg(&address)
            .args(["--server-name", "localhost", "--ca"])
            .arg(pki.join("ca.pem"))
            .arg("--cert")
            .arg(pki.join("client-cert.pem"))
            .arg("--key")
            .arg(pki.join("client-key.pem"))
            .arg("--node-receipt-public-key")
            .arg(state.join("node-signing-public.v2"))
            .arg("--capability")
            .arg(&capability)
            .args(["--operation-id", "cli-v2-op"]);
        let output = status.output().unwrap();
        assert!(
            output.status.success(),
            "session status failed\n{}",
            diagnostic(&output, &server_stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let operation_status = &value["session"]["operations"]["cli-v2-op"]["status"];
        if operation_status == "succeeded" {
            assert!(
                !value["session"]["actor"]["actor_generation"].is_null(),
                "successful first stateful Execute did not establish an actor: {value}"
            );
            break;
        }
        assert_ne!(operation_status, "failed", "operation failed: {value}");
        assert!(
            Instant::now() < status_deadline,
            "V2 operation did not settle: {value}"
        );
        thread::sleep(Duration::from_millis(25));
    }

    let mut close = Command::new(env!("CARGO_BIN_EXE_octl"));
    close
        .args(["node", "session", "close", "--address"])
        .arg(&address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--capability")
        .arg(&capability);
    let closed = close.output().unwrap();
    assert!(
        closed.status.success(),
        "session close failed\n{}",
        diagnostic(&closed, &server_stderr)
    );
}

#[test]
fn durable_v2_integrated_dev_submit_and_checkpoint_recovery_are_usable_end_to_end() {
    if !support::require_runtimes(&["openssl", "python3"]) {
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let pki = root.path().join("pki");
    let state = root.path().join("state");
    let authority = root.path().join("authority");
    let source = root.path().join("persistent.O");
    let slow_source = root.path().join("persistent-slow.O");
    let open_lease = root.path().join("open.json");
    let first_lease = root.path().join("first-execute.json");
    let first_operation = root.path().join("first-operation.json");
    let ambiguous_lease = root.path().join("ambiguous-execute.json");
    let ambiguous_operation = root.path().join("ambiguous-operation.json");
    let recovery_warrant = root.path().join("recovery-warrant.json");
    let recovery_lease = root.path().join("recovery-lease.json");
    let graceful_operation = root.path().join("graceful-operation.json");
    let graceful_lease = root.path().join("graceful-execute.json");
    let capability = root.path().join("session.json");
    let server_stderr = root.path().join("server-integrated.stderr");
    let restarted_stderr = root.path().join("server-integrated-restarted.stderr");
    let graceful_stderr = root.path().join("server-integrated-graceful.stderr");
    fs::write(
        &source,
        "python[7]^(\n__oval_result__ = 1 + 1\n)_python[7]\n",
    )
    .unwrap();
    fs::write(
        &slow_source,
        "python[7]^(\nimport time\ntime.sleep(3)\n__oval_result__ = 3\n)_python[7]\n",
    )
    .unwrap();

    let provision = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args(["pki", "init", "--directory"])
        .arg(&pki)
        .args(["--server-name", "localhost"])
        .output()
        .unwrap();
    assert!(
        provision.status.success(),
        "V2 PKI provisioning failed: {}",
        String::from_utf8_lossy(&provision.stderr)
    );
    let identity = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args(["identity", "init", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        identity.status.success(),
        "V2 identity provisioning failed: {}",
        String::from_utf8_lossy(&identity.stderr)
    );
    let authority_init = Command::new(env!("CARGO_BIN_EXE_octl"))
        .args(["node", "authority", "init", "--directory"])
        .arg(&authority)
        .output()
        .unwrap();
    assert!(
        authority_init.status.success(),
        "V2 authority provisioning failed: {}",
        String::from_utf8_lossy(&authority_init.stderr)
    );

    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    let mut server = spawn_v2_cli_server(&address, &pki, &state, &authority, &server_stderr);
    wait_for_v2_cli_server(&address, &pki, &server_stderr, &mut server);

    let mut open = Command::new(env!("CARGO_BIN_EXE_octl"));
    open.args(["node", "authority", "dev-mint", "open", "--signing-key"])
        .arg(authority.join("placement-signing-key.v2"))
        .arg("--shim-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--source")
        .arg(&source)
        .args([
            "--node-id",
            "hosted-v2-integrated-cli-test",
            "--state-tier",
            "checkpoint-restore",
            "--client-cert",
        ])
        .arg(pki.join("client-cert.pem"))
        .arg("--capability-out")
        .arg(&capability)
        .arg("--out")
        .arg(&open_lease)
        .arg("--submit")
        .arg("--address")
        .arg(&address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"));
    let opened = open.output().unwrap();
    assert!(
        opened.status.success(),
        "integrated dev OpenSession failed\n{}",
        diagnostic(&opened, &server_stderr)
    );
    let opened_json: serde_json::Value = serde_json::from_slice(&opened.stdout).unwrap();
    assert_eq!(opened_json["entry"]["event"]["event"], "session_opened");
    assert!(open_lease.is_file() && capability.is_file());

    let mut first = dev_execute_command(
        &address,
        &pki,
        &state,
        &authority,
        &capability,
        &open_lease,
        &source,
        "integrated-first",
        "2222222222222222222222222222222222222222222222222222222222222222",
        &first_operation,
        &first_lease,
    );
    first.arg("--submit");
    let submitted = first.output().unwrap();
    assert!(
        submitted.status.success(),
        "integrated first Execute failed\n{}",
        diagnostic(&submitted, &server_stderr)
    );
    let submitted_json: serde_json::Value = serde_json::from_slice(&submitted.stdout).unwrap();
    assert_eq!(submitted_json["response"], "committed");
    assert_eq!(
        submitted_json["receipt"]["entry"]["event"]["event"],
        "operation_accepted"
    );
    let ready = wait_for_operation_status(
        &address,
        &pki,
        &state,
        &capability,
        "integrated-first",
        "succeeded",
        &server_stderr,
    );
    let first_actor = ready["session"]["actor"]["actor_generation"].clone();
    assert!(!first_actor.is_null());
    assert!(!ready["session"]["actor"]["checkpoint_sha256"].is_null());

    let mut ambiguous = dev_execute_command(
        &address,
        &pki,
        &state,
        &authority,
        &capability,
        &open_lease,
        &slow_source,
        "integrated-ambiguous",
        "3333333333333333333333333333333333333333333333333333333333333333",
        &ambiguous_operation,
        &ambiguous_lease,
    );
    ambiguous.arg("--submit");
    let accepted = ambiguous.output().unwrap();
    assert!(
        accepted.status.success(),
        "integrated ambiguous Execute admission failed\n{}",
        diagnostic(&accepted, &server_stderr)
    );
    let accepted_json: serde_json::Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(accepted_json["response"], "committed");
    assert_eq!(
        accepted_json["receipt"]["entry"]["event"]["event"],
        "operation_accepted"
    );
    wait_for_operation_status(
        &address,
        &pki,
        &state,
        &capability,
        "integrated-ambiguous",
        "running",
        &server_stderr,
    );

    server.force_kill_and_wait();
    server = spawn_v2_cli_server(&address, &pki, &state, &authority, &restarted_stderr);
    wait_for_v2_cli_server(&address, &pki, &restarted_stderr, &mut server);
    let recovery_required = wait_for_operation_status(
        &address,
        &pki,
        &state,
        &capability,
        "integrated-ambiguous",
        "ambiguous",
        &restarted_stderr,
    );
    assert_eq!(recovery_required["session"]["status"], "recovery_required");
    assert_eq!(
        recovery_required["session"]["actor"]["actor_generation"],
        first_actor
    );

    let mut recover = Command::new(env!("CARGO_BIN_EXE_octl"));
    recover
        .args(["node", "authority", "dev-mint", "recover", "--signing-key"])
        .arg(authority.join("placement-signing-key.v2"))
        .arg("--shim-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--open-lease")
        .arg(&open_lease)
        .arg("--source")
        .arg(&source)
        .args([
            "--operation-id",
            "integrated-ambiguous",
            "--replay-class",
            "pure",
            "--address",
        ])
        .arg(&address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--capability")
        .arg(&capability)
        .arg("--warrant-out")
        .arg(&recovery_warrant)
        .arg("--out")
        .arg(&recovery_lease)
        .arg("--submit");
    let recovered = recover.output().unwrap();
    assert!(
        recovered.status.success(),
        "integrated checkpoint Recover failed\n{}",
        diagnostic(&recovered, &restarted_stderr)
    );
    let recovered_json: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(recovered_json["response"], "committed");
    assert_eq!(
        recovered_json["receipt"]["entry"]["event"]["event"],
        "recovery_committed"
    );
    assert!(recovery_warrant.is_file() && recovery_lease.is_file());
    let recovered_status = wait_for_operation_status(
        &address,
        &pki,
        &state,
        &capability,
        "integrated-ambiguous",
        "failed",
        &restarted_stderr,
    );
    assert_eq!(recovered_status["session"]["status"], "ready");
    assert_ne!(
        recovered_status["session"]["actor"]["actor_generation"],
        first_actor
    );

    let mut graceful = dev_execute_command(
        &address,
        &pki,
        &state,
        &authority,
        &capability,
        &open_lease,
        &slow_source,
        "integrated-graceful-drain",
        "4444444444444444444444444444444444444444444444444444444444444444",
        &graceful_operation,
        &graceful_lease,
    );
    graceful.arg("--submit");
    let accepted = graceful.output().unwrap();
    assert!(
        accepted.status.success(),
        "integrated graceful-drain Execute admission failed\n{}",
        diagnostic(&accepted, &restarted_stderr)
    );
    wait_for_operation_status(
        &address,
        &pki,
        &state,
        &capability,
        "integrated-graceful-drain",
        "running",
        &restarted_stderr,
    );

    let graceful_exit = server.shutdown_gracefully();
    #[cfg(unix)]
    assert!(
        graceful_exit.success(),
        "first SIGTERM must complete the graceful V2 barrier: {graceful_exit}"
    );
    #[cfg(not(unix))]
    let _ = graceful_exit;
    server = spawn_v2_cli_server(&address, &pki, &state, &authority, &graceful_stderr);
    wait_for_v2_cli_server(&address, &pki, &graceful_stderr, &mut server);
    let settled = wait_for_operation_terminal_status(
        &address,
        &pki,
        &state,
        &capability,
        "integrated-graceful-drain",
        &graceful_stderr,
    );
    assert_ne!(
        settled["session"]["operations"]["integrated-graceful-drain"]["status"], "ambiguous",
        "graceful shutdown must settle accepted work before releasing the root"
    );

    let mut close = session_command("close", &address, &pki, &state, &capability);
    let closed = close.output().unwrap();
    assert!(
        closed.status.success(),
        "integrated session close failed\n{}",
        diagnostic(&closed, &graceful_stderr)
    );

    #[cfg(unix)]
    {
        // Keep one accepted TLS worker blocked before it can submit a request.
        // The first signal must therefore remain in its join/drain barrier long
        // enough for a second signal to exercise the explicit force policy.
        let _blocked_connection = TcpStream::connect(&address).unwrap();
        thread::sleep(Duration::from_millis(100));
        server.request_graceful_shutdown().unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(
            server.try_wait().unwrap().is_none(),
            "blocked connection did not hold the graceful join barrier"
        );
        server.request_graceful_shutdown().unwrap();
        let forced = server
            .wait_for_exit(Duration::from_secs(5))
            .unwrap()
            .expect("second SIGTERM did not force o-node termination");
        assert_eq!(forced.signal(), Some(libc::SIGTERM));
    }
}

fn spawn_v2_cli_server(
    address: &str,
    pki: &Path,
    state: &Path,
    authority: &Path,
    stderr_path: &Path,
) -> ServerGuard {
    let stderr_file = fs::File::create(stderr_path).unwrap();
    let server = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args([
            "serve",
            "--node-id",
            "hosted-v2-integrated-cli-test",
            "--shim-dir",
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--bind")
        .arg(address)
        .arg("--cert")
        .arg(pki.join("node-cert.pem"))
        .arg("--key")
        .arg(pki.join("node-key.pem"))
        .arg("--client-ca")
        .arg(pki.join("ca.pem"))
        .arg("--v2-state-dir")
        .arg(state)
        .arg("--v2-authority-public-key")
        .arg(authority.join("placement-public-key.v2"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap();
    ServerGuard::new(server)
}

fn wait_for_v2_cli_server(
    address: &str,
    pki: &Path,
    server_stderr: &Path,
    server: &mut ServerGuard,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = client_command("profile", address, pki).output().unwrap();
        if output.status.success() {
            return;
        }
        if let Some(status) = server.try_wait().unwrap() {
            panic!(
                "V2 node exited before profile with {status}\n{}",
                diagnostic(&output, server_stderr)
            );
        }
        assert!(
            Instant::now() < deadline,
            "V2 node readiness timed out\n{}",
            diagnostic(&output, server_stderr)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[allow(clippy::too_many_arguments)]
fn dev_execute_command(
    address: &str,
    pki: &Path,
    state: &Path,
    authority: &Path,
    capability: &Path,
    open_lease: &Path,
    source: &Path,
    operation_id: &str,
    task_sha256: &str,
    operation_out: &Path,
    lease_out: &Path,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_octl"));
    command
        .args(["node", "authority", "dev-mint", "execute", "--signing-key"])
        .arg(authority.join("placement-signing-key.v2"))
        .arg("--shim-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--open-lease")
        .arg(open_lease)
        .arg("--source")
        .arg(source)
        .args(["--operation-id", operation_id, "--task-sha256", task_sha256])
        .arg("--address")
        .arg(address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--capability")
        .arg(capability)
        .arg("--operation-out")
        .arg(operation_out)
        .arg("--out")
        .arg(lease_out);
    command
}

fn session_command(
    action: &str,
    address: &str,
    pki: &Path,
    state: &Path,
    capability: &Path,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_octl"));
    command
        .args(["node", "session", action, "--address"])
        .arg(address)
        .args(["--server-name", "localhost", "--ca"])
        .arg(pki.join("ca.pem"))
        .arg("--cert")
        .arg(pki.join("client-cert.pem"))
        .arg("--key")
        .arg(pki.join("client-key.pem"))
        .arg("--node-receipt-public-key")
        .arg(state.join("node-signing-public.v2"))
        .arg("--capability")
        .arg(capability);
    command
}

fn wait_for_operation_status(
    address: &str,
    pki: &Path,
    state: &Path,
    capability: &Path,
    operation_id: &str,
    expected: &str,
    server_stderr: &Path,
) -> serde_json::Value {
    wait_for_operation_statuses(
        address,
        pki,
        state,
        capability,
        operation_id,
        &[expected],
        server_stderr,
    )
}

fn wait_for_operation_terminal_status(
    address: &str,
    pki: &Path,
    state: &Path,
    capability: &Path,
    operation_id: &str,
    server_stderr: &Path,
) -> serde_json::Value {
    wait_for_operation_statuses(
        address,
        pki,
        state,
        capability,
        operation_id,
        &["succeeded", "failed"],
        server_stderr,
    )
}

fn wait_for_operation_statuses(
    address: &str,
    pki: &Path,
    state: &Path,
    capability: &Path,
    operation_id: &str,
    expected: &[&str],
    server_stderr: &Path,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut status = session_command("status", address, pki, state, capability);
        status.args(["--operation-id", operation_id]);
        let output = status.output().unwrap();
        assert!(
            output.status.success(),
            "session status failed\n{}",
            diagnostic(&output, server_stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let operation_status = value["session"]["operations"][operation_id]["status"]
            .as_str()
            .unwrap_or("missing");
        if expected.contains(&operation_status) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "operation `{operation_id}` did not reach one of {expected:?}; last status: {value}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
