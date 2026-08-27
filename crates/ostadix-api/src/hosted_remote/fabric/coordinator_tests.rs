use std::sync::{Arc, Condvar, Mutex};

use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::execution_fabric::{
    encode_execution_candidate_v1, CandidateOutputV1, ExecutionCandidateV1, OutputFidelityV1,
    TrustedInlineRendererV1,
};
use crate::execution_fabric_authority::{
    encode_fabric_request_v1, ExecutionCellIncarnationV1, FabricTerminalCandidateV1,
    SignedTerminalCandidateReceiptV1, TerminalCandidateReceiptV1,
    FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1, FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1,
};
use crate::executor::task::TaskOutcome;
use crate::hosted_remote::fabric::coordinator::*;
use crate::placement_protocol::GenerationV1;
use crate::value::OText;

const NODE_ID: &str = "fabric-node-a";
const DEADLINE_UNIX_MS: u64 = 2_000_000_000_000;

struct AcceptanceFixture {
    prepared: PreparedRemotePureAttemptV1,
    node_key: FabricSigningKeyV1,
    terminal: FabricTerminalCandidateV1,
    candidate: ExecutionCandidateV1,
}

fn digest(seed: u8) -> Sha256DigestV1 {
    [seed; 32]
}

fn semantic_digest(seed: u8) -> SemanticDigestV1 {
    SemanticDigestV1::from_sha256(hex::encode(digest(seed))).unwrap()
}

fn fixture_target() -> FabricTargetBindingV1 {
    FabricTargetBindingV1::new(
        semantic_digest(20),
        NODE_ID,
        GenerationV1::new(7).unwrap(),
        ExecutionCellIncarnationV1::new(11).unwrap(),
        semantic_digest(21),
        GenerationV1::new(8).unwrap(),
        GenerationV1::new(9).unwrap(),
        semantic_digest(22),
        semantic_digest(23),
        semantic_digest(24),
        semantic_digest(25),
        semantic_digest(26),
        semantic_digest(27),
        semantic_digest(28),
    )
    .unwrap()
}

