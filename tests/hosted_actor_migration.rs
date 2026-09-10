//! Two actual TLS node processes execute checkpoint transfer and durable fencing.
use o_lang::eval::{Evaluator, PlacementFragmentBindingsV2};
use o_lang::hosted_remote::v2::*;
use o_lang::hosted_remote::{certificate_leaf_sha256, unix_time_ms, ClientTlsIdentity};
use o_lang::ir::BackendRegistry;
use o_lang::placement::*;
use o_lang::value::OValue;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn digest(label: &str) -> SemanticDigestV1 {
    SemanticDigestV1::hash_bytes("ostadix/hosted-migration-test/v2", label.as_bytes())
}
fn quotas() -> StateQuotaLimitsV2 {
    StateQuotaLimitsV2::new(8, 1, 4194304, 8388608, 67108864).unwrap()
}
fn reservation() -> StateReservationV2 {
    StateReservationV2::new(1, 4194304, 8388608).unwrap()
}
fn open_capability(state_session: &StateSessionIdV2, request_id: &str) -> SessionCapabilityV2 {
    SessionCapabilityV2 {
        session_id: state_session.semantic_digest().unwrap().to_string(),
        bearer: digest(request_id).to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn lease_for_node(
    node_id: &str,
    shim_dir: &Path,
    recovery_warrant_sha256: Option<String>,
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    state_session: StateSessionIdV2,
    state_tier: SessionStateTierV2,
    state_quotas: StateQuotaLimitsV2,
    state_reservation: StateReservationV2,
    established_target: Option<&TargetDescriptorV1>,
    actor_generation: Option<&ActorGenerationIdV1>,
    request_id: &str,
    sequence: u64,
    purpose: PlacementPurposeV2,
    operation_sha256: Option<String>,
    operation: &PreparedOperationV2,
    placement_admission_override: Option<SemanticDigestV1>,
    validity_ms: u64,
) -> (SignedPlacementLeaseV2, TargetDescriptorV1) {
    let bindings = prepare_bindings(operation, shim_dir);
    let now = unix_time_ms().unwrap();
    let establishing_logical_environment = purpose == PlacementPurposeV2::OpenSession
        || (purpose == PlacementPurposeV2::Execute
            && state_tier != SessionStateTierV2::Stateless
            && actor_generation.is_none());
    let provisional = build_local_dev_placement_proof_v2(
        &bindings,
        signer.issuer_key(),
        LocalDevPlacementConfigV2 {
            node_id: node_id.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            profile_generation: GenerationV1::new(1).unwrap(),
            capacity_generation: GenerationV1::new(1).unwrap(),
            reservation: PlacementReservationV1::new(1, 1024 * 1024, 0).unwrap(),
            now_unix_ms: now,
        },
        established_target,
        actor_generation,
        establishing_logical_environment,
    )
    .unwrap();
    let target = provisional.evidence.node_profile.descriptor().clone();
    let command = HostedCommandBindingV2 {
        schema: HOSTED_COMMAND_BINDING_SCHEMA_V2.to_owned(),
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        node_id: node_id.to_owned(),
        principal_sha256: principal.to_owned(),
        state_session: state_session.clone(),
        session_state_tier: state_tier,
        client_request_id: request_id.to_owned(),
        client_sequence: sequence,
        purpose,
        operation_sha256,
        recovery_warrant_sha256,
        open_capability_commitment: (purpose == PlacementPurposeV2::OpenSession).then(|| {
            open_capability_commitment_v2(&open_capability(&state_session, request_id)).unwrap()
        }),
        state_quota_generation: GenerationV1::new(1).unwrap(),
        state_quota_limits: state_quotas.clone(),
        state_reservation: state_reservation.clone(),
        actor_generation: actor_generation.cloned(),
    };
    let observation = if purpose == PlacementPurposeV2::OpenSession {
        Some(
            StateCapacityObservationV2::new(
                signer.issuer_key(),
                node_id,
                GenerationV1::new(1).unwrap(),
                GenerationV1::new(1).unwrap(),
                state_quotas,
                0,
                0,
                UnixMillisV1::new(now.saturating_sub(1)),
                UnixMillisV1::new(now + validity_ms),
            )
            .unwrap(),
        )
    } else {
        None
    };
    let state_binding = match &observation {
        Some(observation) => {
            LeaseStateBindingV2::open(observation.semantic_digest().unwrap(), state_reservation)
        }
        None => LeaseStateBindingV2::existing(
            state_session,
            actor_generation
                .map(CanonicalPlacementRecordV1::semantic_digest)
                .transpose()
                .unwrap(),
        ),
    };
    let evidence = provisional.evidence;
    let target_digest = evidence.node_profile.descriptor_digest().unwrap();
    let capacity_digest = evidence.capacity_observation.semantic_digest().unwrap();
    let footprint_digest = evidence.requirement_footprint.semantic_digest().unwrap();
    let discharge_digest = evidence.warrant_discharge.semantic_digest().unwrap();
    let trust_digest = evidence.trust_policy.semantic_digest().unwrap();
    let eligibility_digest = provisional.eligibility.semantic_digest().unwrap();
    let command_digest = command.semantic_digest().unwrap();
    let authority = if purpose == PlacementPurposeV2::Execute {
        let expectation = LeaseExpectationV2::new(
            node_id,
            target_digest,
            evidence.node_profile.profile_generation(),
            evidence.capacity_observation.capacity_generation(),
            capacity_digest,
            eligibility_digest,
            bindings.operation_oir().clone(),
            footprint_digest,
            discharge_digest,
            placement_admission_override.unwrap_or_else(|| bindings.placement_admission().clone()),
            bindings.task_attempt().clone(),
            bindings.backend_implementation_sha256().clone(),
            bindings.realization_pipeline().clone(),
            trust_digest,
            evidence.reservation.clone(),
            command_digest,
            state_binding,
        )
        .unwrap();
        HostedPlacementAuthorityV2::Execution(
            PlacementLeaseV2::new(
                signer.issuer_key(),
                digest(&format!("nonce:{request_id}")),
                expectation,
                UnixMillisV1::new(now.saturating_sub(1)),
                UnixMillisV1::new(now + validity_ms),
            )
            .unwrap(),
        )
    } else {
        let expectation = StateControlExpectationV2::new(
            node_id,
            target_digest,
            evidence.node_profile.profile_generation(),
            evidence.capacity_observation.capacity_generation(),
            capacity_digest,
            eligibility_digest,
            footprint_digest,
            discharge_digest,
            bindings.backend_implementation_sha256().clone(),
            bindings.realization_pipeline().clone(),
            trust_digest,
            evidence.reservation.clone(),
            command_digest,
            state_binding,
        )
        .unwrap();
        HostedPlacementAuthorityV2::StateControl(
            StateControlLeaseV2::new(
                signer.issuer_key(),
                digest(&format!("nonce:{request_id}")),
                expectation,
                UnixMillisV1::new(now.saturating_sub(1)),
                UnixMillisV1::new(now + validity_ms),
            )
            .unwrap(),
        )
    };
    (
        signer
            .sign(authority, command, evidence, observation)
            .unwrap(),
        target,
    )
}

fn prepare_bindings(
    operation: &PreparedOperationV2,
    shim_dir: &Path,
) -> PlacementFragmentBindingsV2 {
    let mut evaluator = Evaluator::new(shim_dir.to_path_buf())
        .with_registered_backends(BackendRegistry::global().registered_backend_tags())
        .with_runtime_executable(Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf());
    evaluator
        .prepare_placement_fragment(&operation.source_utf8, operation.task_attempt.clone())
        .unwrap()
        .bindings()
        .clone()
}

struct Node {
    child: Option<Child>,
    root: PathBuf,
    pki: PathBuf,
    node_id: String,
    key: HostedNodeSignerV2,
    address: String,
    authority_path: PathBuf,
    shim_dir: PathBuf,
}
impl Node {
    fn new(
        base: &Path,
        pki: &Path,
        node_id: &str,
        byte: u8,
        authority: &PlacementLeaseSignerV2,
    ) -> Self {
        Self::with_shims(
            base,
            pki,
            node_id,
            byte,
            authority,
            Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
        )
    }
    fn with_shims(
        base: &Path,
        pki: &Path,
        node_id: &str,
        byte: u8,
        authority: &PlacementLeaseSignerV2,
        shim_dir: PathBuf,
    ) -> Self {
        let root = base.join(node_id);
        fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let key = HostedNodeSignerV2::from_secret_bytes([byte; 32]);
        write_new_node_signing_key_v2(root.join("node.key"), &key).unwrap();
        let authority_path = root.join("authority.pub");
        write_new_placement_public_key_v2(&authority_path, &authority.public_key()).unwrap();
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap().to_string();
        drop(reservation);
        let mut node = Self {
            child: None,
            root,
            pki: pki.to_path_buf(),
            node_id: node_id.to_owned(),
            key,
            address,
            authority_path,
            shim_dir,
        };
        node.start();
        node
    }
    fn start(&mut self) {
        let stderr = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("node.stderr"))
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_o-node"))
            .args([
                "serve",
                "--manual",
                "--no-discovery",
                "--no-bootstrap",
                "--no-mesh",
                "--node-id",
                &self.node_id,
                "--bind",
                &self.address,
            ])
            .arg("--shim-dir")
            .arg(&self.shim_dir)
            .arg("--runtime-binary")
            .arg(env!("CARGO_BIN_EXE_O"))
            .arg("--cert")
            .arg(self.pki.join("node-cert.pem"))
            .arg("--key")
            .arg(self.pki.join("node-key.pem"))
            .arg("--client-ca")
            .arg(self.pki.join("ca.pem"))
            .arg("--v2-state-dir")
            .arg(self.root.join("state"))
            .arg("--v2-node-signing-key")
            .arg(self.root.join("node.key"))
            .arg("--v2-authority-public-key")
            .arg(&self.authority_path)
            .args([
                "--v2-max-open-sessions",
                "8",
                "--v2-max-actors-per-session",
                "1",
                "--v2-max-snapshot-bytes-per-actor",
                "4194304",
                "--v2-max-state-bytes-per-session",
                "8388608",
                "--v2-max-state-bytes-total",
                "67108864",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap();
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if TcpStream::connect(&self.address).is_ok() {
                break;
            }
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "node exited: {}",
                self.stderr()
            );
            assert!(
                Instant::now() < deadline,
                "node startup timed out: {}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
    fn crash_restart(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        self.start();
    }
    fn stderr(&self) -> String {
        fs::read_to_string(self.root.join("node.stderr")).unwrap_or_default()
    }
    fn client(&self) -> HostedNodeClientV2 {
        HostedNodeClientV2::new(
            &self.address,
            ClientTlsIdentity {
                ca_path: self.pki.join("ca.pem"),
                cert_path: self.pki.join("client-cert.pem"),
                key_path: self.pki.join("client-key.pem"),
                server_name: "localhost".to_owned(),
            },
            self.key.public_key(),
        )
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
struct Session {
    capability: SessionCapabilityV2,
    state_session: StateSessionIdV2,
    target: TargetDescriptorV1,
    proof: PreparedOperationV2,
}
fn operation(id: &str, backend: &str, body: &str) -> PreparedOperationV2 {
    PreparedOperationV2::new(
        id,
        TaskAttemptIdV1::new(digest(id), GenerationV1::new(1).unwrap()),
        format!("{backend}[7]^(\n{body}\n)_{backend}[7]"),
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 30000,
        4096,
    )
    .unwrap()
}
fn status(node: &Node, session: &Session) -> SessionViewV2 {
    match node
        .client()
        .status(SessionQueryV2 {
            credentials: session.capability.clone().into(),
            operation_id: None,
        })
        .unwrap()
    {
        HostedResponseV2::Status { session, .. } => session,
        other => panic!("bad status: {other:?}"),
    }
}
fn open(
    node: &Node,
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    label: &str,
    backend: &str,
) -> Session {
    let state_session =
        StateSessionIdV2::new(&node.node_id, GenerationV1::new(1).unwrap(), digest(label)).unwrap();
    let proof = operation(
        &format!("{label}-proof"),
        backend,
        if backend == "python" {
            "pass"
        } else {
            "SELECT NULL"
        },
    );
    let (placement_lease, target) = lease_for_node(
        &node.node_id,
        &node.shim_dir,
        None,
        signer,
        principal,
        state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        quotas(),
        reservation(),
        None,
        None,
        label,
        0,
        PlacementPurposeV2::OpenSession,
        None,
        &proof,
        None,
        4999,
    );
    let capability = open_capability(&state_session, label);
    let opened = node
        .client()
        .open_session(OpenSessionRequestV2 {
            client_request_id: label.to_owned(),
            state_tier: SessionStateTierV2::CheckpointRestore,
            proposed_capability: capability.clone(),
            capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
            placement_lease,
        })
        .unwrap();
    assert!(
        matches!(opened, HostedResponseV2::SessionOpened { .. }),
        "{opened:?}"
    );
    Session {
        capability,
        state_session,
        target,
        proof,
    }
}
fn submit(
    node: &Node,
    session: &Session,
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    op: PreparedOperationV2,
) -> anyhow::Result<HostedResponseV2> {
    let view = status(node, session);
    let (placement_lease, _) = lease_for_node(
        &node.node_id,
        &node.shim_dir,
        None,
        signer,
        principal,
        session.state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        quotas(),
        reservation(),
        Some(&session.target),
        view.actor.actor_generation.as_ref(),
        &op.operation_id,
        view.next_client_sequence,
        PlacementPurposeV2::Execute,
        Some(op.sha256().unwrap()),
        &op,
        None,
        4999,
    );
    node.client().submit_operation(SubmitOperationRequestV2 {
        credentials: session.capability.clone().into(),
        client_request_id: op.operation_id.clone(),
        client_sequence: view.next_client_sequence,
        operation: op,
        placement_lease,
    })
}
fn execute(
    node: &Node,
    session: &Session,
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    op: PreparedOperationV2,
) -> OperationOutcomeV2 {
    let id = op.operation_id.clone();
    submit(node, session, signer, principal, op).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let view = status(node, session);
        if let Some(outcome) = view.operations[&id].outcome.clone() {
            return outcome;
        }
        assert!(
            Instant::now() < deadline,
            "operation did not terminate: {view:?}\n{}",
            node.stderr()
        );
        thread::sleep(Duration::from_millis(10));
    }
}
fn endpoint(node: &Node, session: &Session, principal: &str) -> MigrationEndpointV2 {
    let view = status(node, session);
    MigrationEndpointV2 {
        state_session: session.state_session.clone(),
        node_public_key: node.key.public_key_hex(),
        principal_sha256: principal.to_owned(),
        actor_generation: view.actor.actor_generation.unwrap(),
        journal_head_sha256: view.journal_head_sha256,
    }
}
#[allow(clippy::too_many_arguments)]
fn migration_request(
    node: &Node,
    session: &Session,
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    plan: &MigrationPlanV2,
    action: MigrationActionV2,
    peer: Option<SignedJournalEntryV2>,
    snapshot: Option<o_lang::backend_state::EvaluatorStateSnapshotV1>,
    label: &str,
) -> MigrateSessionRequestV2 {
    let view = status(node, session);
    let warrant = MigrationWarrantV2 {
        plan: plan.clone(),
        action,
        expected_journal_head_sha256: view.journal_head_sha256,
        peer_receipt_sha256: peer.as_ref().map(|receipt| receipt.entry_sha256.clone()),
    };
    let (placement_lease, _) = lease_for_node(
        &node.node_id,
        &node.shim_dir,
        Some(warrant.sha256().unwrap()),
        signer,
        principal,
        session.state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        quotas(),
        reservation(),
        Some(&session.target),
        view.actor.actor_generation.as_ref(),
        label,
        view.next_client_sequence,
        PlacementPurposeV2::Migrate,
        None,
        &session.proof,
        None,
        4999,
    );
    MigrateSessionRequestV2 {
        credentials: session.capability.clone().into(),
        client_request_id: label.to_owned(),
        client_sequence: view.next_client_sequence,
        warrant,
        peer_receipt: peer.map(Box::new),
        snapshot,
        placement_lease,
    }
}
fn migrated(
    response: HostedResponseV2,
    phase: MigrationPhaseV2,
) -> (
    SignedJournalEntryV2,
    Option<o_lang::backend_state::EvaluatorStateSnapshotV1>,
) {
    let HostedResponseV2::Migration { receipt, snapshot } = response else {
        panic!("unexpected migration response {response:?}")
    };
    assert_eq!(
        migration_state_from_receipt(&receipt).unwrap().phase,
        phase,
        "{receipt:?}"
    );
    (receipt, snapshot)
}
fn pki(root: &Path) -> PathBuf {
    let path = root.join("pki");
    let output = Command::new(env!("CARGO_BIN_EXE_o-node"))
        .args(["pki", "init", "--directory"])
        .arg(&path)
        .args(["--server-name", "localhost"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "PKI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

#[test]
fn python_and_sqlite_move_between_real_nodes_with_restart_and_permanent_source_fencing() {
    let root = tempfile::tempdir().unwrap();
    let pki = pki(root.path());
    let principal = certificate_leaf_sha256(pki.join("client-cert.pem")).unwrap();
    let authority = PlacementLeaseSignerV2::from_secret_bytes([31; 32]);
    let mut source = Node::new(root.path(), &pki, "migration-source", 32, &authority);
    let mut destination = Node::new(root.path(), &pki, "migration-destination", 33, &authority);
    assert_ne!(
        source.child.as_ref().unwrap().id(),
        destination.child.as_ref().unwrap().id()
    );
    for backend in ["python", "sql"] {
        let from = open(
            &source,
            &authority,
            &principal,
            &format!("from-{backend}"),
            backend,
        );
        let to = open(
            &destination,
            &authority,
            &principal,
            &format!("to-{backend}"),
            backend,
        );
        let initializer = if backend == "python" {
            "shared = [42]\npayload = {'left': shared, 'right': shared}\n__oval_result__ = 42"
        } else {
            "CREATE TABLE migrated(value); INSERT INTO migrated VALUES (42); SELECT value FROM migrated;"
        };
        execute(
            &source,
            &from,
            &authority,
            &principal,
            operation(&format!("init-source-{backend}"), backend, initializer),
        );
        execute(
            &destination,
            &to,
            &authority,
            &principal,
            operation(
                &format!("init-dest-{backend}"),
                backend,
                if backend == "python" {
                    "pass"
                } else {
                    "SELECT NULL"
                },
            ),
        );
        let plan = MigrationPlanV2 {
            transaction_id: format!("transfer-{backend}"),
            source: endpoint(&source, &from, &principal),
            destination: endpoint(&destination, &to, &principal),
            checkpoint_sha256: status(&source, &from).actor.checkpoint_sha256.unwrap(),
        };
        let prepare = migration_request(
            &source,
            &from,
            &authority,
            &principal,
            &plan,
            MigrationActionV2::Prepare,
            None,
            None,
            &format!("prepare-{backend}"),
        );
        let (prepared, snapshot) = migrated(
            source.client().migrate_session(prepare.clone()).unwrap(),
            MigrationPhaseV2::Prepared,
        );
        assert_eq!(
            source.client().migrate_session(prepare.clone()).unwrap(),
            HostedResponseV2::Migration {
                receipt: prepared.clone(),
                snapshot: snapshot.clone()
            }
        );
        assert!(submit(
            &source,
            &from,
            &authority,
            &principal,
            operation(
                &format!("frozen-{backend}"),
                backend,
                "invalid source must not execute"
            )
        )
        .is_err());
        if backend == "python" {
            source.crash_restart();
            assert_eq!(status(&source, &from).status, SessionStatusV2::Migrating);
            assert_eq!(
                source.client().migrate_session(prepare).unwrap(),
                HostedResponseV2::Migration {
                    receipt: prepared.clone(),
                    snapshot: snapshot.clone()
                }
            );
        }
        let install = migration_request(
            &destination,
            &to,
            &authority,
            &principal,
            &plan,
            MigrationActionV2::Install,
            Some(prepared.clone()),
            snapshot,
            &format!("install-{backend}"),
        );
        let mut altered = install.clone();
        altered.warrant.expected_journal_head_sha256 = "00".repeat(32);
        assert!(destination.client().migrate_session(altered).is_err());
        let (installed, _) = migrated(
            destination
                .client()
                .migrate_session(install.clone())
                .unwrap(),
            MigrationPhaseV2::Installed,
        );
        assert_eq!(status(&destination, &to).status, SessionStatusV2::Migrating);
        assert!(submit(
            &destination,
            &to,
            &authority,
            &principal,
            operation(
                &format!("standby-{backend}"),
                backend,
                "invalid standby source must not execute"
            )
        )
        .is_err());
        if backend == "python" {
            destination.crash_restart();
        }
        assert_eq!(
            destination.client().migrate_session(install).unwrap(),
            HostedResponseV2::Migration {
                receipt: installed.clone(),
                snapshot: None
            }
        );
        let fence = migration_request(
            &source,
            &from,
            &authority,
            &principal,
            &plan,
            MigrationActionV2::Fence,
            Some(installed),
            None,
            &format!("fence-{backend}"),
        );
        let (fenced, _) = migrated(
            source.client().migrate_session(fence.clone()).unwrap(),
            MigrationPhaseV2::Fenced,
        );
        source.crash_restart();
        assert_eq!(status(&source, &from).status, SessionStatusV2::Migrated);
        assert_eq!(
            source.client().migrate_session(fence).unwrap(),
            HostedResponseV2::Migration {
                receipt: fenced.clone(),
                snapshot: None
            }
        );
        let activate = migration_request(
            &destination,
            &to,
            &authority,
            &principal,
            &plan,
            MigrationActionV2::Activate,
            Some(fenced),
            None,
            &format!("activate-{backend}"),
        );
        let (activated, _) = migrated(
            destination
                .client()
                .migrate_session(activate.clone())
                .unwrap(),
            MigrationPhaseV2::Activated,
        );
        assert_eq!(
            destination.client().migrate_session(activate).unwrap(),
            HostedResponseV2::Migration {
                receipt: activated,
                snapshot: None
            }
        );
        let verify = if backend == "python" {
            "__oval_result__ = payload['left'] is payload['right'] and payload['left'][0] == 42"
        } else {
            "SELECT value FROM migrated;"
        };
        let outcome = execute(
            &destination,
            &to,
            &authority,
            &principal,
            operation(&format!("verify-{backend}"), backend, verify),
        );
        match outcome {
            OperationOutcomeV2::Succeeded { value, .. } => {
                if backend == "python" {
                    assert_eq!(value, OValue::Bool { v: true });
                } else {
                    assert!(format!("{value:?}").contains("42"), "{value:?}");
                }
            }
            other => panic!("migrated state did not execute: {other:?}"),
        }
        assert!(submit(
            &source,
            &from,
            &authority,
            &principal,
            operation(
                &format!("retired-{backend}"),
                backend,
                "invalid old owner source must not execute"
            )
        )
        .is_err());
        let source_view = status(&source, &from);
        assert!(source
            .client()
            .reset_session(SessionMutationRequestV2 {
                credentials: from.capability.clone().into(),
                client_request_id: format!("reset-fenced-{backend}"),
                client_sequence: source_view.next_client_sequence
            })
            .is_err());
        source
            .client()
            .close_session(SessionMutationRequestV2 {
                credentials: from.capability.clone().into(),
                client_request_id: format!("close-fenced-{backend}"),
                client_sequence: source_view.next_client_sequence,
            })
            .unwrap();
        assert_eq!(status(&source, &from).status, SessionStatusV2::Closed);
    }
}

#[test]
fn receiver_refusal_preserves_source_and_old_destination_checkpoint_with_exact_recovery_authority()
{
    let root = tempfile::tempdir().unwrap();
    let pki = pki(root.path());
    let principal = certificate_leaf_sha256(pki.join("client-cert.pem")).unwrap();
    let authority = PlacementLeaseSignerV2::from_secret_bytes([51; 32]);
    // Both nodes and the authority inspect exactly the same custom adapter.
    // Only state containing this explicit fixture marker refuses RestoreV1.
    let shims = root.path().join("backends");
    fs::create_dir(&shims).unwrap();
    for name in ["python_shim.py", "o_shim_common.py", "o_native_objects.py"] {
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("backends")
                .join(name),
            shims.join(name),
        )
        .unwrap();
    }
    let shim_path = shims.join("python_shim.py");
    let shim = fs::read_to_string(&shim_path).unwrap();
    let needle = "    restored, deleted = _decode_python_globals(checkpoint[\"payload\"])";
    assert_eq!(shim.matches(needle).count(), 1);
    fs::write(&shim_path, shim.replace(needle, &format!("{needle}\n    if restored.get('_refuse_migration_restore', False):\n        raise ValueError('fixture receiver refused restore')"))).unwrap();
    let source = Node::with_shims(
        root.path(),
        &pki,
        "refusal-source",
        52,
        &authority,
        shims.clone(),
    );
    let destination = Node::with_shims(
        root.path(),
        &pki,
        "refusal-destination",
        53,
        &authority,
        shims,
    );
    let from = open(&source, &authority, &principal, "refusal-from", "python");
    let to = open(&destination, &authority, &principal, "refusal-to", "python");
    execute(
        &source,
        &from,
        &authority,
        &principal,
        operation(
            "refusal-source-init",
            "python",
            "value = 42\n_refuse_migration_restore = True\n__oval_result__ = value",
        ),
    );
    execute(
        &destination,
        &to,
        &authority,
        &principal,
        operation(
            "refusal-target-init",
            "python",
            "value = 0\n__oval_result__ = value",
        ),
    );
    let old_destination = status(&destination, &to).actor.checkpoint_sha256.unwrap();
    let plan = MigrationPlanV2 {
        transaction_id: "refused-transfer".to_owned(),
        source: endpoint(&source, &from, &principal),
        destination: endpoint(&destination, &to, &principal),
        checkpoint_sha256: status(&source, &from).actor.checkpoint_sha256.unwrap(),
    };
    let request = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Prepare,
        None,
        None,
        "refusal-prepare",
    );
    let (prepared, snapshot) = migrated(
        source.client().migrate_session(request).unwrap(),
        MigrationPhaseV2::Prepared,
    );
    let install = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Install,
        Some(prepared),
        snapshot,
        "refusal-install",
    );
    let mut unauthorized = install.clone();
    unauthorized.placement_lease.signature = "00".repeat(64);
    assert!(destination.client().migrate_session(unauthorized).is_err());
    assert_eq!(status(&destination, &to).status, SessionStatusV2::Ready);
    let mut wrong_principal = install.clone();
    wrong_principal.credentials.bearer = "ee".repeat(32);
    assert!(destination
        .client()
        .migrate_session(wrong_principal)
        .is_err());
    let (failed, _) = migrated(
        destination
            .client()
            .migrate_session(install.clone())
            .unwrap(),
        MigrationPhaseV2::InstallFailed,
    );
    assert!(migration_state_from_receipt(&failed)
        .unwrap()
        .failure
        .as_ref()
        .unwrap()
        .message
        .contains("fixture receiver refused restore"));
    assert_eq!(
        destination.client().migrate_session(install).unwrap(),
        HostedResponseV2::Migration {
            receipt: failed.clone(),
            snapshot: None
        }
    );
    let fence = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Fence,
        Some(failed),
        None,
        "fence-refused-restore",
    );
    assert!(source.client().migrate_session(fence).is_err());
    assert_eq!(
        status(&source, &from).actor.checkpoint_sha256.as_deref(),
        Some(plan.checkpoint_sha256.as_str())
    );
    assert_eq!(
        status(&destination, &to).actor.checkpoint_sha256.as_deref(),
        Some(old_destination.as_str())
    );
    let abort = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Abort,
        None,
        None,
        "refusal-abort",
    );
    migrated(
        source.client().migrate_session(abort).unwrap(),
        MigrationPhaseV2::Aborted,
    );
    assert_eq!(status(&source, &from).status, SessionStatusV2::Ready);
    assert!(matches!(
        execute(
            &source,
            &from,
            &authority,
            &principal,
            operation(
                "source-still-owned",
                "python",
                "__oval_result__ = value == 42"
            )
        ),
        OperationOutcomeV2::Succeeded {
            value: OValue::Bool { v: true }
        }
    ));

    // The failed physical generation cannot resume. The existing exact
    // recovery warrant restores the destination's old durable value, without
    // replaying the attempted migration or any accepted source operation.
    let view = status(&destination, &to);
    assert_eq!(view.status, SessionStatusV2::RecoveryRequired);
    let warrant = RecoveryWarrantV2 {
        schema: HOSTED_RECOVERY_WARRANT_SCHEMA_V2.to_owned(),
        warrant_id: "recover-old-destination".to_owned(),
        session_id: to.capability.session_id.clone(),
        evidence_sha256: view.journal_head_sha256.clone(),
        trigger: RecoveryTriggerV2::ActorLost {
            previous_actor_generation: view.actor.actor_generation.clone().unwrap(),
            checkpoint_sha256: view.actor.checkpoint_sha256.clone().unwrap(),
            checkpoint_bytes: view.actor.checkpoint_bytes.unwrap(),
            recovery_required_head_sha256: view.journal_head_sha256.clone(),
        },
    };
    let (placement_lease, _) = lease_for_node(
        &destination.node_id,
        &destination.shim_dir,
        Some(warrant.sha256().unwrap()),
        &authority,
        &principal,
        to.state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        quotas(),
        reservation(),
        Some(&to.target),
        view.actor.actor_generation.as_ref(),
        "restore-old-destination",
        view.next_client_sequence,
        PlacementPurposeV2::Recover,
        None,
        &to.proof,
        None,
        4999,
    );
    destination
        .client()
        .recover_session(RecoverSessionRequestV2 {
            credentials: to.capability.clone().into(),
            client_request_id: "restore-old-destination".to_owned(),
            client_sequence: view.next_client_sequence,
            warrant,
            placement_lease,
        })
        .unwrap();
    assert!(matches!(
        execute(
            &destination,
            &to,
            &authority,
            &principal,
            operation(
                "old-target-restored",
                "python",
                "__oval_result__ = value == 0"
            )
        ),
        OperationOutcomeV2::Succeeded {
            value: OValue::Bool { v: true }
        }
    ));
}

#[test]
fn source_abort_cancels_standby_with_signed_proof_and_releases_destination_capacity() {
    let root = tempfile::tempdir().unwrap();
    let pki = pki(root.path());
    let principal = certificate_leaf_sha256(pki.join("client-cert.pem")).unwrap();
    let authority = PlacementLeaseSignerV2::from_secret_bytes([61; 32]);
    let source = Node::new(root.path(), &pki, "abort-source", 62, &authority);
    let destination = Node::new(root.path(), &pki, "abort-target", 63, &authority);
    let from = open(&source, &authority, &principal, "abort-from", "python");
    let to = open(&destination, &authority, &principal, "abort-to", "python");
    execute(
        &source,
        &from,
        &authority,
        &principal,
        operation(
            "abort-source-init",
            "python",
            "value = 42\n__oval_result__ = value",
        ),
    );
    execute(
        &destination,
        &to,
        &authority,
        &principal,
        operation(
            "abort-target-init",
            "python",
            "value = 17\n__oval_result__ = value",
        ),
    );
    let old_destination = status(&destination, &to).actor.checkpoint_sha256.unwrap();
    let plan = MigrationPlanV2 {
        transaction_id: "cancel-standby".to_owned(),
        source: endpoint(&source, &from, &principal),
        destination: endpoint(&destination, &to, &principal),
        checkpoint_sha256: status(&source, &from).actor.checkpoint_sha256.unwrap(),
    };
    let prepare = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Prepare,
        None,
        None,
        "abort-prepare",
    );
    let (prepared, snapshot) = migrated(
        source.client().migrate_session(prepare).unwrap(),
        MigrationPhaseV2::Prepared,
    );
    let install = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Install,
        Some(prepared),
        snapshot,
        "abort-install",
    );
    migrated(
        destination.client().migrate_session(install).unwrap(),
        MigrationPhaseV2::Installed,
    );
    let abort = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Abort,
        None,
        None,
        "abort-before-fence",
    );
    let (aborted, _) = migrated(
        source.client().migrate_session(abort).unwrap(),
        MigrationPhaseV2::Aborted,
    );
    let activate = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Activate,
        Some(aborted.clone()),
        None,
        "activate-after-abort",
    );
    assert!(destination.client().migrate_session(activate).is_err());
    let cancel = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Cancel,
        Some(aborted),
        None,
        "cancel-standby",
    );
    let (cancelled, _) = migrated(
        destination
            .client()
            .migrate_session(cancel.clone())
            .unwrap(),
        MigrationPhaseV2::Cancelled,
    );
    assert_eq!(
        destination.client().migrate_session(cancel).unwrap(),
        HostedResponseV2::Migration {
            receipt: cancelled,
            snapshot: None
        }
    );
    let view = status(&destination, &to);
    assert_eq!(view.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(
        view.actor.checkpoint_sha256.as_deref(),
        Some(old_destination.as_str())
    );
    destination
        .client()
        .close_session(SessionMutationRequestV2 {
            credentials: to.capability.clone().into(),
            client_request_id: "close-cancelled".to_owned(),
            client_sequence: view.next_client_sequence,
        })
        .unwrap();
    assert_eq!(status(&destination, &to).status, SessionStatusV2::Closed);
    // Seven new 8 MiB reservations fit only after the old 8 MiB reservation
    // was released (the total is 64 MiB including durable node metadata).
    for index in 0..7 {
        open(
            &destination,
            &authority,
            &principal,
            &format!("after-cancel-{index}"),
            "python",
        );
    }
    assert!(matches!(
        execute(
            &source,
            &from,
            &authority,
            &principal,
            operation(
                "source-after-cancel",
                "python",
                "__oval_result__ = value == 42"
            )
        ),
        OperationOutcomeV2::Succeeded {
            value: OValue::Bool { v: true }
        }
    ));
}

