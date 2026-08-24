//! Black-box loopback gate for the o-link project mesh.

use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use o_lang::hosted_remote::{
    observe_mesh_peers_read_only, store_paired_lan_peer, ClientTlsIdentity, MeshNodeClient,
    MeshReadOnlyDiscoveryConfig, StoredLanPeerPathsV1,
};

mod support;

struct NodeGuard {
    child: Option<Child>,
    stderr: PathBuf,
}

impl NodeGuard {
    fn shutdown(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        #[cfg(unix)]
        {
            // SAFETY: the guard owns this exact unreaped child process.
            let _ = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        }
        #[cfg(not(unix))]
        let _ = child.kill();

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.child.take();
                    return;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                _ => break,
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn diagnostics(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_else(|error| error.to_string())
    }
}

impl Drop for NodeGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct TestNode {
    node_id: String,
    address: SocketAddr,
    max_connections: u32,
    pki: PathBuf,
    state: PathBuf,
    guard: NodeGuard,
}

impl TestNode {
    fn client_identity(&self) -> ClientTlsIdentity {
        ClientTlsIdentity {
            ca_path: self.pki.join("ca.pem"),
            cert_path: self.pki.join("client-cert.pem"),
            key_path: self.pki.join("client-key.pem"),
            server_name: "localhost".to_owned(),
        }
    }