fn acceptance_fixture() -> AcceptanceFixture {
    let authority_key = FabricSigningKeyV1::from_secret_bytes([0x11; 32]);
    let node_key = FabricSigningKeyV1::from_secret_bytes([0x22; 32]);
    let execution = ExecutionIdV1::new(digest(1)).unwrap();
    let task = LogicalTaskIdV1::new(execution, digest(2)).unwrap();
    let attempt = AttemptIdV1::new(task, 1).unwrap();
    let input = PortableValueRecord::Core(
        PortableOValue::text(OText {
            utf8: "world".to_string(),
            encoding: Some("utf-8".to_string()),
        })
        .unwrap(),
    );
    let inputs = InputManifestV1::new(vec![InputBindingV1::new("name", &input).unwrap()]).unwrap();
    let region = SourceClosedRendererV1::new(
        TrustedInlineRendererV1::Text,
        vec![
            RendererPartV1::literal("hello "),
            RendererPartV1::input("name"),
        ],
        digest(3),
        digest(4),
        digest(5),
        digest(6),
    )
    .unwrap();
    let output = OutputContractV1::for_renderer(
        REMOTE_RESULT_SLOT_V1,
        TrustedInlineRendererV1::Text,
        MAX_OVALUE_RECORD_BYTES,
    )
    .unwrap();
    let capsule = ExecutionCapsuleV1::new(
        attempt.clone(),
        region,
        digest(7),
        inputs,
        output,
        DEADLINE_UNIX_MS,
        ExecutionLimitsV1::new(30_000, 16 * 1024, MAX_OVALUE_RECORD_BYTES).unwrap(),
    )
    .unwrap();
    let source_closure = FabricSourceClosureV1::new(
        FABRIC_SOURCE_CLOSURE_DIALECT_V1,
        "main = render(name)",
        FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
        "eager",
        digest(10),
        digest(3),
        digest(4),
    )
    .unwrap();
    let target = fixture_target();
    let lease = PlacementLeaseV3::new(
        authority_key.key_id_digest(),
        semantic_digest(29),
        target.clone(),
        &source_closure,
        &capsule,
        UnixMillisV1::new(DEADLINE_UNIX_MS - 30_000),
        UnixMillisV1::new(DEADLINE_UNIX_MS),
    )
    .unwrap();
    let submission = FabricSubmissionV1::new(
        authority_key.sign_execution_lease(lease).unwrap(),
        source_closure,
        encode_execution_capsule_v1(&capsule).unwrap(),
    )
    .unwrap();
    let output_record = PortableValueRecord::Core(
        PortableOValue::text(OText {
            utf8: "hello world".to_string(),
            encoding: Some("utf-8".to_string()),
        })
        .unwrap(),
    );
    let candidate = ExecutionCandidateV1::new(
        &capsule,
        CandidateOutcomeV1::Succeeded {
            output: CandidateOutputV1::new(
                REMOTE_RESULT_SLOT_V1,
                &output_record,
                OutputValueKindV1::Text,
                OutputFidelityV1::Structural,
            )
            .unwrap(),
        },
        DEADLINE_UNIX_MS - 1,
    )
    .unwrap();
    let terminal = node_key
        .sign_terminal_candidate(
            &submission,
            encode_execution_candidate_v1(&candidate).unwrap(),
            25,
        )
        .unwrap();
    let pinned_node = PinnedFabricNodeKeyV1::new(
        target.node_id(),
        target.node_generation(),
        target.execution_cell_incarnation(),
        node_key.public_key(),
    )
    .unwrap();
    let mut trusted_authorities = TrustedFabricAuthoritiesV1::new();
    trusted_authorities.enroll(authority_key.public_key());
    let (_tls_directory, tls_server) = crate::hosted_remote::test_server_tls_identity().unwrap();
    let client = FabricAttemptClientV1::new(
        "127.0.0.1:9".parse().unwrap(),
        ClientTlsIdentity {
            ca_path: tls_server.client_ca_path,
            cert_path: tls_server.cert_path,
            key_path: tls_server.key_path,
            server_name: "localhost".to_string(),
        },
        semantic_digest(30).as_sha256(),
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .unwrap();
    let coordinator_attempt_started = Instant::now();
    let coordinator_attempt_deadline = coordinator_attempt_started
        .checked_add(Duration::from_secs(30))
        .unwrap();
    let prepared = PreparedRemotePureAttemptV1 {
        attempt,
        client,
        expected_server_principal_sha256: semantic_digest(30),
        pinned_node,
        trusted_authorities,
        target,
        submission,
        capsule,
        poll_interval: Duration::from_millis(1),
        attempt_lifetime: Duration::from_secs(30),
        coordinator_attempt_started,
        coordinator_attempt_deadline,
    };
    AcceptanceFixture {
        prepared,
        node_key,
        terminal,
        candidate,
    }
}

fn value_field_mut<'a>(value: &'a mut Value, path: &[&str]) -> &'a mut Value {
    let mut current = value;
    for field in path {
        current = current
            .get_mut(*field)
            .unwrap_or_else(|| panic!("fixture omitted `{field}` in path {path:?}"));
    }
    current
}

fn resign_terminal(
    fixture: &AcceptanceFixture,
    candidate_bytes: Vec<u8>,
    mutate: impl FnOnce(&mut Value),
) -> FabricTerminalCandidateV1 {
    let mut receipt_value =
        serde_json::to_value(fixture.terminal.signed_receipt().receipt()).unwrap();
    *value_field_mut(&mut receipt_value, &["candidate_payload", "byte_length"]) =
        Value::from(candidate_bytes.len() as u64);
    *value_field_mut(&mut receipt_value, &["candidate_payload", "sha256"]) =
        serde_json::to_value(<[u8; 32]>::from(Sha256::digest(&candidate_bytes))).unwrap();
    mutate(&mut receipt_value);
    let receipt: TerminalCandidateReceiptV1 = serde_json::from_value(receipt_value).unwrap();
    let body = crate::canonical_cbor::encode(&receipt).unwrap();
    let preimage =
        crate::canonical_cbor::signing_preimage(FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1, &body)
            .unwrap();
    let key = SigningKey::from_bytes(&fixture.node_key.secret_bytes());
    let signed = SignedTerminalCandidateReceiptV1 {
        schema: FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1.to_string(),
        receipt,
        signer_public_key: hex::encode(key.verifying_key().to_bytes()),
        signer_key_id: fixture.node_key.key_id_hex(),
        signature: hex::encode(key.sign(&preimage).to_bytes()),
    };
    FabricTerminalCandidateV1::from_wire(signed, candidate_bytes).unwrap()
}