#[test]
fn crash_during_actual_backend_restore_becomes_a_signed_refusal_and_never_an_ack() {
    let root = tempfile::tempdir().unwrap();
    let pki = pki(root.path());
    let principal = certificate_leaf_sha256(pki.join("client-cert.pem")).unwrap();
    let authority = PlacementLeaseSignerV2::from_secret_bytes([71; 32]);
    let shims = barrier_shims(root.path());
    let source = Node::with_shims(
        root.path(),
        &pki,
        "crash-source",
        72,
        &authority,
        shims.clone(),
    );
    let mut destination =
        Node::with_shims(root.path(), &pki, "crash-target", 73, &authority, shims);
    let from = open(&source, &authority, &principal, "crash-from", "python");
    let to = open(&destination, &authority, &principal, "crash-to", "python");
    let barrier_path = root.path().join("restore-barrier");
    execute(
        &source,
        &from,
        &authority,
        &principal,
        operation(
            "crash-source-init",
            "python",
            &format!(
                "value = 42\n_migration_test_barrier = {:?}\n__oval_result__ = value",
                barrier_path.to_str().unwrap()
            ),
        ),
    );
    execute(
        &destination,
        &to,
        &authority,
        &principal,
        operation(
            "crash-target-init",
            "python",
            "value = 0\n__oval_result__ = value",
        ),
    );
    let old_destination = status(&destination, &to).actor.checkpoint_sha256.unwrap();
    let plan = MigrationPlanV2 {
        transaction_id: "crash-mid-restore".to_owned(),
        source: endpoint(&source, &from, &principal),
        destination: endpoint(&destination, &to, &principal),
        checkpoint_sha256: status(&source, &from).actor.checkpoint_sha256.unwrap(),
    };
    let prepare = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Prepare,
        None,
        None,
        "crash-prepare",
    );
    let (prepared, snapshot) = migrated(
        source.client().migrate_session(prepare).unwrap(),
        MigrationPhaseV2::Prepared,
    );
    let install = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Install,
        Some(prepared),
        snapshot,
        "crash-install",
    );
    let client = destination.client();
    let wire_request = install.clone();
    let pending = thread::spawn(move || client.migrate_session(wire_request));
    let entered = barrier_path.with_extension("entered");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !entered.exists() {
        assert!(
            Instant::now() < deadline,
            "backend did not enter actual RestoreV1: {}",
            destination.stderr()
        );
        thread::sleep(Duration::from_millis(5));
    }
    let mut child = destination.child.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    fs::write(
        barrier_path.with_extension("release"),
        "release orphaned fixture shim",
    )
    .unwrap();
    assert!(
        pending.join().unwrap().is_err(),
        "an interrupted restore must not produce an ACK"
    );
    destination.start();
    let (refused, _) = migrated(
        destination.client().migrate_session(install).unwrap(),
        MigrationPhaseV2::InstallFailed,
    );
    assert_eq!(
        migration_state_from_receipt(&refused)
            .unwrap()
            .failure
            .as_ref()
            .unwrap()
            .code,
        "migration-restore-interrupted"
    );
    let view = status(&destination, &to);
    assert_eq!(view.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(
        view.actor.checkpoint_sha256.as_deref(),
        Some(old_destination.as_str())
    );
    let fence = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Fence,
        Some(refused),
        None,
        "fence-after-crash",
    );
    assert!(source.client().migrate_session(fence).is_err());
    let abort = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Abort,
        None,
        None,
        "abort-after-crash",
    );
    migrated(
        source.client().migrate_session(abort).unwrap(),
        MigrationPhaseV2::Aborted,
    );
    assert!(matches!(
        execute(
            &source,
            &from,
            &authority,
            &principal,
            operation(
                "source-after-crash",
                "python",
                "__oval_result__ = value == 42"
            )
        ),
        OperationOutcomeV2::Succeeded {
            value: OValue::Bool { v: true }
        }
    ));
}