    fn wait_for_full_capacity(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let full = MeshNodeClient::new(
                self.address.to_string(),
                self.client_identity(),
                Duration::from_millis(250),
                Duration::from_secs(2),
            )
            .capacity()
            .is_ok_and(|capacity| {
                capacity.available_slots == self.max_connections && capacity.active_actors == 0
            });
            if full {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "mesh node {} did not return to full capacity\n{}",
                self.node_id,
                self.guard.diagnostics()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}

fn reserve_loopback() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

fn command_output(mut command: Command, label: &str) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn provision_node(root: &Path, node_id: &str, max_connections: usize, node_b: bool) -> TestNode {
    let node_root = root.join(node_id);
    let pki = node_root.join("pki");
    let state = node_root.join("state");
    let authority = node_root.join("authority");
    let stderr = node_root.join("node.stderr");
    fs::create_dir_all(&node_root).unwrap();

    let mut pki_init = Command::new(env!("CARGO_BIN_EXE_o-node"));
    pki_init
        .args(["pki", "init", "--directory"])
        .arg(&pki)
        .args(["--server-name", "localhost"]);
    command_output(pki_init, "mesh PKI provisioning");

    let mut identity_init = Command::new(env!("CARGO_BIN_EXE_o-node"));
    identity_init
        .args(["identity", "init", "--state-dir"])
        .arg(&state);
    command_output(identity_init, "mesh node identity provisioning");

    let mut authority_init = Command::new(env!("CARGO_BIN_EXE_octl"));
    authority_init
        .args(["node", "authority", "init", "--directory"])
        .arg(&authority);
    command_output(authority_init, "mesh placement authority provisioning");

    let address = reserve_loopback();
    let stderr_file = fs::File::create(&stderr).unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_o-node"));
    server
        .args(["serve", "--manual", "--node-id", node_id, "--shim-dir"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .arg("--runtime-binary")
        .arg(env!("CARGO_BIN_EXE_O"))
        .arg("--bind")
        .arg(address.to_string())
        .arg("--cert")
        .arg(pki.join("node-cert.pem"))
        .arg("--key")
        .arg(pki.join("node-key.pem"))
        .arg("--client-ca")
        .arg(pki.join("ca.pem"))
        .arg("--v2-state-dir")
        .arg(&state)
        .arg("--mesh-state-dir")
        .arg(state.join("mesh-v1"))
        .arg("--v2-authority-public-key")
        .arg(authority.join("placement-public-key.v2"))
        .arg("--max-connections")
        .arg(max_connections.to_string())
        .env("MESH_NODE", node_id)
        .env("MESH_BARRIER_DIR", root.join("barrier"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file));
    if node_b {
        server.env("ONLY_NODE_B", "yes");
    } else {
        server.env_remove("ONLY_NODE_B");
    }
    let child = server.spawn().unwrap();
    let mut node = TestNode {
        node_id: node_id.to_owned(),
        address,
        max_connections: u32::try_from(max_connections).unwrap(),
        pki,
        state,
        guard: NodeGuard {
            child: Some(child),
            stderr,
        },
    };

    let identity = node.client_identity();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let ready = MeshNodeClient::new(
            node.address.to_string(),
            identity.clone(),
            Duration::from_millis(250),
            Duration::from_secs(2),
        )
        .profile()
        .is_ok_and(|profile| profile.node_id == node.node_id);
        if ready {
            break;
        }
        if node
            .guard
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_some()
        {
            panic!(
                "mesh node {} exited during startup\n{}",
                node.node_id,
                node.guard.diagnostics()
            );
        }
        assert!(
            Instant::now() < deadline,
            "mesh node {} did not become ready\n{}",
            node.node_id,
            node.guard.diagnostics()
        );
        thread::sleep(Duration::from_millis(50));
    }
    node
}

fn register_node(peer_root: &Path, node: &TestNode) {
    let ca = fs::read_to_string(node.pki.join("ca.pem")).unwrap();
    let client_cert = fs::read_to_string(node.pki.join("client-cert.pem")).unwrap();
    let client_key = fs::read(node.pki.join("client-key.pem")).unwrap();
    let receipt_key = fs::read_to_string(node.state.join("node-signing-public.v2")).unwrap();
    store_paired_lan_peer(
        peer_root,
        node.address,
        &node.node_id,
        "localhost",
        node.address.port(),
        true,
        &ca,
        &client_cert,
        &client_key,
        Some(receipt_key.trim()),
    )
    .unwrap();
    let paths = StoredLanPeerPathsV1::for_root(peer_root, &node.node_id).unwrap();
    assert!(paths.metadata.is_file());
}

fn run_o_cli_mesh_no_record(
    project: &Path,
    peer_root: &Path,
    trace: &Path,
    state: &Path,
    args: &[&str],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command
        .arg("run")
        .arg(project)
        .args(args)
        .arg("--mesh-peer-root")
        .arg(peer_root)
        .arg("--closed-registry")
        .arg("--mesh-trace-out")
        .arg(trace)
        .args(["--no-record", "--json"])
        .env("XDG_STATE_HOME", state);
    command.output().unwrap()
}

fn run_o_cli_auto(project: &Path, peer_root: &Path, trace: &Path, state: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command
        .args(["run"])
        .arg(project)
        .args([
            "--parallel",
            "auto",
            "--route",
            "parallel",
            "--routes-policy",
            "all",
            "--mesh-retries=0",
            "--closed-registry",
        ])
        .arg("--mesh-peer-root")
        .arg(peer_root)
        .arg("--mesh-trace-out")
        .arg(trace)
        .env("XDG_STATE_HOME", state);
    command.output().unwrap()
}

fn run_o_cli_required_retry(
    project: &Path,
    peer_root: &Path,
    trace: &Path,
    state: &Path,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command
        .args(["run"])
        .arg(project)
        .args([
            "--route",
            "retry",
            "--mesh=required",
            "--mesh-retries=1",
            "--closed-registry",
        ])
        .arg("--mesh-peer-root")
        .arg(peer_root)
        .arg("--mesh-trace-out")
        .arg(trace)
        .env("XDG_STATE_HOME", state);
    command.output().unwrap()
}

fn run_o_cli_evidence(state: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command.env("XDG_STATE_HOME", state).args(args);
    command.output().unwrap()
}

fn read_trace(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn visit(root: &Path, path: &Path, snapshot: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        if !path.exists() {
            return;
        }
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        if path.is_dir() {
            snapshot.push((relative, None));
            let mut entries = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for entry in entries {
                visit(root, &entry, snapshot);
            }
        } else {
            snapshot.push((relative, Some(fs::read(path).unwrap())));
        }
    }

    let mut snapshot = Vec::new();
    visit(root, root, &mut snapshot);
    snapshot
}

#[test]
fn two_node_mesh_parallel_retry_and_local_fallback_are_end_to_end() {
    if !support::require_runtimes(&["bash", "openssl"]) {
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let peer_root = root.path().join("peers");
    let empty_peer_root = root.path().join("empty-peers");
    let project = root.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(root.path().join("barrier")).unwrap();
    fs::write(
        project.join("olang.project.toml"),
        r#"
[project]
name = "mesh-e2e"

[[routes]]
id = "left"
command = ["bash", "-c", "marker=\"$MESH_BARRIER_DIR/$MESH_NODE\"; : > \"$marker\"; spins=0; while [ ! -f \"$MESH_BARRIER_DIR/mesh-node-a\" ] || [ ! -f \"$MESH_BARRIER_DIR/mesh-node-b\" ]; do spins=$((spins + 1)); if [ \"$spins\" -ge 200 ]; then exit 91; fi; sleep 0.05; done; printf 'parallel:%s\\n' \"$MESH_NODE\""]

[[routes]]
id = "right"
command = ["bash", "-c", "marker=\"$MESH_BARRIER_DIR/$MESH_NODE\"; : > \"$marker\"; spins=0; while [ ! -f \"$MESH_BARRIER_DIR/mesh-node-a\" ] || [ ! -f \"$MESH_BARRIER_DIR/mesh-node-b\" ]; do spins=$((spins + 1)); if [ \"$spins\" -ge 200 ]; then exit 91; fi; sleep 0.05; done; printf 'parallel:%s\\n' \"$MESH_NODE\""]

[[routes]]
id = "retry"
command = ["bash", "-c", "if [ \"$MESH_NODE\" = mesh-node-a ]; then printf 'generation-one-failed\\n' >&2; exit 75; fi; printf 'retry:%s\\n' \"$MESH_NODE\""]
failure_continuation = "declared_idempotent"

[[routes]]
id = "retry-denied"
command = ["bash", "-c", "printf 'unproven-effects\\n' >&2; exit 76"]

[[routes]]
id = "local"
command = ["bash", "-c", "printf 'local-fallback\\n'"]

[[route_sets]]
provides = "parallel"
alternatives = ["left", "right"]
policy = "all"
"#,
    )
    .unwrap();

    // Capacity-first selection deterministically chooses node-a first. The
    // interleaved slot pool still sends the second ready branch to node-b.
    let mut node_a = provision_node(root.path(), "mesh-node-a", 2, false);
    let mut node_b = provision_node(root.path(), "mesh-node-b", 1, true);
    register_node(&peer_root, &node_a);
    register_node(&peer_root, &node_b);

    let registry_before = snapshot_tree(&peer_root);
    let node_a_before = snapshot_tree(&node_a.state.join("mesh-v1"));
    let node_b_before = snapshot_tree(&node_b.state.join("mesh-v1"));
    let live_preview = observe_mesh_peers_read_only(&MeshReadOnlyDiscoveryConfig {
        discover_lan: false,
        discovery_timeout: Duration::from_millis(100),
        peer_root: Some(peer_root.clone()),
    })
    .unwrap();
    assert_eq!(
        live_preview
            .peers
            .iter()
            .map(|peer| (
                peer.node_id.as_str(),
                peer.profile.as_ref().unwrap().node_id.as_str(),
                peer.capacity.as_ref().unwrap().available_slots,
            ))
            .collect::<Vec<_>>(),
        [
            ("mesh-node-a", "mesh-node-a", 2),
            ("mesh-node-b", "mesh-node-b", 1),
        ]
    );
    assert!(live_preview.peers.iter().all(|peer| peer.eligible));
    assert_eq!(snapshot_tree(&peer_root), registry_before);
    assert_eq!(snapshot_tree(&node_a.state.join("mesh-v1")), node_a_before);
    assert_eq!(snapshot_tree(&node_b.state.join("mesh-v1")), node_b_before);

    let front_door_state = root.path().join("front-door-state");
    let parallel_trace = root.path().join("parallel-trace.json");
    let parallel = run_o_cli_auto(&project, &peer_root, &parallel_trace, &front_door_state);
    assert!(
        parallel.status.success(),
        "parallel mesh failed\nstdout:\n{}\nstderr:\n{}\nnode-a:\n{}\nnode-b:\n{}",
        String::from_utf8_lossy(&parallel.stdout),
        String::from_utf8_lossy(&parallel.stderr),
        node_a.guard.diagnostics(),
        node_b.guard.diagnostics(),
    );
    let parallel_stdout = String::from_utf8_lossy(&parallel.stdout);
    assert!(parallel_stdout.contains("parallel:mesh-node-a"));
    assert!(parallel_stdout.contains("parallel:mesh-node-b"));
    let parallel_trace = read_trace(&parallel_trace);
    let dispatched_nodes = parallel_trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event"] == "dispatched")
        .filter_map(|event| event["node_id"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(dispatched_nodes, ["mesh-node-a", "mesh-node-b"].into());

    // The two-party barrier above cannot complete under sequential dispatch.
    // Wait for both durable workers to release their slots before measuring
    // capacity-first retry ordering in the next invocation.
    node_a.wait_for_full_capacity();
    node_b.wait_for_full_capacity();

    // node-a wins target rank and actually settles generation 1 with a route
    // failure. The island's explicit idempotence contract authorizes replay of
    // generation 2 on node-b.
    let retry_trace = root.path().join("retry-trace.json");
    let retry = run_o_cli_required_retry(&project, &peer_root, &retry_trace, &front_door_state);
    assert!(
        retry.status.success(),
        "mesh retry failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&retry.stdout),
        String::from_utf8_lossy(&retry.stderr),
    );
    assert!(String::from_utf8_lossy(&retry.stdout).contains("retry:mesh-node-b"));
    let retry_trace = read_trace(&retry_trace);
    let retry_events = retry_trace["events"].as_array().unwrap();
    assert_eq!(
        retry_events.len(),
        5,
        "unexpected retry trace: {retry_events:?}"
    );
    assert_eq!(retry_events[0]["event"], "dispatched");
    assert_eq!(retry_events[0]["generation"], 1);
    assert_eq!(retry_events[0]["node_id"], "mesh-node-a");
    assert_eq!(retry_events[1]["event"], "settled");
    assert_eq!(retry_events[1]["generation"], 1);
    assert_eq!(retry_events[1]["node_id"], "mesh-node-a");
    assert_eq!(retry_events[1]["succeeded"], false);
    assert_eq!(retry_events[2]["event"], "migrated");
    assert_eq!(retry_events[2]["from_generation"], 1);
    assert_eq!(retry_events[2]["to_generation"], 2);
    assert_eq!(retry_events[2]["from_node_id"], "mesh-node-a");
    assert_eq!(retry_events[2]["to_node_id"], "mesh-node-b");
    assert_eq!(retry_events[3]["event"], "dispatched");
    assert_eq!(retry_events[3]["generation"], 2);
    assert_eq!(retry_events[3]["node_id"], "mesh-node-b");
    assert_eq!(retry_events[4]["event"], "settled");
    assert_eq!(retry_events[4]["generation"], 2);
    assert_eq!(retry_events[4]["node_id"], "mesh-node-b");
    assert_eq!(retry_events[4]["succeeded"], true);
    assert!(!retry_events
        .iter()
        .any(|event| { event["event"] == "local_fallback" || event["event"] == "retry_denied" }));

    // A settled command without an explicit replay contract must never be
    // migrated, even when the retry budget would otherwise permit it. Run
    // through the root front door and request its causal explanation too.
    node_a.wait_for_full_capacity();
    node_b.wait_for_full_capacity();
    let denied_trace = root.path().join("retry-denied-trace.json");
    let denied = run_o_cli_mesh_no_record(
        &project,
        &peer_root,
        &denied_trace,
        &front_door_state,
        &[
            "--route",
            "retry-denied",
            "--mesh=required",
            "--mesh-retries=1",
            "--explain-mesh",
        ],
    );
    assert!(
        !denied.status.success(),
        "non-idempotent retry unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&denied.stdout),
        String::from_utf8_lossy(&denied.stderr),
    );
    let denied_summary: serde_json::Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert_eq!(denied_summary["disposition"], "execution_failed");
    assert_eq!(denied_summary["recording"]["status"], "disabled");
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("o mesh: retry denied route retry-denied"),
        "--explain-mesh omitted retry causality: {}",
        String::from_utf8_lossy(&denied.stderr)
    );
    let denied_trace = read_trace(&denied_trace);
    let denied_events = denied_trace["events"].as_array().unwrap();
    assert!(denied_events
        .iter()
        .any(|event| event["event"] == "retry_denied"));
    assert!(!denied_events
        .iter()
        .any(|event| event["event"] == "migrated"));
    assert!(!denied_events.iter().any(|event| event["generation"] == 2));

    fs::create_dir_all(&empty_peer_root).unwrap();
    let fallback_trace = root.path().join("fallback-trace.json");
    let fallback = run_o_cli_mesh_no_record(
        &project,
        &empty_peer_root,
        &fallback_trace,
        &front_door_state,
        &["--parallel", "auto", "--route", "local", "--mesh-retries=0"],
    );
    assert!(
        fallback.status.success(),
        "local fallback failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&fallback.stdout),
        String::from_utf8_lossy(&fallback.stderr),
    );
    let fallback_summary: serde_json::Value = serde_json::from_slice(&fallback.stdout).unwrap();
    assert_eq!(fallback_summary["disposition"], "succeeded");
    assert_eq!(fallback_summary["recording"]["status"], "disabled");
    let fallback_trace = read_trace(&fallback_trace);
    assert!(fallback_trace["candidates"].as_array().unwrap().is_empty());
    let fallback_events = fallback_trace["events"].as_array().unwrap();
    assert_eq!(fallback_events.len(), 1);
    assert_eq!(fallback_events[0]["event"], "local_fallback");
    assert_eq!(fallback_events[0]["after_generation"], 0);
    assert!(!fallback_events
        .iter()
        .any(|event| event["event"] == "dispatched"));

    let required_trace = root.path().join("required-empty-trace.json");
    let required = run_o_cli_mesh_no_record(
        &project,
        &empty_peer_root,
        &required_trace,
        &front_door_state,
        &["--route", "local", "--mesh=required", "--mesh-retries=0"],
    );
    assert!(
        !required.status.success(),
        "required mesh unexpectedly fell back with an empty registry\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&required.stdout),
        String::from_utf8_lossy(&required.stderr),
    );
    let required_summary: serde_json::Value = serde_json::from_slice(&required.stdout).unwrap();
    assert_eq!(required_summary["disposition"], "infrastructure_failed");
    assert_eq!(required_summary["recording"]["status"], "disabled");
    let required_reason =
        "mesh placement is required, but discovery found no authenticated peer eligible";
    assert!(
        String::from_utf8_lossy(&required.stderr).contains(required_reason),
        "required-mode diagnostic lost its exact safe cause: {}",
        String::from_utf8_lossy(&required.stderr)
    );
    assert!(required_summary["failure"]["message"]
        .as_str()
        .unwrap()
        .contains(required_reason));
    let required_trace = read_trace(&required_trace);
    assert!(required_trace["candidates"].as_array().unwrap().is_empty());
    assert!(required_trace["events"].as_array().unwrap().is_empty());

    node_a.guard.shutdown();
    node_b.guard.shutdown();
    assert!(node_a.state.join("mesh-v1/actors").is_dir());
    assert!(node_b.state.join("mesh-v1/actors").is_dir());

    // Retained evidence must be self-contained: remove both the input bundle
    // and every peer hint after the nodes are offline, then prove explanation
    // and strict trace inspection are read-only and still tell the retry story.
    fs::remove_dir_all(&project).unwrap();
    fs::remove_dir_all(&peer_root).unwrap();
    let history_before = snapshot_tree(&front_door_state);
    let explanation = run_o_cli_evidence(&front_door_state, &["explain", "last-run"]);
    assert!(
        explanation.status.success(),
        "offline explanation failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&explanation.stdout),
        String::from_utf8_lossy(&explanation.stderr)
    );
    let explanation = String::from_utf8_lossy(&explanation.stdout);
    for expected in [
        "settled on node `mesh-node-a` with succeeded=false",
        "migrated route `retry`",
        "node `mesh-node-b`",
        "succeeded=true",
        "unsigned_observation",
    ] {
        assert!(
            explanation.contains(expected),
            "offline explanation omitted {expected:?}:\n{explanation}"
        );
    }
    let inspection = run_o_cli_evidence(&front_door_state, &["inspect", "last-run", "--trace"]);
    assert!(inspection.status.success());
    let inspection: serde_json::Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(inspection["state"], "terminal");
    assert_eq!(inspection["trace"]["schema"], "ostadix.run-trace/v1");
    assert_eq!(inspection["trace"]["payload"]["engine"], "project_mesh");
    assert_eq!(
        snapshot_tree(&front_door_state),
        history_before,
        "read-only explain/inspect changed run history"
    );
}