fn assert_gate(gate: u8, fixture: &AcceptanceFixture, terminal: &FabricTerminalCandidateV1) {
    let error = accept_terminal_candidate(
        &fixture.prepared,
        DEADLINE_UNIX_MS + 1,
        Duration::from_millis(1),
        terminal,
    )
    .expect_err("faulted candidate must be rejected");
    assert!(
        error.to_string().contains(&format!("gate {gate:02}")),
        "expected gate {gate:02}, got {error:#}"
    );
}

#[test]
fn terminal_acceptance_is_valid_and_lowest_gate_wins() {
    let valid = acceptance_fixture();
    let value = accept_terminal_candidate(
        &valid.prepared,
        DEADLINE_UNIX_MS - 100,
        Duration::from_millis(1),
        &valid.terminal,
    )
    .unwrap();
    assert_eq!(value, OValue::str_("hello world"));

    let mut gate3 = acceptance_fixture();
    gate3.prepared.pinned_node = PinnedFabricNodeKeyV1::new(
        NODE_ID,
        gate3.prepared.target.node_generation(),
        gate3.prepared.target.execution_cell_incarnation(),
        FabricSigningKeyV1::from_secret_bytes([0x33; 32]).public_key(),
    )
    .unwrap();
    assert_gate(3, &gate3, &gate3.terminal);

    let mut gate4 = acceptance_fixture();
    gate4.prepared.attempt = AttemptIdV1::new(
        LogicalTaskIdV1::new(ExecutionIdV1::new(digest(40)).unwrap(), digest(2)).unwrap(),
        1,
    )
    .unwrap();
    assert_gate(4, &gate4, &gate4.terminal);

    let mut gate5 = acceptance_fixture();
    gate5.prepared.attempt = AttemptIdV1::new(
        LogicalTaskIdV1::new(
            gate5.prepared.attempt.task().execution().clone(),
            digest(41),
        )
        .unwrap(),
        1,
    )
    .unwrap();
    assert_gate(5, &gate5, &gate5.terminal);

    let mut gate6 = acceptance_fixture();
    gate6.prepared.attempt = AttemptIdV1::new(gate6.prepared.attempt.task().clone(), 2).unwrap();
    assert_gate(6, &gate6, &gate6.terminal);

    for (gate, path, replacement) in [
        (7, vec!["node_generation"], Value::from(8_u64)),
        (
            9,
            vec!["lease_nonce"],
            serde_json::to_value(semantic_digest(42)).unwrap(),
        ),
        (
            10,
            vec!["capsule_sha256"],
            serde_json::to_value(digest(43)).unwrap(),
        ),
        (
            11,
            vec!["source_closure_sha256"],
            serde_json::to_value(digest(44)).unwrap(),
        ),
        (
            12,
            vec!["input_manifest_sha256"],
            serde_json::to_value(digest(45)).unwrap(),
        ),
        (
            13,
            vec!["backend_catalog_sha256"],
            serde_json::to_value(digest(46)).unwrap(),
        ),
        (
            14,
            vec!["backend_implementation_sha256"],
            serde_json::to_value(digest(47)).unwrap(),
        ),
        (
            15,
            vec!["output_contract_sha256"],
            serde_json::to_value(digest(48)).unwrap(),
        ),
    ] {
        let fixture = acceptance_fixture();
        let terminal = resign_terminal(
            &fixture,
            encode_execution_candidate_v1(&fixture.candidate).unwrap(),
            |receipt| *value_field_mut(receipt, &path) = replacement,
        );
        assert_gate(gate, &fixture, &terminal);
    }

    let mut gate8 = acceptance_fixture();
    gate8.prepared.trusted_authorities = TrustedFabricAuthoritiesV1::new();
    assert_gate(8, &gate8, &gate8.terminal);

    let gate16 = acceptance_fixture();
    let mut wrong_kind = gate16.candidate.clone();
    let output_record = PortableValueRecord::Core(
        PortableOValue::text(OText {
            utf8: "hello world".to_string(),
            encoding: Some("utf-8".to_string()),
        })
        .unwrap(),
    );
    wrong_kind.outcome = CandidateOutcomeV1::Succeeded {
        output: CandidateOutputV1::new(
            REMOTE_RESULT_SLOT_V1,
            &output_record,
            OutputValueKindV1::Html,
            OutputFidelityV1::Presentation,
        )
        .unwrap(),
    };
    let terminal16 = resign_terminal(
        &gate16,
        encode_execution_candidate_v1(&wrong_kind).unwrap(),
        |_| {},
    );
    assert_gate(16, &gate16, &terminal16);

    let gate17 = acceptance_fixture();
    let mut wrong_content = gate17.candidate.clone();
    let changed_record = PortableValueRecord::Core(
        PortableOValue::text(OText {
            utf8: "tampered".to_string(),
            encoding: Some("utf-8".to_string()),
        })
        .unwrap(),
    );
    wrong_content.outcome = CandidateOutcomeV1::Succeeded {
        output: CandidateOutputV1::new(
            REMOTE_RESULT_SLOT_V1,
            &changed_record,
            OutputValueKindV1::Text,
            OutputFidelityV1::Structural,
        )
        .unwrap(),
    };
    let terminal17 = resign_terminal(
        &gate17,
        encode_execution_candidate_v1(&wrong_content).unwrap(),
        |_| {},
    );
    assert_gate(17, &gate17, &terminal17);

    let gate18 = acceptance_fixture();
    let error = accept_terminal_candidate(
        &gate18.prepared,
        DEADLINE_UNIX_MS + 1,
        Duration::from_millis(1),
        &gate18.terminal,
    )
    .unwrap_err();
    assert!(error.to_string().contains("gate 18"), "{error:#}");
}

