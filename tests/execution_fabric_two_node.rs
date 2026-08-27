//! Same-host, distinct-process proof for authenticated Fabric V1 execution.
//!
//! This is deliberately not evidence of distinct kernels, hardware, or
//! architectures. It proves the process/TLS/authority/ledger boundary using
//! two independently configured `o-node` processes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use o_lang::eval::Evaluator;
use o_lang::execution_fabric::ExecutionIdV1;
use o_lang::execution_fabric_authority::{
    ExecutionCellIncarnationV1, FabricSigningKeyV1, FabricTargetBindingV1,
};
use o_lang::hosted_remote::{
    certificate_leaf_sha256, trusted_inline_fabric_realization_pipeline_sha256_v1,
    write_new_fabric_node_signing_key_v1, write_new_fabric_public_key_v1, ClientTlsIdentity,
    HostedNodeClient, MeshNodeClient, RemotePureExecutionConfigV1,
};
use o_lang::ir::OIrProgram;
use o_lang::parser::Parser;
use o_lang::placement::{GenerationV1, SemanticDigestV1};
use o_lang::value::OValue;

mod support;

const SOURCE: &str = "result = text^(fabric-v1: α <tag>)_text\n";

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
            // SAFETY: this guard owns the exact unreaped child process.
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

struct FabricNode {
    node_id: String,
    address: SocketAddr,
    generation: GenerationV1,
    incarnation: ExecutionCellIncarnationV1,
    pki: PathBuf,
    coordinator_pki: PathBuf,
    fabric_state: PathBuf,
    receipt_public_key: [u8; 32],
    guard: NodeGuard,
}

impl FabricNode {
    fn client_identity(&self) -> ClientTlsIdentity {
        ClientTlsIdentity {
            ca_path: self.pki.join("ca.pem"),
            cert_path: self.coordinator_pki.join("client-cert.pem"),
            key_path: self.coordinator_pki.join("client-key.pem"),
            server_name: "localhost".to_string(),
        }
    }

    fn server_principal(&self) -> SemanticDigestV1 {
        semantic_certificate_digest(&self.pki.join("node-cert.pem"))
    }

    fn coordinator_principal(&self) -> SemanticDigestV1 {
        semantic_certificate_digest(&self.coordinator_pki.join("client-cert.pem"))
    }

    fn ledger_path(&self) -> PathBuf {
        self.fabric_state.join("fabric-v1/attempt-ledger.cbor")
    }