fn barrier_shims(root: &Path) -> PathBuf {
    let shims = root.join("backends");
    fs::create_dir(&shims).unwrap();
    for name in ["python_shim.py", "o_shim_common.py", "o_native_objects.py"] {
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("backends")
                .join(name),
            shims.join(name),
        )
        .unwrap();
    }
    let shim_path = shims.join("python_shim.py");
    let shim = fs::read_to_string(&shim_path).unwrap();
    let needle = "    restored, deleted = _decode_python_globals(checkpoint[\"payload\"])";
    let barrier = "\n    barrier = restored.get('_migration_test_barrier')\n    if barrier:\n        import time\n        with open(barrier + '.entered', 'w') as marker: marker.write('restoring')\n        for attempt in range(500):\n            if os.path.exists(barrier + '.release'): break\n            time.sleep(0.01)";
    assert_eq!(shim.matches(needle).count(), 1);
    fs::write(
        &shim_path,
        shim.replace(needle, &format!("{needle}{barrier}")),
    )
    .unwrap();
    shims
}

#[test]
fn crash_during_activation_preserves_source_fence_and_requires_a_new_restore_ack() {
    let root = tempfile::tempdir().unwrap();
    let pki = pki(root.path());
    let principal = certificate_leaf_sha256(pki.join("client-cert.pem")).unwrap();
    let authority = PlacementLeaseSignerV2::from_secret_bytes([81; 32]);
    let shims = barrier_shims(root.path());
    let source = Node::with_shims(
        root.path(),
        &pki,
        "activate-source",
        82,
        &authority,
        shims.clone(),
    );
    let mut destination =
        Node::with_shims(root.path(), &pki, "activate-target", 83, &authority, shims);
    let from = open(&source, &authority, &principal, "activate-from", "python");
    let to = open(
        &destination,
        &authority,
        &principal,
        "activate-to",
        "python",
    );
    let barrier = root.path().join("activate-barrier");
    let entered = barrier.with_extension("entered");
    let release = barrier.with_extension("release");
    // Initial installation must really restore and ACK. The later fresh
    // activation restore is blocked independently using the same saved state.
    fs::write(&release, "allow initial restore").unwrap();
    execute(
        &source,
        &from,
        &authority,
        &principal,
        operation(
            "activate-source-init",
            "python",
            &format!(
                "value = 42\n_migration_test_barrier = {:?}\n__oval_result__ = value",
                barrier.to_str().unwrap()
            ),
        ),
    );
    execute(
        &destination,
        &to,
        &authority,
        &principal,
        operation(
            "activate-target-init",
            "python",
            "value = 0\n__oval_result__ = value",
        ),
    );
    let plan = MigrationPlanV2 {
        transaction_id: "crash-activation".to_owned(),
        source: endpoint(&source, &from, &principal),
        destination: endpoint(&destination, &to, &principal),
        checkpoint_sha256: status(&source, &from).actor.checkpoint_sha256.unwrap(),
    };
    let prepare = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Prepare,
        None,
        None,
        "activation-prepare",
    );
    let (prepared, snapshot) = migrated(
        source.client().migrate_session(prepare).unwrap(),
        MigrationPhaseV2::Prepared,
    );
    let install = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Install,
        Some(prepared),
        snapshot,
        "activation-install",
    );
    let (installed, _) = migrated(
        destination.client().migrate_session(install).unwrap(),
        MigrationPhaseV2::Installed,
    );
    let installed_generation = migration_state_from_receipt(&installed)
        .unwrap()
        .actor_generation
        .clone();
    let fence = migration_request(
        &source,
        &from,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Fence,
        Some(installed),
        None,
        "activation-fence",
    );
    let (fenced, _) = migrated(
        source.client().migrate_session(fence).unwrap(),
        MigrationPhaseV2::Fenced,
    );
    destination.crash_restart();
    fs::remove_file(&entered).unwrap();
    fs::remove_file(&release).unwrap();
    let activate = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Activate,
        Some(fenced.clone()),
        None,
        "activation-interrupted",
    );
    let client = destination.client();
    let wire_request = activate.clone();
    let pending = thread::spawn(move || client.migrate_session(wire_request));
    let deadline = Instant::now() + Duration::from_secs(3);
    while !entered.exists() {
        assert!(
            Instant::now() < deadline,
            "activation did not enter actual RestoreV1: {}",
            destination.stderr()
        );
        thread::sleep(Duration::from_millis(5));
    }
    let mut child = destination.child.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    fs::write(
        &release,
        "release interrupted fixture and allow new attempt",
    )
    .unwrap();
    assert!(
        pending.join().unwrap().is_err(),
        "interrupted activation cannot acknowledge"
    );
    destination.start();
    let (failed, _) = migrated(
        destination.client().migrate_session(activate).unwrap(),
        MigrationPhaseV2::ActivationFailed,
    );
    let failure = migration_state_from_receipt(&failed).unwrap();
    assert_eq!(
        failure.failure.as_ref().unwrap().code,
        "migration-restore-interrupted"
    );
    assert_eq!(
        failure.actor_generation.generation().get(),
        installed_generation.generation().get() + 1
    );
    assert_eq!(status(&source, &from).status, SessionStatusV2::Migrated);
    assert_eq!(status(&destination, &to).status, SessionStatusV2::Migrating);
    assert!(submit(
        &source,
        &from,
        &authority,
        &principal,
        operation("fenced-source-never-resumes", "python", "value = -1")
    )
    .is_err());
    assert!(submit(
        &destination,
        &to,
        &authority,
        &principal,
        operation("failed-activation-not-ready", "python", "value = -1")
    )
    .is_err());
    let retry = migration_request(
        &destination,
        &to,
        &authority,
        &principal,
        &plan,
        MigrationActionV2::Activate,
        Some(fenced),
        None,
        "activation-new-attempt",
    );
    let (activated, _) = migrated(
        destination.client().migrate_session(retry).unwrap(),
        MigrationPhaseV2::Activated,
    );
    assert_eq!(
        migration_state_from_receipt(&activated)
            .unwrap()
            .actor_generation
            .generation()
            .get(),
        failure.actor_generation.generation().get() + 1
    );
    assert!(matches!(
        execute(
            &destination,
            &to,
            &authority,
            &principal,
            operation(
                "after-activation-crash",
                "python",
                "__oval_result__ = value == 42"
            )
        ),
        OperationOutcomeV2::Succeeded {
            value: OValue::Bool { v: true }
        }
    ));
}