#[test]
fn wrong_tls_principal_stops_at_gate_02() {
    let fixture = acceptance_fixture();
    let error =
        require_server_principal(&fixture.prepared, semantic_digest(99).as_sha256()).unwrap_err();
    assert!(error.to_string().contains("gate 02"), "{error:#}");
}

#[test]
fn terminal_from_prior_execution_cell_incarnation_stops_at_gate_07() {
    let fixture = acceptance_fixture();
    let current = fixture.prepared.target.execution_cell_incarnation().get();
    assert!(
        current > 1,
        "fixture needs a representable prior incarnation"
    );
    let terminal = resign_terminal(
        &fixture,
        encode_execution_candidate_v1(&fixture.candidate).unwrap(),
        |receipt| {
            *value_field_mut(receipt, &["execution_cell_incarnation"]) = Value::from(current - 1);
        },
    );

    assert_gate(7, &fixture, &terminal);
}

#[test]
fn candidate_payload_digest_is_gate_17_not_gate_01() {
    let fixture = acceptance_fixture();
    let mut changed = fixture.candidate.clone();
    let record = PortableValueRecord::Core(
        PortableOValue::text(OText {
            utf8: "changed but canonical".to_string(),
            encoding: Some("utf-8".to_string()),
        })
        .unwrap(),
    );
    changed.outcome = CandidateOutcomeV1::Succeeded {
        output: CandidateOutputV1::new(
            REMOTE_RESULT_SLOT_V1,
            &record,
            OutputValueKindV1::Text,
            OutputFidelityV1::Structural,
        )
        .unwrap(),
    };
    let terminal = FabricTerminalCandidateV1::from_wire(
        fixture.terminal.signed_receipt().clone(),
        encode_execution_candidate_v1(&changed).unwrap(),
    )
    .expect("canonical payload with a stale descriptor reaches ordered acceptance");
    assert_gate(17, &fixture, &terminal);
}

#[test]
fn worker_reported_late_candidate_is_gate_18() {
    let fixture = acceptance_fixture();
    let mut late = fixture.candidate.clone();
    late.completed_unix_ms = DEADLINE_UNIX_MS + 1;
    let terminal = resign_terminal(
        &fixture,
        encode_execution_candidate_v1(&late).unwrap(),
        |receipt| {
            *value_field_mut(receipt, &["provider_completed_unix_ms"]) =
                Value::from(DEADLINE_UNIX_MS + 1);
        },
    );
    let error = accept_terminal_candidate(
        &fixture.prepared,
        DEADLINE_UNIX_MS - 1,
        Duration::from_millis(1),
        &terminal,
    )
    .expect_err("worker-reported completion after the signed deadline must be rejected");
    assert!(error.to_string().contains("gate 18"), "{error:#}");
}