    fn assert_hosted_and_mesh_routes(&self) {
        let mut hosted = HostedNodeClient::new(self.address.to_string(), self.client_identity());
        hosted.connect_timeout = Duration::from_secs(2);
        hosted.io_timeout = Duration::from_secs(2);
        let hosted_profile = hosted.profile().unwrap_or_else(|error| {
            panic!(
                "Hosted V1 profile failed for {}: {error:#}\n{}",
                self.node_id,
                self.guard.diagnostics()
            )
        });
        assert_eq!(hosted_profile.node_id, self.node_id);

        let mesh_profile = MeshNodeClient::new(
            self.address.to_string(),
            self.client_identity(),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .profile()
        .unwrap_or_else(|error| {
            panic!(
                "Mesh V1 profile failed for {}: {error:#}\n{}",
                self.node_id,
                self.guard.diagnostics()
            )
        });
        assert_eq!(mesh_profile.node_id, self.node_id);
    }
}

fn semantic_certificate_digest(path: &Path) -> SemanticDigestV1 {
    SemanticDigestV1::from_sha256(certificate_leaf_sha256(path).unwrap()).unwrap()
}

fn semantic_digest(seed: u8) -> SemanticDigestV1 {
    SemanticDigestV1::from_sha256(hex::encode([seed; 32])).unwrap()
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

fn execution_cell_incarnation(diagnostics: &str) -> Option<u64> {
    diagnostics
        .split("execution-cell incarnation ")
        .nth(1)?
        .split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn provision_node(
    root: &Path,
    node_id: &str,
    generation: u64,
    coordinator_pki: Option<&Path>,
    authority_public_key: &Path,
) -> FabricNode {
    let node_root = root.join(node_id);
    let pki = node_root.join("pki");
    let state = node_root.join("v2-state");
    let placement_authority = node_root.join("placement-authority");
    let fabric_state = node_root.join("fabric-state");
    let fabric_key = node_root.join("fabric-keys/node-signing-key.v1");
    let stderr = node_root.join("node.stderr");
    fs::create_dir_all(&node_root).unwrap();
    #[cfg(unix)]
    fs::set_permissions(&node_root, fs::Permissions::from_mode(0o700)).unwrap();

    let mut pki_init = Command::new(env!("CARGO_BIN_EXE_o-node"));
    pki_init
        .args(["pki", "init", "--directory"])
        .arg(&pki)
        .args(["--server-name", "localhost"]);
    command_output(pki_init, "Fabric node PKI provisioning");

    let mut identity_init = Command::new(env!("CARGO_BIN_EXE_o-node"));
    identity_init
        .args(["identity", "init", "--state-dir"])
        .arg(&state);
    command_output(identity_init, "Fabric node V2 identity provisioning");

    let mut placement_init = Command::new(env!("CARGO_BIN_EXE_octl"));
    placement_init
        .args(["node", "authority", "init", "--directory"])
        .arg(&placement_authority);
    command_output(
        placement_init,
        "Fabric node placement-authority provisioning",
    );

    let node_signer = FabricSigningKeyV1::generate().unwrap();
    write_new_fabric_node_signing_key_v1(&fabric_key, &node_signer).unwrap();
    let receipt_public_key = node_signer.public_key();
    let coordinator_pki = coordinator_pki.unwrap_or(&pki).to_path_buf();
    let generation = GenerationV1::new(generation).unwrap();
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
        .arg(coordinator_pki.join("ca.pem"))
        .arg("--fabric-state-dir")
        .arg(&fabric_state)
        .arg("--fabric-node-signing-key")
        .arg(&fabric_key)
        .arg("--fabric-authority-public-key")
        .arg(authority_public_key)
        .arg("--fabric-node-generation")
        .arg(generation.get().to_string())
        .arg("--v2-state-dir")
        .arg(&state)
        .arg("--mesh-state-dir")
        .arg(state.join("mesh-v1"))
        .arg("--v2-authority-public-key")
        .arg(placement_authority.join("placement-public-key.v2"))
        .arg("--v2-node-generation")
        .arg(generation.get().to_string())
        .arg("--max-connections")
        .arg("8")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file));
    let child = server.spawn().unwrap();
    let mut guard = NodeGuard {
        child: Some(child),
        stderr,
    };

    let readiness_deadline = Instant::now() + Duration::from_secs(15);
    let incarnation = loop {
        let client_identity = ClientTlsIdentity {
            ca_path: pki.join("ca.pem"),
            cert_path: coordinator_pki.join("client-cert.pem"),
            key_path: coordinator_pki.join("client-key.pem"),
            server_name: "localhost".to_string(),
        };
        let hosted_ready = {
            let mut client = HostedNodeClient::new(address.to_string(), client_identity.clone());
            client.connect_timeout = Duration::from_millis(250);
            client.io_timeout = Duration::from_secs(2);
            client
                .profile()
                .is_ok_and(|profile| profile.node_id == node_id)
        };
        let mesh_ready = MeshNodeClient::new(
            address.to_string(),
            client_identity,
            Duration::from_millis(250),
            Duration::from_secs(2),
        )
        .profile()
        .is_ok_and(|profile| profile.node_id == node_id);
        let observed_incarnation = execution_cell_incarnation(&guard.diagnostics());
        if hosted_ready && mesh_ready {
            if let Some(incarnation) = observed_incarnation {
                break ExecutionCellIncarnationV1::new(incarnation).unwrap();
            }
        }
        if guard.child.as_mut().unwrap().try_wait().unwrap().is_some() {
            panic!(
                "Fabric node {node_id} exited during startup\n{}",
                guard.diagnostics()
            );
        }
        assert!(
            Instant::now() < readiness_deadline,
            "Fabric node {node_id} did not become ready\n{}",
            guard.diagnostics()
        );
        thread::sleep(Duration::from_millis(50));
    };

    FabricNode {
        node_id: node_id.to_string(),
        address,
        generation,
        incarnation,
        pki,
        coordinator_pki,
        fabric_state,
        receipt_public_key,
        guard,
    }
}

fn target_for(
    node: &FabricNode,
    realization_pipeline: &SemanticDigestV1,
    seed: u8,
) -> FabricTargetBindingV1 {
    FabricTargetBindingV1::new(
        node.coordinator_principal(),
        node.node_id.clone(),
        node.generation,
        node.incarnation,
        semantic_digest(seed),
        GenerationV1::new(1).unwrap(),
        GenerationV1::new(1).unwrap(),
        semantic_digest(seed.wrapping_add(1)),
        semantic_digest(seed.wrapping_add(2)),
        semantic_digest(seed.wrapping_add(3)),
        semantic_digest(seed.wrapping_add(4)),
        semantic_digest(seed.wrapping_add(5)),
        semantic_digest(seed.wrapping_add(6)),
        realization_pipeline.clone(),
    )
    .unwrap()
}

fn remote_config(
    connection: &FabricNode,
    target: &FabricNode,
    receipt_public_key: [u8; 32],
    authority: &FabricSigningKeyV1,
    realization_pipeline: &SemanticDigestV1,
    execution_seed: u8,
    target_seed: u8,
) -> RemotePureExecutionConfigV1 {
    RemotePureExecutionConfigV1::new(
        connection.address.to_string(),
        connection.client_identity(),
        connection.server_principal(),
        authority.clone(),
        target_for(target, realization_pipeline, target_seed),
        receipt_public_key,
        ExecutionIdV1::new([execution_seed; 32]).unwrap(),
    )
    .unwrap()
    .with_timeouts(
        Duration::from_secs(2),
        Duration::from_secs(2),
        Duration::from_millis(10),
        Duration::from_secs(5),
    )
    .unwrap()
}

fn evaluate(
    config: Option<RemotePureExecutionConfigV1>,
    scope: &mut HashMap<String, OValue>,
) -> anyhow::Result<OValue> {
    let registered = HashSet::from(["text".to_string()]);
    let nodes = Parser::new(SOURCE, &registered).parse()?;
    let program = OIrProgram::lower(&nodes);
    let evaluator = Evaluator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .with_registered_backends(registered);
    let mut evaluator = match config {
        Some(config) => evaluator.with_remote_pure_execution(config),
        None => evaluator,
    };
    evaluator.eval_ir_program_graph_with_scope(&program, scope)
}

fn assert_error_chain_contains(error: &anyhow::Error, needle: &str) {
    let chain = format!("{error:#}");
    assert!(chain.contains(needle), "missing `{needle}` in: {chain}");
}

#[test]
fn two_real_o_nodes_execute_provisional_pure_candidates_without_graph_authority() {
    if !support::require_runtimes(&["openssl"]) {
        return;
    }

    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let authority = FabricSigningKeyV1::generate().unwrap();
    let authority_public_key = root.path().join("fabric-authority/authority-public-key.v1");
    write_new_fabric_public_key_v1(&authority_public_key, &authority.public_key()).unwrap();

    let mut node_a = provision_node(
        root.path(),
        "fabric-node-a",
        11,
        None,
        &authority_public_key,
    );
    let mut node_b = provision_node(
        root.path(),
        "fabric-node-b",
        17,
        Some(&node_a.pki),
        &authority_public_key,
    );
    assert_ne!(node_a.address, node_b.address);
    assert_ne!(node_a.pki, node_b.pki);
    assert_ne!(node_a.fabric_state, node_b.fabric_state);
    assert_ne!(node_a.generation, node_b.generation);
    assert_ne!(node_a.receipt_public_key, node_b.receipt_public_key);
    node_a.assert_hosted_and_mesh_routes();
    node_b.assert_hosted_and_mesh_routes();

    let realization_pipeline =
        trusted_inline_fabric_realization_pipeline_sha256_v1("text").unwrap();
    let mut local_scope = HashMap::new();
    let local = evaluate(None, &mut local_scope).unwrap();

    let mut node_a_scope = HashMap::new();
    let from_a = evaluate(
        Some(remote_config(
            &node_a,
            &node_a,
            node_a.receipt_public_key,
            &authority,
            &realization_pipeline,
            1,
            31,
        )),
        &mut node_a_scope,
    )
    .unwrap_or_else(|error| {
        panic!(
            "node A Fabric execution failed: {error:#}\n{}",
            node_a.guard.diagnostics()
        )
    });
    assert_eq!(from_a, local);
    assert_eq!(node_a_scope, local_scope);

    let mut node_b_scope = HashMap::new();
    let from_b = evaluate(
        Some(remote_config(
            &node_b,
            &node_b,
            node_b.receipt_public_key,
            &authority,
            &realization_pipeline,
            2,
            51,
        )),
        &mut node_b_scope,
    )
    .unwrap_or_else(|error| {
        panic!(
            "node B Fabric execution failed: {error:#}\n{}",
            node_b.guard.diagnostics()
        )
    });
    assert_eq!(from_b, local);
    assert_eq!(node_b_scope, local_scope);

    let mut wrong_lease_scope = HashMap::new();
    let wrong_lease = evaluate(
        Some(remote_config(
            &node_b,
            &node_a,
            node_a.receipt_public_key,
            &authority,
            &realization_pipeline,
            3,
            71,
        )),
        &mut wrong_lease_scope,
    )
    .expect_err("node B must reject a lease targeting node A");
    assert_error_chain_contains(&wrong_lease, "gate 08");
    assert_error_chain_contains(&wrong_lease, "target-binding-rejected");
    assert!(wrong_lease_scope.is_empty());

    let mut wrong_result_scope = HashMap::new();
    let wrong_result = evaluate(
        Some(remote_config(
            &node_b,
            &node_b,
            node_a.receipt_public_key,
            &authority,
            &realization_pipeline,
            4,
            91,
        )),
        &mut wrong_result_scope,
    )
    .expect_err("a result signed by the wrong pinned node key must not commit");
    assert_error_chain_contains(&wrong_result, "gate 03");
    assert!(wrong_result_scope.is_empty());

    let node_a_ledger = fs::read(node_a.ledger_path()).unwrap();
    let node_b_ledger_before_stopped_a = fs::read(node_b.ledger_path()).unwrap();
    assert!(!node_a_ledger.is_empty());
    assert!(!node_b_ledger_before_stopped_a.is_empty());
    node_a.guard.shutdown();

    let mut stopped_scope = HashMap::new();
    let stopped = evaluate(
        Some(remote_config(
            &node_a,
            &node_a,
            node_a.receipt_public_key,
            &authority,
            &realization_pipeline,
            5,
            111,
        )),
        &mut stopped_scope,
    )
    .expect_err("an explicitly selected stopped node must not fall back locally");
    assert_error_chain_contains(
        &stopped,
        "Fabric connection failed before candidate acceptance",
    );
    assert!(stopped_scope.is_empty());
    assert_eq!(
        fs::read(node_b.ledger_path()).unwrap(),
        node_b_ledger_before_stopped_a,
        "failure of explicit node A selection dispatched work to node B"
    );

    // The direct execution above proves this exact source succeeds locally;
    // the stopped-node failure and empty scope therefore prove no local
    // trusted-renderer fallback occurred.
    assert_eq!(local, OValue::text("fabric-v1: α <tag>"));
    assert!(local_scope.is_empty());
    node_b.assert_hosted_and_mesh_routes();
    node_b.guard.shutdown();
}