#[test]
fn preparation_time_deadline_is_not_refreshed_when_worker_execution_begins() {
    let mut fixture = acceptance_fixture();
    let expired = Instant::now();
    fixture.prepared.coordinator_attempt_started = expired
        .checked_sub(fixture.prepared.attempt_lifetime)
        .unwrap();
    fixture.prepared.coordinator_attempt_deadline = expired;

    let error = fixture
        .prepared
        .execute_remote()
        .expect_err("queue delay must consume the signed attempt lifetime");

    assert!(error.to_string().contains("gate 18"), "{error:#}");
}

#[test]
fn client_failure_taxonomy_preserves_acceptance_gate_causality() {
    let representation = map_client_failure(FabricClientFailureV1::ResponseRepresentation(
        anyhow::anyhow!("truncated authenticated frame"),
    ));
    assert!(representation.to_string().contains("gate 01"));

    let deadline = map_client_failure(FabricClientFailureV1::Deadline);
    assert!(deadline.to_string().contains("gate 18"));

    for infrastructure in [
        FabricClientFailureV1::Connection(anyhow::anyhow!("connect")),
        FabricClientFailureV1::RequestPreparation(anyhow::anyhow!("encode")),
        FabricClientFailureV1::RequestTransport(anyhow::anyhow!("write")),
        FabricClientFailureV1::ResponseTransport(anyhow::anyhow!("read")),
    ] {
        let error = map_client_failure(infrastructure);
        assert!(
            !error.to_string().contains("acceptance gate"),
            "infrastructure failure was mislabeled as an acceptance gate: {error:#}"
        );
    }
}

fn remote_completion(attempt: AttemptIdV1, token: TaskToken, value: &str) -> WorkerEvent {
    WorkerEvent::Completion(crate::executor::task::TaskCompletion {
        token,
        physical_attempt: Some(physical_attempt_coordinate_v1(&attempt).unwrap()),
        outcome: crate::executor::task::TaskOutcome::Completed(Ok(Box::new(OValue::str_(value)))),
    })
}

#[test]
fn remote_driver_forwards_current_attempt_once_and_discards_fenced_results() {
    let execution = ExecutionIdV1::new(digest(60)).unwrap();
    let task = LogicalTaskIdV1::new(execution, digest(61)).unwrap();
    let first = AttemptIdV1::new(task.clone(), 1).unwrap();
    let second = AttemptIdV1::new(task, 2).unwrap();
    let token = TaskToken(73);
    let mut driver = RemotePureAttemptDriver {
        pool: WorkerPool::new(1).unwrap(),
        remote_attempts: vec![
            RemoteAttemptBindingV1 {
                attempt: physical_attempt_coordinate_v1(&first).unwrap(),
                token,
                lifecycle: RemoteAttemptLifecycleV1::Fenced,
            },
            RemoteAttemptBindingV1 {
                attempt: physical_attempt_coordinate_v1(&second).unwrap(),
                token,
                lifecycle: RemoteAttemptLifecycleV1::Active,
            },
        ],
        active_local: HashSet::new(),
        seen_local: HashSet::new(),
    };

    assert!(driver
        .accept_event(remote_completion(first, token, "stale"))
        .unwrap()
        .is_none());
    let accepted = driver
        .accept_event(remote_completion(second.clone(), token, "current"))
        .unwrap()
        .expect("current attempt emits one coordinator event");
    let WorkerEvent::Completion(completion) = accepted else {
        panic!("remote candidate changed event class")
    };
    assert_eq!(completion.token, token);
    assert_eq!(
        completion.physical_attempt(),
        Some(physical_attempt_coordinate_v1(&second).unwrap())
    );
    assert!(matches!(
        completion.outcome,
        TaskOutcome::Completed(Ok(value)) if *value == OValue::str_("current")
    ));
    assert!(driver
        .accept_event(remote_completion(second, token, "duplicate"))
        .unwrap()
        .is_none());
}

struct ImmediateTask(&'static str);

impl PreparedTask for ImmediateTask {
    fn execute(self: Box<Self>, _context: &crate::executor::task::TaskContext) -> Result<OValue> {
        Ok(OValue::str_(self.0))
    }
}

struct BlockingRemoteTask {
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl PreparedTask for BlockingRemoteTask {
    fn execute(self: Box<Self>, _context: &crate::executor::task::TaskContext) -> Result<OValue> {
        let (released, changed) = &*self.gate;
        let released = released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let released = changed
            .wait_while(released, |released| !*released)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(*released);
        Ok(OValue::str_("released"))
    }
}

fn empty_remote_driver(capacity: usize) -> RemotePureAttemptDriver {
    RemotePureAttemptDriver {
        pool: WorkerPool::new(capacity).unwrap(),
        remote_attempts: Vec::new(),
        active_local: HashSet::new(),
        seen_local: HashSet::new(),
    }
}

fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let (released, changed) = &**gate;
    *released
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    changed.notify_all();
}

#[test]
fn remote_driver_failed_superseding_submit_keeps_predecessor_active() {
    let task = LogicalTaskIdV1::new(ExecutionIdV1::new(digest(70)).unwrap(), digest(71)).unwrap();
    let first = AttemptIdV1::new(task.clone(), 1).unwrap();
    let second = AttemptIdV1::new(task, 2).unwrap();
    let token = TaskToken(74);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mut driver = empty_remote_driver(1);
    driver
        .submit(TaskSubmission::physical(
            token,
            physical_attempt_coordinate_v1(&first).unwrap(),
            Box::new(BlockingRemoteTask {
                gate: Arc::clone(&gate),
            }),
        ))
        .unwrap();

    let error = driver
        .submit(TaskSubmission::physical(
            token,
            physical_attempt_coordinate_v1(&second).unwrap(),
            Box::new(ImmediateTask("must-not-run")),
        ))
        .expect_err("at-capacity successor must fail before fencing its predecessor");
    assert!(error.to_string().contains("at capacity"), "{error:#}");
    assert_eq!(driver.remote_attempts.len(), 1);
    assert_eq!(
        driver.remote_attempts[0].attempt,
        physical_attempt_coordinate_v1(&first).unwrap()
    );
    assert_eq!(
        driver.remote_attempts[0].lifecycle,
        RemoteAttemptLifecycleV1::Active
    );

    release(&gate);
    let WorkerEvent::Completion(completion) = driver.recv_event().unwrap() else {
        panic!("released predecessor returned a callback")
    };
    assert_eq!(
        completion.physical_attempt(),
        Some(physical_attempt_coordinate_v1(&first).unwrap())
    );
    assert_eq!(
        driver.remote_attempts[0].lifecycle,
        RemoteAttemptLifecycleV1::Delivered
    );
}

#[test]
fn remote_driver_rejects_exact_reuse_and_generation_rollback() {
    let task = LogicalTaskIdV1::new(ExecutionIdV1::new(digest(72)).unwrap(), digest(73)).unwrap();
    let first = AttemptIdV1::new(task.clone(), 1).unwrap();
    let third = AttemptIdV1::new(task.clone(), 3).unwrap();
    let second = AttemptIdV1::new(task, 2).unwrap();
    let token = TaskToken(75);
    let mut driver = empty_remote_driver(1);
    driver.remote_attempts = vec![
        RemoteAttemptBindingV1 {
            attempt: physical_attempt_coordinate_v1(&first).unwrap(),
            token,
            lifecycle: RemoteAttemptLifecycleV1::Fenced,
        },
        RemoteAttemptBindingV1 {
            attempt: physical_attempt_coordinate_v1(&third).unwrap(),
            token,
            lifecycle: RemoteAttemptLifecycleV1::Fenced,
        },
    ];

    assert!(driver
        .submit(TaskSubmission::physical(
            token,
            physical_attempt_coordinate_v1(&first).unwrap(),
            Box::new(ImmediateTask("duplicate")),
        ))
        .unwrap_err()
        .to_string()
        .contains("previously seen"));
    assert!(driver
        .submit(TaskSubmission::physical(
            token,
            physical_attempt_coordinate_v1(&second).unwrap(),
            Box::new(ImmediateTask("rollback")),
        ))
        .unwrap_err()
        .to_string()
        .contains("stale attempt generation"));
    assert_eq!(driver.remote_attempts.len(), 2);
    assert_eq!(driver.outstanding(), 0);
}

#[test]
fn remote_driver_rejects_task_token_rebinding() {
    let execution = ExecutionIdV1::new(digest(74)).unwrap();
    let first_task = LogicalTaskIdV1::new(execution.clone(), digest(75)).unwrap();
    let other_task = LogicalTaskIdV1::new(execution, digest(76)).unwrap();
    let first = AttemptIdV1::new(first_task, 1).unwrap();
    let other = AttemptIdV1::new(other_task, 1).unwrap();
    let token = TaskToken(76);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mut driver = empty_remote_driver(1);
    driver
        .submit(TaskSubmission::physical(
            token,
            physical_attempt_coordinate_v1(&first).unwrap(),
            Box::new(BlockingRemoteTask {
                gate: Arc::clone(&gate),
            }),
        ))
        .unwrap();
    let error = driver
        .submit(TaskSubmission::physical(
            token,
            physical_attempt_coordinate_v1(&other).unwrap(),
            Box::new(ImmediateTask("wrong-task")),
        ))
        .unwrap_err();
    assert!(
        error.to_string().contains("different logical task"),
        "{error:#}"
    );
    assert_eq!(driver.remote_attempts.len(), 1);
    release(&gate);
    driver.recv_event().unwrap();
}

#[test]
fn real_pool_preserves_remote_attempt_mapping_exactly_once() {
    let task = LogicalTaskIdV1::new(ExecutionIdV1::new(digest(77)).unwrap(), digest(78)).unwrap();
    let attempt = AttemptIdV1::new(task, 1).unwrap();
    let token = TaskToken(77);
    let mut driver = empty_remote_driver(1);
    driver
        .submit(TaskSubmission::physical(
            token,
            physical_attempt_coordinate_v1(&attempt).unwrap(),
            Box::new(ImmediateTask("remote-value")),
        ))
        .unwrap();
    let WorkerEvent::Completion(completion) = driver.recv_event().unwrap() else {
        panic!("immediate remote task returned a callback")
    };
    assert_eq!(completion.token, token);
    assert_eq!(
        completion.physical_attempt(),
        Some(physical_attempt_coordinate_v1(&attempt).unwrap())
    );
    assert_eq!(
        driver.remote_attempts[0].lifecycle,
        RemoteAttemptLifecycleV1::Delivered
    );
    assert!(driver.try_recv_event().unwrap().is_none());
}

struct NeverExecutedTask;

impl PreparedTask for NeverExecutedTask {
    fn execute(self: Box<Self>, _context: &crate::executor::task::TaskContext) -> Result<OValue> {
        panic!("wire-surface test task must remain coordinator-local")
    }
}

fn reject_forbidden_coordinate_keys(value: &Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert!(
                    ![
                        "task_token",
                        "operation_index",
                        "plan_node_id",
                        "hgraph_node_id",
                        "hgraph_edge_index",
                        "ordinal",
                    ]
                    .contains(&key.as_str()),
                    "Fabric wire exposed forbidden local coordinate `{key}`"
                );
                reject_forbidden_coordinate_keys(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_forbidden_coordinate_keys(value);
            }
        }
        _ => {}
    }
}

#[test]
fn fabric_request_contains_no_local_task_or_graph_coordinates() {
    let fixture = acceptance_fixture();
    let request = FabricRequestV1::SubmitPureAttempt(fixture.prepared.submission.clone());
    let encoded = encode_fabric_request_v1(&request).unwrap();
    let header: Value = crate::canonical_cbor::decode(encoded.header_bytes()).unwrap();
    let capsule: Value = crate::canonical_cbor::decode(encoded.payload_bytes().unwrap()).unwrap();
    reject_forbidden_coordinate_keys(&header);
    reject_forbidden_coordinate_keys(&capsule);

    let sentinel = TaskToken(usize::MAX - 17);
    let local = TaskSubmission::physical(
        sentinel,
        physical_attempt_coordinate_v1(&fixture.prepared.attempt).unwrap(),
        Box::new(NeverExecutedTask),
    );
    assert_eq!(local.token(), sentinel);
    assert_eq!(
        local.physical_attempt(),
        Some(physical_attempt_coordinate_v1(&fixture.prepared.attempt).unwrap())
    );
}
