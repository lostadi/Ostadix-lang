use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use o_lang::eval::{Evaluator, PlacementFragmentBindingsV2};
use o_lang::hosted_remote::v2::{
    build_local_dev_placement_proof_v2, open_capability_commitment_v2, validate_hosted_response_v2,
    DenyAllPlacementAuthorizerV2, DurableSessionStoreV2, HostedCommandBindingV2,
    HostedNodeSignerV2, HostedPlacementAuthorityV2, HostedProtocolErrorV2, HostedRequestV2,
    HostedResponseV2, HostedV2RuntimeClosedV2, HostedV2RuntimeConfig, HostedV2RuntimeHandle,
    HostedV2RuntimeOwner, HostedV2RuntimeShutdownErrorV2, JournalEntryV2, JournalEventV2,
    LocalDevPlacementConfigV2, OpenSessionRequestV2, OperationStatusV2,
    PinnedEd25519PlacementAuthorizerV2, PlacementLeaseSignerV2, PlacementPurposeV2,
    PreparedOperationV2, SessionCapabilityV2, SessionMutationRequestV2, SessionQueryV2,
    SessionStateTierV2, SignedJournalEntryV2, SignedPlacementLeaseV2, SubmitOperationRequestV2,
    HOSTED_COMMAND_BINDING_SCHEMA_V2, HOSTED_JOURNAL_ENTRY_SCHEMA_V2, HOSTED_PROTOCOL_V2,
};
use o_lang::hosted_remote::{canonical_hosted_sha256, unix_time_ms, MAX_HOSTED_OUTPUT_BYTES};
use o_lang::ir::BackendRegistry;
use o_lang::placement::{
    ActorGenerationIdV1, CanonicalPlacementRecordV1, EnvironmentRequirementV1, GenerationV1,
    LeaseExpectationV2, LeaseStateBindingV2, PlacementLeaseV2, PlacementReservationV1,
    RequirementAtomV1, SemanticDigestV1, StateCapacityObservationV2, StateControlExpectationV2,
    StateControlLeaseV2, StateQuotaLimitsV2, StateReservationV2, StateSessionIdV2,
    TargetDescriptorV1, TaskAttemptIdV1, UnixMillisV1,
};

const NODE_ID: &str = "node-v2-test";
// Capacity observations are protocol-bounded to a 5-second inclusive span;
// fixtures start one millisecond before `now`, so 4_999 is the exact maximum.
const TEST_EVIDENCE_VALIDITY_MS: u64 = 4_999;
const EXPIRED_RETRY_VALIDITY_MS: u64 = 4_000;
const MAX_FRESH_PLACEMENT_ATTEMPTS: usize = 3;

#[derive(Clone)]
struct OpenedSession {
    capability: SessionCapabilityV2,
    state_session: StateSessionIdV2,
    reservation: StateReservationV2,
    tier: SessionStateTierV2,
    target: TargetDescriptorV1,
    open_request: OpenSessionRequestV2,
    open_receipt: SignedJournalEntryV2,
}

fn digest(label: &str) -> SemanticDigestV1 {
    SemanticDigestV1::hash_bytes("ostadix/hosted-v2-test/v1", label.as_bytes())
}

fn principal_digest(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}

fn open_capability(state_session: &StateSessionIdV2, request_id: &str) -> SessionCapabilityV2 {
    SessionCapabilityV2 {
        session_id: state_session.semantic_digest().unwrap().to_string(),
        bearer: digest(&format!("bearer:{request_id}")).to_string(),
    }
}

fn quotas(max_open_sessions: u32) -> StateQuotaLimitsV2 {
    StateQuotaLimitsV2::new(
        max_open_sessions,
        1,
        4 * 1024 * 1024,
        8 * 1024 * 1024,
        64 * 1024 * 1024,
    )
    .unwrap()
}

fn reservation() -> StateReservationV2 {
    StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap()
}

struct OwnedRuntimeV2 {
    owner: HostedV2RuntimeOwner,
    handle: HostedV2RuntimeHandle,
}

impl OwnedRuntimeV2 {
    fn from_owner(owner: HostedV2RuntimeOwner) -> Self {
        let handle = owner.handle();
        Self { owner, handle }
    }

    fn handle(&self) -> HostedV2RuntimeHandle {
        self.handle.clone()
    }

    fn shutdown(&self) -> anyhow::Result<()> {
        self.owner.shutdown()
    }
}

impl Deref for OwnedRuntimeV2 {
    type Target = HostedV2RuntimeHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

fn runtime(
    root: &Path,
    node_signer: HostedNodeSignerV2,
    placement_signer: &PlacementLeaseSignerV2,
    state_quotas: StateQuotaLimitsV2,
) -> OwnedRuntimeV2 {
    let store = DurableSessionStoreV2::open(root, node_signer).unwrap();
    OwnedRuntimeV2::from_owner(
        HostedV2RuntimeOwner::open(
            HostedV2RuntimeConfig {
                node_id: NODE_ID.to_owned(),
                node_generation: GenerationV1::new(1).unwrap(),
                shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
                runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
                state_quota_generation: GenerationV1::new(1).unwrap(),
                state_quotas,
            },
            store,
            Arc::new(PinnedEd25519PlacementAuthorizerV2::new(
                placement_signer.public_key(),
            )),
        )
        .unwrap(),
    )
}

#[allow(clippy::too_many_arguments)]
fn lease(
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
) -> (SignedPlacementLeaseV2, TargetDescriptorV1) {
    lease_with_validity(
        signer,
        principal,
        state_session,
        state_tier,
        state_quotas,
        state_reservation,
        established_target,
        actor_generation,
        request_id,
        sequence,
        purpose,
        operation_sha256,
        operation,
        None,
        TEST_EVIDENCE_VALIDITY_MS,
    )
}

#[allow(clippy::too_many_arguments)]
fn lease_with_validity(
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
    let bindings = prepare_bindings(operation);
    let now = unix_time_ms().unwrap();
    let establishing_logical_environment = purpose == PlacementPurposeV2::OpenSession
        || (purpose == PlacementPurposeV2::Execute
            && state_tier != SessionStateTierV2::Stateless
            && actor_generation.is_none());
    let provisional = build_local_dev_placement_proof_v2(
        &bindings,
        signer.issuer_key(),
        LocalDevPlacementConfigV2 {
            node_id: NODE_ID.to_owned(),
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
        node_id: NODE_ID.to_owned(),
        principal_sha256: principal.to_owned(),
        state_session: state_session.clone(),
        session_state_tier: state_tier,
        client_request_id: request_id.to_owned(),
        client_sequence: sequence,
        purpose,
        operation_sha256,
        recovery_warrant_sha256: None,
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
                NODE_ID,
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
            NODE_ID,
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
            NODE_ID,
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

fn prepare_bindings(operation: &PreparedOperationV2) -> PlacementFragmentBindingsV2 {
    let mut evaluator = Evaluator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .with_registered_backends(BackendRegistry::global().registered_backend_tags())
        .with_runtime_executable(Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf());
    evaluator
        .prepare_placement_fragment(&operation.source_utf8, operation.task_attempt.clone())
        .unwrap()
        .bindings()
        .clone()
}

fn actor_for(
    bindings: &PlacementFragmentBindingsV2,
    target: &TargetDescriptorV1,
    generation: u64,
) -> ActorGenerationIdV1 {
    let logical_environment = bindings
        .requirement_footprint()
        .known_atoms()
        .iter()
        .find_map(|requirement| match requirement {
            RequirementAtomV1::Environment(EnvironmentRequirementV1::SameLogicalEnvironment {
                identity,
            }) => Some(identity.clone()),
            _ => None,
        })
        .expect("stateful fragment has a logical environment");
    ActorGenerationIdV1::new(
        logical_environment,
        bindings.backend_implementation_sha256().clone(),
        target.semantic_digest().unwrap(),
        bindings.sandbox_policy_sha256().clone(),
        bindings.backend_launch_generation().clone(),
        GenerationV1::new(generation).unwrap(),
    )
}

fn resign_operation_actor(
    signer: &HostedNodeSignerV2,
    receipt: &SignedJournalEntryV2,
    actor: ActorGenerationIdV1,
) -> SignedJournalEntryV2 {
    let mut entry = receipt.entry.clone();
    let o_lang::hosted_remote::v2::JournalEventV2::OperationAccepted {
        actor_generation, ..
    } = &mut entry.event
    else {
        panic!("test receipt is not OperationAccepted")
    };
    *actor_generation = Some(actor);
    signer.issue_journal_entry(entry).unwrap()
}

fn open_session(
    runtime: &HostedV2RuntimeHandle,
    placement_signer: &PlacementLeaseSignerV2,
    principal: &str,
    request_id: &str,
    tier: SessionStateTierV2,
    state_quotas: StateQuotaLimitsV2,
) -> OpenedSession {
    open_session_with_reservation(
        runtime,
        placement_signer,
        principal,
        request_id,
        tier,
        state_quotas,
        reservation(),
    )
}

fn open_session_with_reservation(
    runtime: &HostedV2RuntimeHandle,
    placement_signer: &PlacementLeaseSignerV2,
    principal: &str,
    request_id: &str,
    tier: SessionStateTierV2,
    state_quotas: StateQuotaLimitsV2,
    reservation: StateReservationV2,
) -> OpenedSession {
    open_session_with_reservation_and_validity(
        runtime,
        placement_signer,
        principal,
        request_id,
        tier,
        state_quotas,
        reservation,
        TEST_EVIDENCE_VALIDITY_MS,
    )
}

#[allow(clippy::too_many_arguments)]
fn open_session_with_reservation_and_validity(
    runtime: &HostedV2RuntimeHandle,
    placement_signer: &PlacementLeaseSignerV2,
    principal: &str,
    request_id: &str,
    tier: SessionStateTierV2,
    state_quotas: StateQuotaLimitsV2,
    reservation: StateReservationV2,
    validity_ms: u64,
) -> OpenedSession {
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest(&format!("session:{request_id}")),
    )
    .unwrap();
    let proof_operation = operation("open-proof", tier);
    let (placement_lease, target) = lease_with_validity(
        placement_signer,
        principal,
        state_session.clone(),
        tier,
        state_quotas,
        reservation.clone(),
        None,
        None,
        request_id,
        0,
        PlacementPurposeV2::OpenSession,
        None,
        &proof_operation,
        None,
        validity_ms,
    );
    let capability = open_capability(&state_session, request_id);
    let request = OpenSessionRequestV2 {
        client_request_id: request_id.to_owned(),
        state_tier: tier,
        capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
        proposed_capability: capability,
        placement_lease,
    };
    let response = runtime.open_session(principal, request.clone()).unwrap();
    match response {
        HostedResponseV2::SessionOpened {
            capability,
            receipt,
        } => {
            receipt.verify().unwrap();
            assert_eq!(
                capability.session_id,
                state_session.semantic_digest().unwrap().to_string()
            );
            OpenedSession {
                capability,
                state_session,
                reservation,
                tier,
                target,
                open_request: request,
                open_receipt: receipt,
            }
        }
        other => panic!("wrong open response: {other:?}"),
    }
}

fn operation(operation_id: &str, tier: SessionStateTierV2) -> PreparedOperationV2 {
    let source = match tier {
        SessionStateTierV2::Stateless => "bash^(\nprintf '2'\n)_bash".to_owned(),
        SessionStateTierV2::CheckpointRestore => {
            "python[7]^(\n__oval_result__ = 1 + 1\n)_python[7]".to_owned()
        }
        other => panic!("test operation does not support {other:?}"),
    };
    PreparedOperationV2::new(
        operation_id,
        TaskAttemptIdV1::new(
            digest(&format!("task:{operation_id}")),
            GenerationV1::new(1).unwrap(),
        ),
        source,
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 20_000,
        4096,
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn submit(
    runtime: &HostedV2RuntimeHandle,
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    opened: &OpenedSession,
    request_id: &str,
    sequence: u64,
    operation: PreparedOperationV2,
    state_quotas: StateQuotaLimitsV2,
) -> HostedResponseV2 {
    let actor_generation = current_actor_generation(runtime, principal, &opened.capability);
    runtime
        .submit_operation(
            principal,
            SubmitOperationRequestV2 {
                credentials: opened.capability.clone().into(),
                client_request_id: request_id.to_owned(),
                client_sequence: sequence,
                placement_lease: lease(
                    signer,
                    principal,
                    opened.state_session.clone(),
                    opened.tier,
                    state_quotas,
                    opened.reservation.clone(),
                    Some(&opened.target),
                    actor_generation.as_ref(),
                    request_id,
                    sequence,
                    PlacementPurposeV2::Execute,
                    Some(operation.sha256().unwrap()),
                    &operation,
                )
                .0,
                operation,
            },
        )
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn submit_with_fresh_placement_until_not_expired(
    runtime: &HostedV2RuntimeHandle,
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    opened: &OpenedSession,
    request_id: &str,
    sequence: u64,
    operation: &PreparedOperationV2,
    state_quotas: &StateQuotaLimitsV2,
    expected_lease_nonce: Option<&str>,
) -> HostedResponseV2 {
    let operation_sha256 = operation.sha256().unwrap();
    for attempt in 1..=MAX_FRESH_PLACEMENT_ATTEMPTS {
        let actor_generation = current_actor_generation(runtime, principal, &opened.capability);
        let placement_lease = lease(
            signer,
            principal,
            opened.state_session.clone(),
            opened.tier,
            state_quotas.clone(),
            opened.reservation.clone(),
            Some(&opened.target),
            actor_generation.as_ref(),
            request_id,
            sequence,
            PlacementPurposeV2::Execute,
            Some(operation_sha256.clone()),
            operation,
        )
        .0;
        if let Some(expected_lease_nonce) = expected_lease_nonce {
            assert_eq!(
                placement_lease.authority.lease_nonce().to_string(),
                expected_lease_nonce,
                "fresh test evidence changed the deterministic lease nonce"
            );
        }
        let freshness_deadline = test_placement_freshness_deadline(&placement_lease);
        let response = runtime.handle_request(
            principal,
            HostedRequestV2::SubmitOperation {
                protocol: HOSTED_PROTOCOL_V2.to_owned(),
                request: SubmitOperationRequestV2 {
                    credentials: opened.capability.clone().into(),
                    client_request_id: request_id.to_owned(),
                    client_sequence: sequence,
                    placement_lease,
                    operation: operation.clone(),
                },
            },
        );
        let response_time = unix_time_ms().unwrap();
        if should_refresh_self_minted_test_placement(
            &response,
            attempt,
            response_time,
            freshness_deadline,
        ) {
            continue;
        }
        return response;
    }
    unreachable!("bounded fresh-placement attempts always return a response")
}

fn test_placement_freshness_deadline(lease: &SignedPlacementLeaseV2) -> u64 {
    let mut deadline = [
        lease.authority.expires_at().get(),
        lease.evidence.node_profile.expires_at().get(),
        lease.evidence.capacity_observation.expires_at().get(),
    ]
    .into_iter()
    .min()
    .unwrap();
    if let Some(state_capacity) = lease.state_capacity_observation.as_ref() {
        deadline = deadline.min(state_capacity.expires_at().get());
    }
    for warrant in &lease.evidence.warrants {
        if let Some(expires_at) = warrant.expires_at() {
            deadline = deadline.min(expires_at.get());
        }
    }
    deadline
}

fn should_refresh_self_minted_test_placement(
    response: &HostedResponseV2,
    attempt: usize,
    response_time: u64,
    freshness_deadline: u64,
) -> bool {
    matches!(
        response,
        HostedResponseV2::Error { error }
            if matches!(error.code.as_str(), "placement-denied" | "placement-expired")
                && response_time >= freshness_deadline
                && attempt < MAX_FRESH_PLACEMENT_ATTEMPTS
    )
}

#[test]
fn self_minted_placement_refresh_is_code_and_deadline_bounded() {
    let response = |code| HostedResponseV2::Error {
        error: HostedProtocolErrorV2::new(code, "test rejection", false),
    };
    assert!(should_refresh_self_minted_test_placement(
        &response("placement-denied"),
        1,
        101,
        100,
    ));
    assert!(!should_refresh_self_minted_test_placement(
        &response("placement-denied"),
        1,
        99,
        100,
    ));
    assert!(should_refresh_self_minted_test_placement(
        &response("placement-expired"),
        1,
        101,
        100,
    ));
    assert!(!should_refresh_self_minted_test_placement(
        &response("placement-expired"),
        1,
        99,
        100,
    ));
    assert!(!should_refresh_self_minted_test_placement(
        &response("placement-denied"),
        MAX_FRESH_PLACEMENT_ATTEMPTS,
        101,
        100,
    ));
    assert!(!should_refresh_self_minted_test_placement(
        &response("quota-exceeded"),
        1,
        101,
        100,
    ));
}

fn current_actor_generation(
    runtime: &HostedV2RuntimeHandle,
    principal: &str,
    capability: &SessionCapabilityV2,
) -> Option<ActorGenerationIdV1> {
    match runtime
        .status(
            principal,
            SessionQueryV2 {
                credentials: capability.clone().into(),
                operation_id: None,
            },
        )
        .unwrap()
    {
        HostedResponseV2::Status { session, .. } => session.actor.actor_generation,
        other => panic!("wrong response while reading actor generation: {other:?}"),
    }
}

fn wait_for_terminal(
    runtime: &HostedV2RuntimeHandle,
    principal: &str,
    capability: &SessionCapabilityV2,
    operation_id: &str,
) -> OperationStatusV2 {
    for _ in 0..400 {
        let response = runtime
            .status(
                principal,
                SessionQueryV2 {
                    credentials: capability.clone().into(),
                    operation_id: Some(operation_id.to_owned()),
                },
            )
            .unwrap();
        let HostedResponseV2::Status { session, .. } = response else {
            panic!("wrong status response")
        };
        let status = session.operations[operation_id].status;
        if matches!(
            status,
            OperationStatusV2::Succeeded | OperationStatusV2::Failed
        ) {
            return status;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("operation did not reach a terminal record")
}

fn wait_for_ambiguous(
    runtime: &HostedV2RuntimeHandle,
    principal: &str,
    capability: &SessionCapabilityV2,
    operation_id: &str,
) -> o_lang::hosted_remote::v2::SessionViewV2 {
    for _ in 0..800 {
        let response = runtime
            .status(
                principal,
                SessionQueryV2 {
                    credentials: capability.clone().into(),
                    operation_id: Some(operation_id.to_owned()),
                },
            )
            .unwrap();
        let HostedResponseV2::Status { session, .. } = response else {
            panic!("wrong status response")
        };
        if session.operations[operation_id].status == OperationStatusV2::Ambiguous {
            return session;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("operation did not reach durable ambiguous state")
}

fn current_session_view(
    runtime: &HostedV2RuntimeHandle,
    principal: &str,
    capability: &SessionCapabilityV2,
) -> o_lang::hosted_remote::v2::SessionViewV2 {
    let HostedResponseV2::Status { session, .. } = runtime
        .status(
            principal,
            SessionQueryV2 {
                credentials: capability.clone().into(),
                operation_id: None,
            },
        )
        .unwrap()
    else {
        panic!("wrong status response")
    };
    session
}

#[test]
fn signed_session_journal_binds_principal_bearer_and_exact_duplicate_sequence() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let node_public_key = node_signer.public_key();
    let runtime = runtime(
        &state_root,
        node_signer,
        &placement_signer,
        state_quotas.clone(),
    );
    let principal = principal_digest('a');
    let opened = open_session(
        &runtime,
        &placement_signer,
        &principal,
        "open-a",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );

    let mut operation = operation("op-a", opened.tier);
    // This test exercises exact durable replay after expiry, not a one-second
    // preparation SLA. Leave enough room for a cold debug actor to prepare;
    // the explicit sleep below still crosses this exact captured deadline.
    operation.deadline_unix_ms = unix_time_ms().unwrap() + 5_000;
    let operation_deadline = operation.deadline_unix_ms;
    let request = SubmitOperationRequestV2 {
        credentials: opened.capability.clone().into(),
        client_request_id: "execute-a".to_owned(),
        client_sequence: 1,
        placement_lease: lease(
            &placement_signer,
            &principal,
            opened.state_session.clone(),
            opened.tier,
            state_quotas,
            opened.reservation.clone(),
            Some(&opened.target),
            None,
            "execute-a",
            1,
            PlacementPurposeV2::Execute,
            Some(operation.sha256().unwrap()),
            &operation,
        )
        .0,
        operation: operation.clone(),
    };
    let first = runtime
        .submit_operation(&principal, request.clone())
        .unwrap();
    validate_hosted_response_v2(
        &HostedRequestV2::SubmitOperation {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request: request.clone(),
        },
        &first,
        &node_public_key,
    )
    .unwrap();
    let mut swapped_request = request.clone();
    swapped_request.client_request_id = "execute-swapped".to_owned();
    assert!(validate_hosted_response_v2(
        &HostedRequestV2::SubmitOperation {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request: swapped_request,
        },
        &first,
        &node_public_key,
    )
    .is_err());
    let now = unix_time_ms().unwrap();
    if now <= operation_deadline {
        thread::sleep(Duration::from_millis(operation_deadline - now + 1));
    }
    let duplicate = runtime.submit_operation(&principal, request).unwrap();
    let (
        HostedResponseV2::Committed { receipt: first },
        HostedResponseV2::Committed { receipt: duplicate },
    ) = (first, duplicate)
    else {
        panic!("wrong commit response")
    };
    assert_eq!(first.entry_sha256, duplicate.entry_sha256);
    first.verify().unwrap();
    wait_for_terminal(&runtime, &principal, &opened.capability, "op-a");

    let wrong_principal = runtime.status(
        &principal_digest('b'),
        SessionQueryV2 {
            credentials: opened.capability.clone().into(),
            operation_id: None,
        },
    );
    assert!(wrong_principal
        .unwrap_err()
        .to_string()
        .contains("different authenticated client"));
    let mut wrong_bearer = opened.capability;
    wrong_bearer.bearer = "00".repeat(32);
    assert!(runtime
        .status(
            &principal,
            SessionQueryV2 {
                credentials: wrong_bearer.into(),
                operation_id: None,
            },
        )
        .unwrap_err()
        .to_string()
        .contains("bearer"));
}

#[test]
fn open_retry_survives_dropped_response_restart_and_rejects_mismatch() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('f');
    let opened = {
        let runtime = runtime(
            &state_root,
            node_signer.clone(),
            &placement_signer,
            state_quotas.clone(),
        );
        // The helper observes the returned value so the test can retain its
        // expected digest, but no acknowledgement is fed back to the runtime:
        // dropping it here models response loss after the first durable frame.
        let opened = open_session_with_reservation_and_validity(
            &runtime,
            &placement_signer,
            &principal,
            "open-dropped-response",
            SessionStateTierV2::Stateless,
            state_quotas.clone(),
            reservation(),
            EXPIRED_RETRY_VALIDITY_MS,
        );
        runtime.shutdown().unwrap();
        opened
    };

    // Expire the original 4-second capacity observation. Exact duplicate Open
    // must be answered from the durable first record before reauthorization.
    thread::sleep(Duration::from_millis(4_100));
    let runtime = runtime(
        &state_root,
        node_signer,
        &placement_signer,
        state_quotas.clone(),
    );
    let mut signature_mutation = opened.open_request.clone();
    signature_mutation.placement_lease.signature = "00".repeat(64);
    let conflict = runtime
        .open_session(&principal, signature_mutation)
        .unwrap_err();
    assert!(
        format!("{conflict:#}").contains("already opened by different"),
        "mutated envelope signature was accepted as an exact retry: {conflict:#}"
    );
    let retried = runtime
        .open_session(&principal, opened.open_request.clone())
        .unwrap();
    let HostedResponseV2::SessionOpened {
        capability,
        receipt,
    } = retried
    else {
        panic!("exact retry returned the wrong response")
    };
    assert_eq!(capability, opened.capability);
    assert_eq!(receipt.entry_sha256, opened.open_receipt.entry_sha256);
    let journal = DurableSessionStoreV2::open(
        directory.path().join("inspection-state"),
        HostedNodeSignerV2::from_secret_bytes([99; 32]),
    )
    .unwrap();
    drop(journal);
    let session_journal = runtime
        .status(
            &principal,
            SessionQueryV2 {
                credentials: opened.capability.clone().into(),
                operation_id: None,
            },
        )
        .unwrap();
    let HostedResponseV2::Status { session, .. } = session_journal else {
        panic!("retry status returned wrong response")
    };
    assert_eq!(
        session.journal_head_sha256,
        opened.open_receipt.entry_sha256
    );

    let mismatch_id = "open-dropped-mismatch";
    let mismatch_capability = open_capability(&opened.state_session, mismatch_id);
    let mismatch_lease = lease(
        &placement_signer,
        &principal,
        opened.state_session.clone(),
        opened.tier,
        state_quotas,
        opened.reservation.clone(),
        None,
        None,
        mismatch_id,
        0,
        PlacementPurposeV2::OpenSession,
        None,
        &operation("open-mismatch-proof", SessionStateTierV2::Stateless),
    )
    .0;
    let mismatch = HostedRequestV2::OpenSession {
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        request: OpenSessionRequestV2 {
            client_request_id: mismatch_id.to_owned(),
            state_tier: opened.tier,
            capability_commitment: open_capability_commitment_v2(&mismatch_capability).unwrap(),
            proposed_capability: mismatch_capability,
            placement_lease: mismatch_lease,
        },
    };
    let HostedResponseV2::Error { error } = runtime.handle_request(&principal, mismatch) else {
        panic!("mismatched retry unexpectedly opened the existing state session")
    };
    assert_eq!(error.code, "open-retry-conflict");
    assert!(!error.retryable);
}

#[test]
fn open_response_validation_rejects_swapped_receipt_and_mutated_capability() {
    let directory = tempfile::tempdir().unwrap();
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let node_public_key = node_signer.public_key();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let runtime = runtime(
        &directory.path().join("state"),
        node_signer,
        &placement_signer,
        state_quotas.clone(),
    );
    let principal = principal_digest('7');
    let first = open_session(
        &runtime,
        &placement_signer,
        &principal,
        "open-validation-a",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );
    let second = open_session(
        &runtime,
        &placement_signer,
        &principal,
        "open-validation-b",
        SessionStateTierV2::Stateless,
        state_quotas,
    );
    let request = HostedRequestV2::OpenSession {
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        request: first.open_request.clone(),
    };
    let response = HostedResponseV2::SessionOpened {
        capability: first.capability.clone(),
        receipt: first.open_receipt.clone(),
    };
    validate_hosted_response_v2(&request, &response, &node_public_key).unwrap();
    let swapped = HostedResponseV2::SessionOpened {
        capability: second.capability,
        receipt: second.open_receipt,
    };
    assert!(validate_hosted_response_v2(&request, &swapped, &node_public_key).is_err());
    let mut mutated = response;
    let HostedResponseV2::SessionOpened { capability, .. } = &mut mutated else {
        unreachable!()
    };
    capability.bearer = "00".repeat(32);
    assert!(validate_hosted_response_v2(&request, &mutated, &node_public_key).is_err());
}

#[test]
fn stateful_actor_is_node_established_once_then_exactly_pinned() {
    let directory = tempfile::tempdir().unwrap();
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let node_public_key = node_signer.public_key();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('8');
    let runtime = runtime(
        &directory.path().join("state"),
        node_signer.clone(),
        &placement_signer,
        state_quotas.clone(),
    );
    let opened = open_session(
        &runtime,
        &placement_signer,
        &principal,
        "open-actor-establishment",
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
    );

    let forged_operation = operation("forged-first-actor", opened.tier);
    let forged_bindings = prepare_bindings(&forged_operation);
    let forged_actor = actor_for(&forged_bindings, &opened.target, 1);
    let forged_error = runtime
        .submit_operation(
            &principal,
            SubmitOperationRequestV2 {
                credentials: opened.capability.clone().into(),
                client_request_id: "forged-first-actor".to_owned(),
                client_sequence: 1,
                placement_lease: lease(
                    &placement_signer,
                    &principal,
                    opened.state_session.clone(),
                    opened.tier,
                    state_quotas.clone(),
                    opened.reservation.clone(),
                    Some(&opened.target),
                    Some(&forged_actor),
                    "forged-first-actor",
                    1,
                    PlacementPurposeV2::Execute,
                    Some(forged_operation.sha256().unwrap()),
                    &forged_operation,
                )
                .0,
                operation: forged_operation,
            },
        )
        .unwrap_err();
    assert!(
        format!("{forged_error:#}")
            .contains("first stateful Execute must let the node establish actor generation"),
        "{forged_error:#}"
    );
    assert!(current_actor_generation(&runtime, &principal, &opened.capability).is_none());

    let establish_operation = operation("establish-first-actor", opened.tier);
    let establish_request = SubmitOperationRequestV2 {
        credentials: opened.capability.clone().into(),
        client_request_id: "establish-first-actor".to_owned(),
        client_sequence: 1,
        placement_lease: lease(
            &placement_signer,
            &principal,
            opened.state_session.clone(),
            opened.tier,
            state_quotas.clone(),
            opened.reservation.clone(),
            Some(&opened.target),
            None,
            "establish-first-actor",
            1,
            PlacementPurposeV2::Execute,
            Some(establish_operation.sha256().unwrap()),
            &establish_operation,
        )
        .0,
        operation: establish_operation,
    };
    let accepted = runtime
        .submit_operation(&principal, establish_request.clone())
        .unwrap();
    let HostedResponseV2::Committed { receipt } = &accepted else {
        panic!("first stateful Execute returned the wrong response")
    };
    let validation_request = HostedRequestV2::SubmitOperation {
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        request: establish_request,
    };
    validate_hosted_response_v2(&validation_request, &accepted, &node_public_key).unwrap();
    let established_actor = match &receipt.entry.event {
        o_lang::hosted_remote::v2::JournalEventV2::OperationAccepted {
            actor_generation: Some(actor),
            ..
        } => actor.clone(),
        _ => panic!("first stateful Execute receipt omitted the established actor"),
    };
    assert!(matches!(
        &receipt.entry.event,
        o_lang::hosted_remote::v2::JournalEventV2::OperationAccepted {
            actor_generation: Some(_),
            ..
        }
    ));
    let swapped_target_actor = ActorGenerationIdV1::new(
        established_actor.logical_environment().clone(),
        established_actor.backend_implementation().clone(),
        digest("swapped-receipt-target"),
        established_actor.sandbox_policy().clone(),
        established_actor.launch_context().clone(),
        established_actor.generation(),
    );
    let swapped_target = HostedResponseV2::Committed {
        receipt: resign_operation_actor(&node_signer, receipt, swapped_target_actor),
    };
    assert!(
        validate_hosted_response_v2(&validation_request, &swapped_target, &node_public_key)
            .unwrap_err()
            .to_string()
            .contains("signed target descriptor")
    );
    let forged_backend_actor = ActorGenerationIdV1::new(
        established_actor.logical_environment().clone(),
        digest("forged-receipt-backend"),
        established_actor.target_descriptor().clone(),
        established_actor.sandbox_policy().clone(),
        established_actor.launch_context().clone(),
        established_actor.generation(),
    );
    let forged_backend = HostedResponseV2::Committed {
        receipt: resign_operation_actor(&node_signer, receipt, forged_backend_actor),
    };
    assert!(
        validate_hosted_response_v2(&validation_request, &forged_backend, &node_public_key)
            .unwrap_err()
            .to_string()
            .contains("signed backend implementation")
    );
    let forged_environment_actor = ActorGenerationIdV1::new(
        digest("forged-receipt-environment"),
        established_actor.backend_implementation().clone(),
        established_actor.target_descriptor().clone(),
        established_actor.sandbox_policy().clone(),
        established_actor.launch_context().clone(),
        established_actor.generation(),
    );
    let forged_environment = HostedResponseV2::Committed {
        receipt: resign_operation_actor(&node_signer, receipt, forged_environment_actor),
    };
    assert!(validate_hosted_response_v2(
        &validation_request,
        &forged_environment,
        &node_public_key
    )
    .unwrap_err()
    .to_string()
    .contains("signed logical environment"));
    wait_for_terminal(
        &runtime,
        &principal,
        &opened.capability,
        "establish-first-actor",
    );
    let current = current_actor_generation(&runtime, &principal, &opened.capability)
        .expect("first stateful Execute must establish the node actor");

    let omitted_operation = operation("omit-established-actor", opened.tier);
    let omitted_error = runtime
        .submit_operation(
            &principal,
            SubmitOperationRequestV2 {
                credentials: opened.capability.clone().into(),
                client_request_id: "omit-established-actor".to_owned(),
                client_sequence: 2,
                placement_lease: lease(
                    &placement_signer,
                    &principal,
                    opened.state_session.clone(),
                    opened.tier,
                    state_quotas.clone(),
                    opened.reservation.clone(),
                    Some(&opened.target),
                    None,
                    "omit-established-actor",
                    2,
                    PlacementPurposeV2::Execute,
                    Some(omitted_operation.sha256().unwrap()),
                    &omitted_operation,
                )
                .0,
                operation: omitted_operation,
            },
        )
        .unwrap_err();
    assert!(
        format!("{omitted_error:#}").contains("omitted the established actor generation"),
        "{omitted_error:#}"
    );

    let mismatched_operation = operation("mismatched-established-actor", opened.tier);
    let mismatched_actor = ActorGenerationIdV1::new(
        current.logical_environment().clone(),
        current.backend_implementation().clone(),
        current.target_descriptor().clone(),
        current.sandbox_policy().clone(),
        current.launch_context().clone(),
        GenerationV1::new(current.generation().get() + 1).unwrap(),
    );
    let mismatched_error = runtime
        .submit_operation(
            &principal,
            SubmitOperationRequestV2 {
                credentials: opened.capability.clone().into(),
                client_request_id: "mismatched-established-actor".to_owned(),
                client_sequence: 2,
                placement_lease: lease(
                    &placement_signer,
                    &principal,
                    opened.state_session.clone(),
                    opened.tier,
                    state_quotas,
                    opened.reservation.clone(),
                    Some(&opened.target),
                    Some(&mismatched_actor),
                    "mismatched-established-actor",
                    2,
                    PlacementPurposeV2::Execute,
                    Some(mismatched_operation.sha256().unwrap()),
                    &mismatched_operation,
                )
                .0,
                operation: mismatched_operation,
            },
        )
        .unwrap_err();
    assert!(
        format!("{mismatched_error:#}").contains("binds a different actor generation"),
        "{mismatched_error:#}"
    );
}

#[test]
fn checkpoint_tier_reuses_identical_snapshot_and_restores_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let principal = principal_digest('c');
    let state_quotas = quotas(8);
    let checkpointed;
    {
        let runtime = runtime(
            &state_root,
            node_signer.clone(),
            &placement_signer,
            state_quotas.clone(),
        );
        checkpointed = open_session(
            &runtime,
            &placement_signer,
            &principal,
            "open-checkpoint",
            SessionStateTierV2::CheckpointRestore,
            state_quotas.clone(),
        );
        submit(
            &runtime,
            &placement_signer,
            &principal,
            &checkpointed,
            "execute-checkpoint",
            1,
            operation("op-checkpoint", checkpointed.tier),
            state_quotas.clone(),
        );
        wait_for_terminal(
            &runtime,
            &principal,
            &checkpointed.capability,
            "op-checkpoint",
        );
        let first_checkpoint = match runtime
            .status(
                &principal,
                SessionQueryV2 {
                    credentials: checkpointed.capability.clone().into(),
                    operation_id: None,
                },
            )
            .unwrap()
        {
            HostedResponseV2::Status { session, .. } => session
                .actor
                .checkpoint_sha256
                .expect("first operation must publish a checkpoint"),
            other => panic!("wrong status response: {other:?}"),
        };
        submit(
            &runtime,
            &placement_signer,
            &principal,
            &checkpointed,
            "execute-checkpoint-unchanged",
            2,
            operation("op-checkpoint-unchanged", checkpointed.tier),
            state_quotas.clone(),
        );
        wait_for_terminal(
            &runtime,
            &principal,
            &checkpointed.capability,
            "op-checkpoint-unchanged",
        );
        let second_checkpoint = match runtime
            .status(
                &principal,
                SessionQueryV2 {
                    credentials: checkpointed.capability.clone().into(),
                    operation_id: None,
                },
            )
            .unwrap()
        {
            HostedResponseV2::Status { session, .. } => session
                .actor
                .checkpoint_sha256
                .expect("second operation must retain a checkpoint"),
            other => panic!("wrong status response: {other:?}"),
        };
        assert_eq!(
            first_checkpoint, second_checkpoint,
            "unchanged actor state must reuse its content-addressed snapshot"
        );
        runtime.shutdown().unwrap();
    }
    let restarted = runtime(&state_root, node_signer, &placement_signer, state_quotas);
    let status = restarted
        .status(
            &principal,
            SessionQueryV2 {
                credentials: checkpointed.capability.into(),
                operation_id: None,
            },
        )
        .unwrap();
    let HostedResponseV2::Status { session, .. } = status else {
        panic!("wrong status response")
    };
    assert_eq!(
        session.status,
        o_lang::hosted_remote::v2::SessionStatusV2::RecoveryRequired
    );
    assert!(session.actor.actor_id.is_none());
    assert!(session.actor.checkpoint_sha256.is_some());
}

#[test]
fn stateful_infrastructure_timeout_preserves_checkpoint_and_requires_recovery_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let principal = principal_digest('f');
    let state_quotas = quotas(8);
    let opened;
    let prior_checkpoint;
    {
        let runtime = runtime(
            &state_root,
            node_signer.clone(),
            &placement_signer,
            state_quotas.clone(),
        );
        opened = open_session(
            &runtime,
            &placement_signer,
            &principal,
            "open-timeout",
            SessionStateTierV2::CheckpointRestore,
            state_quotas.clone(),
        );
        submit(
            &runtime,
            &placement_signer,
            &principal,
            &opened,
            "execute-before-timeout",
            1,
            operation("op-before-timeout", opened.tier),
            state_quotas.clone(),
        );
        wait_for_terminal(
            &runtime,
            &principal,
            &opened.capability,
            "op-before-timeout",
        );
        prior_checkpoint = match runtime
            .status(
                &principal,
                SessionQueryV2 {
                    credentials: opened.capability.clone().into(),
                    operation_id: None,
                },
            )
            .unwrap()
        {
            HostedResponseV2::Status { session, .. } => session
                .actor
                .checkpoint_sha256
                .expect("successful stateful command must checkpoint"),
            other => panic!("wrong status response: {other:?}"),
        };

        let timed = PreparedOperationV2::new(
            "op-timeout",
            TaskAttemptIdV1::new(digest("task:op-timeout"), GenerationV1::new(1).unwrap()),
            "python[7]^(\nimport time\ntime.sleep(30)\n__oval_result__ = 9\n)_python[7]",
            BackendRegistry::global().catalog_sha256(),
            unix_time_ms().unwrap() + 2_000,
            4096,
        )
        .unwrap();
        submit(
            &runtime,
            &placement_signer,
            &principal,
            &opened,
            "execute-timeout",
            2,
            timed,
            state_quotas.clone(),
        );
        let ambiguous = wait_for_ambiguous(&runtime, &principal, &opened.capability, "op-timeout");
        assert_eq!(
            ambiguous.status,
            o_lang::hosted_remote::v2::SessionStatusV2::RecoveryRequired
        );
        assert_eq!(
            ambiguous.actor.checkpoint_sha256.as_deref(),
            Some(prior_checkpoint.as_str()),
            "in-flight failure must not replace the last good checkpoint"
        );
    }

    {
        let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
        let journal = store.read_journal(&opened.capability.session_id).unwrap();
        assert!(journal.corruption.is_none());
        assert!(journal.entries.iter().any(|entry| matches!(
            entry.entry.event,
            o_lang::hosted_remote::v2::JournalEventV2::ActorStateLost { .. }
        )));
        assert!(journal.entries.iter().any(|entry| matches!(
            entry.entry.event,
            o_lang::hosted_remote::v2::JournalEventV2::OperationInterrupted {
                classification: OperationStatusV2::Ambiguous,
                ..
            }
        )));
    }

    let restarted = runtime(&state_root, node_signer, &placement_signer, state_quotas);
    let after_restart = match restarted
        .status(
            &principal,
            SessionQueryV2 {
                credentials: opened.capability.into(),
                operation_id: None,
            },
        )
        .unwrap()
    {
        HostedResponseV2::Status { session, .. } => session,
        other => panic!("wrong status response: {other:?}"),
    };
    assert_eq!(
        after_restart.status,
        o_lang::hosted_remote::v2::SessionStatusV2::RecoveryRequired
    );
    assert_eq!(
        after_restart.actor.checkpoint_sha256.as_deref(),
        Some(prior_checkpoint.as_str())
    );
}

#[test]
fn hard_quota_refusal_consumes_nonce_without_eviction_and_gc_is_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(1);
    let running = runtime(
        &state_root,
        node_signer.clone(),
        &placement_signer,
        state_quotas.clone(),
    );
    let principal = principal_digest('d');
    let first = open_session(
        &running,
        &placement_signer,
        &principal,
        "open-first",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );
    let second_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest("session:open-second"),
    )
    .unwrap();
    let second_operation = operation("open-second-proof", SessionStateTierV2::Stateless);
    let second_capability = open_capability(&second_session, "open-second");
    let second_lease = lease(
        &placement_signer,
        &principal,
        second_session.clone(),
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
        reservation(),
        None,
        None,
        "open-second",
        0,
        PlacementPurposeV2::OpenSession,
        None,
        &second_operation,
    )
    .0;
    let second_request = OpenSessionRequestV2 {
        client_request_id: "open-second".to_owned(),
        state_tier: SessionStateTierV2::Stateless,
        capability_commitment: open_capability_commitment_v2(&second_capability).unwrap(),
        proposed_capability: second_capability.clone(),
        placement_lease: second_lease,
    };
    let second = running.open_session(&principal, second_request.clone());
    assert!(second
        .unwrap_err()
        .to_string()
        .contains("no session was evicted"));
    running.shutdown().unwrap();
    let runtime = runtime(
        &state_root,
        node_signer.clone(),
        &placement_signer,
        state_quotas.clone(),
    );
    // Reissue the same deterministic nonce under a fresh capacity observation
    // after restart. This tests durable nonce consumption without depending on
    // a 5-second protocol freshness window surviving a loaded CI host.
    let retry_operation = operation("open-second-retry-proof", SessionStateTierV2::Stateless);
    let retry_request = OpenSessionRequestV2 {
        client_request_id: "open-second".to_owned(),
        state_tier: SessionStateTierV2::Stateless,
        capability_commitment: open_capability_commitment_v2(&second_capability).unwrap(),
        proposed_capability: second_capability,
        placement_lease: lease(
            &placement_signer,
            &principal,
            second_session,
            SessionStateTierV2::Stateless,
            state_quotas.clone(),
            reservation(),
            None,
            None,
            "open-second",
            0,
            PlacementPurposeV2::OpenSession,
            None,
            &retry_operation,
        )
        .0,
    };
    assert!(runtime
        .open_session(&principal, retry_request)
        .unwrap_err()
        .to_string()
        .contains("already consumed"));

    runtime
        .close_session(
            &principal,
            SessionMutationRequestV2 {
                credentials: first.capability.clone().into(),
                client_request_id: "close-first".to_owned(),
                client_sequence: 1,
            },
        )
        .unwrap();
    let session_id = first.capability.session_id;
    runtime.shutdown().unwrap();
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let authorization = store.authorize_closed_session_gc(&session_id).unwrap();
    assert!(matches!(
        authorization.entry.event,
        o_lang::hosted_remote::v2::JournalEventV2::ClosedSessionGcAuthorized { .. }
    ));
    drop(store);
    // Simulate a process dying after signed authorization. Reopening resumes
    // archive retention and removal in the store's ordered completion phase.
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let gc = store.gc_closed_session(&session_id).unwrap();
    gc.verify().unwrap();
    assert!(matches!(
        gc.entry.event,
        o_lang::hosted_remote::v2::JournalEventV2::ClosedSessionGcCompleted { .. }
    ));
}

#[test]
fn durable_capacity_refusal_preserves_reserved_close_headroom() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('6');
    let runtime = runtime(
        &state_root,
        node_signer.clone(),
        &placement_signer,
        state_quotas.clone(),
    );
    let small_reservation = StateReservationV2::new(1, 64 * 1024, 256 * 1024).unwrap();
    let reservation_bytes = small_reservation.state_bytes();
    let opened = open_session_with_reservation(
        &runtime,
        &placement_signer,
        &principal,
        "open-close-headroom",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
        small_reservation,
    );

    // The output reservation alone exceeds the entire per-session durable
    // reservation. A tiny source therefore reaches the hard quota in one
    // admission without making proof freshness depend on repeated parsing or
    // backend execution under a loaded CI runner.
    let operation_id = "fill-once";
    let prepared = PreparedOperationV2::new(
        operation_id,
        TaskAttemptIdV1::new(digest("task:fill-once"), GenerationV1::new(1).unwrap()),
        "bash^(\nprintf '2'\n)_bash",
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 60_000,
        MAX_HOSTED_OUTPUT_BYTES as u64,
    )
    .unwrap();
    assert!(prepared.output_limit_bytes > reservation_bytes);
    let response = submit_with_fresh_placement_until_not_expired(
        &runtime,
        &placement_signer,
        &principal,
        &opened,
        "execute-fill-once",
        1,
        &prepared,
        &state_quotas,
        None,
    );
    let HostedResponseV2::Error { error } = response else {
        panic!("one-shot capacity fixture returned the wrong response: {response:?}")
    };
    assert_eq!(error.code, "quota-exceeded", "{error:?}");
    let refused = current_session_view(&runtime, &principal, &opened.capability);
    assert_eq!(refused.next_client_sequence, 1);
    assert!(refused.operations.is_empty());

    let session_id = opened.capability.session_id.clone();
    runtime
        .close_session(
            &principal,
            SessionMutationRequestV2 {
                credentials: opened.capability.into(),
                client_request_id: "close-after-fill".to_owned(),
                client_sequence: 1,
            },
        )
        .expect("reserved control headroom must keep Close durable");
    runtime.shutdown().unwrap();
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    assert!(
        store.session_durable_bytes(&session_id).unwrap() <= reservation_bytes,
        "durable Close must remain inside the exact session reservation"
    );
}

#[test]
fn expired_self_minted_execute_placement_does_not_consume_sequence_or_lease_nonce() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('a');
    let runtime = runtime(
        &state_root,
        node_signer,
        &placement_signer,
        state_quotas.clone(),
    );
    let opened = open_session(
        &runtime,
        &placement_signer,
        &principal,
        "open-expired-execute",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );
    let prepared = operation("expired-execute", opened.tier);
    let request_id = "execute-expired";
    let expired_lease = lease_with_validity(
        &placement_signer,
        &principal,
        opened.state_session.clone(),
        opened.tier,
        state_quotas.clone(),
        opened.reservation.clone(),
        Some(&opened.target),
        None,
        request_id,
        1,
        PlacementPurposeV2::Execute,
        Some(prepared.sha256().unwrap()),
        &prepared,
        None,
        1,
    )
    .0;
    let expired_nonce = expired_lease.authority.lease_nonce().to_string();
    let expired_deadline = test_placement_freshness_deadline(&expired_lease);
    thread::sleep(Duration::from_millis(5));

    let expired = runtime.handle_request(
        &principal,
        HostedRequestV2::SubmitOperation {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request: SubmitOperationRequestV2 {
                credentials: opened.capability.clone().into(),
                client_request_id: request_id.to_owned(),
                client_sequence: 1,
                placement_lease: expired_lease,
                operation: prepared.clone(),
            },
        },
    );
    let HostedResponseV2::Error { error } = expired else {
        panic!("expired placement was not rejected: {expired:?}")
    };
    assert_eq!(error.code, "placement-denied");
    assert!(!error.retryable);
    assert!(unix_time_ms().unwrap() >= expired_deadline);
    let unchanged = current_session_view(&runtime, &principal, &opened.capability);
    assert_eq!(unchanged.next_client_sequence, 1);
    assert!(unchanged.operations.is_empty());

    let accepted = submit_with_fresh_placement_until_not_expired(
        &runtime,
        &placement_signer,
        &principal,
        &opened,
        request_id,
        1,
        &prepared,
        &state_quotas,
        Some(&expired_nonce),
    );
    assert!(matches!(accepted, HostedResponseV2::Committed { .. }));
    wait_for_terminal(
        &runtime,
        &principal,
        &opened.capability,
        &prepared.operation_id,
    );
    runtime.shutdown().unwrap();
}

#[test]
fn signed_archival_v1_placement_admission_is_rejected_before_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('8');
    let runtime = runtime(
        &state_root,
        node_signer,
        &placement_signer,
        state_quotas.clone(),
    );
    let opened = open_session(
        &runtime,
        &placement_signer,
        &principal,
        "open-archival-placement",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );
    let prepared = operation("archival-placement", opened.tier);
    let request_id = "execute-archival-placement";
    let archival_v1 = SemanticDigestV1::hash_bytes(
        "ostadix/placement-admission/v1",
        b"archival-v1-authority-cannot-be-uplifted",
    );
    let signed = lease_with_validity(
        &placement_signer,
        &principal,
        opened.state_session.clone(),
        opened.tier,
        state_quotas,
        opened.reservation.clone(),
        Some(&opened.target),
        None,
        request_id,
        1,
        PlacementPurposeV2::Execute,
        Some(prepared.sha256().unwrap()),
        &prepared,
        Some(archival_v1),
        TEST_EVIDENCE_VALIDITY_MS,
    )
    .0;

    let response = runtime.handle_request(
        &principal,
        HostedRequestV2::SubmitOperation {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request: SubmitOperationRequestV2 {
                credentials: opened.capability.clone().into(),
                client_request_id: request_id.to_owned(),
                client_sequence: 1,
                placement_lease: signed,
                operation: prepared,
            },
        },
    );
    let HostedResponseV2::Error { error } = response else {
        panic!("archival V1 placement admission reached dispatch: {response:?}")
    };
    assert_eq!(error.code, "placement-denied");
    assert!(
        error
            .message
            .contains("canonical execution placement lease validation failed"),
        "{}",
        error.message
    );
    let unchanged = current_session_view(&runtime, &principal, &opened.capability);
    assert_eq!(unchanged.next_client_sequence, 1);
    assert!(unchanged.operations.is_empty());
    runtime.shutdown().unwrap();
}

#[test]
fn near_quota_retry_admits_preexisting_exact_operation_blob_at_zero_new_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('9');
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let running = OwnedRuntimeV2::from_owner(
        HostedV2RuntimeOwner::open(
            HostedV2RuntimeConfig {
                node_id: NODE_ID.to_owned(),
                node_generation: GenerationV1::new(1).unwrap(),
                shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
                runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
                state_quota_generation: GenerationV1::new(1).unwrap(),
                state_quotas: state_quotas.clone(),
            },
            store.clone(),
            Arc::new(PinnedEd25519PlacementAuthorizerV2::new(
                placement_signer.public_key(),
            )),
        )
        .unwrap(),
    );
    let tight_reservation = StateReservationV2::new(1, 0, 208 * 1024).unwrap();
    let opened = open_session_with_reservation(
        &running,
        &placement_signer,
        &principal,
        "open-prepublished-operation",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
        tight_reservation,
    );
    let prepared = PreparedOperationV2::new(
        "prepublished-operation",
        TaskAttemptIdV1::new(
            digest("task:prepublished-operation"),
            GenerationV1::new(1).unwrap(),
        ),
        format!("bash^(\nprintf '2'\n#{}\n)_bash", "x".repeat(8 * 1024)),
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 60_000,
        4096,
    )
    .unwrap();
    let full_blob_bytes = store
        .operation_new_bytes(&opened.capability.session_id, &prepared)
        .unwrap();
    assert!(full_blob_bytes > 6 * 1024);
    let placement_lease = lease(
        &placement_signer,
        &principal,
        opened.state_session.clone(),
        opened.tier,
        state_quotas.clone(),
        opened.reservation.clone(),
        Some(&opened.target),
        None,
        "execute-prepublished-operation",
        1,
        PlacementPurposeV2::Execute,
        Some(prepared.sha256().unwrap()),
        &prepared,
    )
    .0;
    let request = SubmitOperationRequestV2 {
        credentials: opened.capability.clone().into(),
        client_request_id: "execute-prepublished-operation".to_owned(),
        client_sequence: 1,
        operation: prepared.clone(),
        placement_lease,
    };
    let operation_sha256 = prepared.sha256().unwrap();
    let accepted_preview = store
        .signer()
        .issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: opened.capability.session_id.clone(),
            sequence: opened.open_receipt.entry.sequence + 1,
            previous_entry_sha256: Some(opened.open_receipt.entry_sha256.clone()),
            recorded_unix_ms: unix_time_ms().unwrap(),
            event: JournalEventV2::OperationAccepted {
                client_sequence: 1,
                client_request_id: request.client_request_id.clone(),
                request_sha256: canonical_hosted_sha256(&request).unwrap(),
                operation_id: prepared.operation_id.clone(),
                task_attempt: prepared.task_attempt.clone(),
                operation_sha256,
                source_sha256: prepared.source_sha256.clone(),
                actor_id: None,
                actor_generation: None,
                placement_lease_sha256: request
                    .placement_lease
                    .authority
                    .semantic_digest()
                    .unwrap()
                    .to_string(),
                placement_lease_nonce: request.placement_lease.authority.lease_nonce().to_string(),
            },
        })
        .unwrap();
    let current = store
        .session_durable_bytes(&opened.capability.session_id)
        .unwrap();
    let exact_retry_projection = current
        + full_blob_bytes
        + store.encoded_frame_bytes(&accepted_preview).unwrap()
        + prepared.output_limit_bytes
        + 3 * 64 * 1024;
    assert!(
        exact_retry_projection <= opened.reservation.state_bytes(),
        "fixture must fit the already durable blob exactly once: projected={exact_retry_projection} reservation={}",
        opened.reservation.state_bytes()
    );
    let duplicate_blob_projection = exact_retry_projection + full_blob_bytes;
    assert!(
        duplicate_blob_projection > opened.reservation.state_bytes(),
        "fixture must reject charging the same immutable blob twice: projected={duplicate_blob_projection} reservation={}",
        opened.reservation.state_bytes()
    );
    assert_eq!(
        store
            .write_operation(&opened.capability.session_id, &prepared)
            .unwrap(),
        full_blob_bytes
    );
    assert_eq!(
        store
            .operation_new_bytes(&opened.capability.session_id, &prepared)
            .unwrap(),
        0,
        "an exact crash-gap retry must be durable-byte neutral"
    );

    drop(running);
    drop(store);
    let restarted = runtime(&state_root, node_signer, &placement_signer, state_quotas);
    let accepted = restarted.submit_operation(&principal, request).unwrap();
    assert!(matches!(accepted, HostedResponseV2::Committed { .. }));
    wait_for_terminal(
        &restarted,
        &principal,
        &opened.capability,
        "prepublished-operation",
    );
}

#[test]
fn accepted_execute_nonce_cannot_authorize_another_session_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('7');
    let (first, second);
    {
        let runtime = runtime(
            &state_root,
            node_signer.clone(),
            &placement_signer,
            state_quotas.clone(),
        );
        first = open_session(
            &runtime,
            &placement_signer,
            &principal,
            "open-nonce-a",
            SessionStateTierV2::Stateless,
            state_quotas.clone(),
        );
        second = open_session(
            &runtime,
            &placement_signer,
            &principal,
            "open-nonce-b",
            SessionStateTierV2::Stateless,
            state_quotas.clone(),
        );
        submit(
            &runtime,
            &placement_signer,
            &principal,
            &first,
            "shared-execute-nonce",
            1,
            operation("nonce-op-a", first.tier),
            state_quotas.clone(),
        );
        wait_for_terminal(&runtime, &principal, &first.capability, "nonce-op-a");
        runtime.shutdown().unwrap();
    }

    let restarted = runtime(
        &state_root,
        node_signer,
        &placement_signer,
        state_quotas.clone(),
    );
    let operation = operation("nonce-op-b", second.tier);
    let error = restarted
        .submit_operation(
            &principal,
            SubmitOperationRequestV2 {
                credentials: second.capability.into(),
                client_request_id: "shared-execute-nonce".to_owned(),
                client_sequence: 1,
                placement_lease: lease(
                    &placement_signer,
                    &principal,
                    second.state_session,
                    second.tier,
                    state_quotas,
                    second.reservation,
                    Some(&second.target),
                    None,
                    "shared-execute-nonce",
                    1,
                    PlacementPurposeV2::Execute,
                    Some(operation.sha256().unwrap()),
                    &operation,
                )
                .0,
                operation,
            },
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("already consumed"),
        "{error:#}"
    );
}

#[test]
fn unconfigured_authority_denies_before_state_creation() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let denied = OwnedRuntimeV2::from_owner(
        HostedV2RuntimeOwner::open(
            HostedV2RuntimeConfig {
                node_id: NODE_ID.to_owned(),
                node_generation: GenerationV1::new(1).unwrap(),
                shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
                runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
                state_quota_generation: GenerationV1::new(1).unwrap(),
                state_quotas: state_quotas.clone(),
            },
            DurableSessionStoreV2::open(&state_root, node_signer).unwrap(),
            Arc::new(DenyAllPlacementAuthorizerV2),
        )
        .unwrap(),
    );
    let principal = principal_digest('e');
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest("session:denied"),
    )
    .unwrap();
    let capability = open_capability(&state_session, "open-denied");
    let capability_commitment = open_capability_commitment_v2(&capability).unwrap();
    let error = denied
        .open_session(
            &principal,
            OpenSessionRequestV2 {
                client_request_id: "open-denied".to_owned(),
                state_tier: SessionStateTierV2::Stateless,
                proposed_capability: capability,
                capability_commitment,
                placement_lease: lease(
                    &placement_signer,
                    &principal,
                    state_session,
                    SessionStateTierV2::Stateless,
                    state_quotas,
                    reservation(),
                    None,
                    None,
                    "open-denied",
                    0,
                    PlacementPurposeV2::OpenSession,
                    None,
                    &operation("denied-proof", SessionStateTierV2::Stateless),
                )
                .0,
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("no authenticated"));
}

#[test]
fn explicit_shutdown_drains_terminal_and_checkpoint_workers_then_reopens_immediately() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('b');
    let running = runtime(
        &state_root,
        node_signer.clone(),
        &placement_signer,
        state_quotas.clone(),
    );
    let handle = running.handle();

    let first = open_session(
        &running,
        &placement_signer,
        &principal,
        "shutdown-open-a",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );
    let checkpoint = open_session(
        &running,
        &placement_signer,
        &principal,
        "shutdown-open-checkpoint",
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
    );
    let second = open_session(
        &running,
        &placement_signer,
        &principal,
        "shutdown-open-b",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );

    submit(
        &running,
        &placement_signer,
        &principal,
        &first,
        "shutdown-execute-a",
        1,
        operation("shutdown-op-a", first.tier),
        state_quotas.clone(),
    );
    submit(
        &running,
        &placement_signer,
        &principal,
        &checkpoint,
        "shutdown-execute-checkpoint",
        1,
        operation("shutdown-op-checkpoint", checkpoint.tier),
        state_quotas.clone(),
    );
    running
        .inject_actor_close_before_execute_for_test(&second.capability.session_id)
        .unwrap();
    submit(
        &running,
        &placement_signer,
        &principal,
        &second,
        "shutdown-execute-b",
        1,
        operation("shutdown-op-b", second.tier),
        state_quotas.clone(),
    );

    // This is the only settlement barrier: there is no status polling, sleep,
    // or debug acknowledgement. Close is FIFO behind every accepted Execute.
    running.shutdown().unwrap();

    let closed = handle
        .status(
            &principal,
            SessionQueryV2 {
                credentials: first.capability.clone().into(),
                operation_id: None,
            },
        )
        .unwrap_err();
    assert!(closed.downcast_ref::<HostedV2RuntimeClosedV2>().is_some());
    assert!(handle
        .node_id()
        .unwrap_err()
        .downcast_ref::<HostedV2RuntimeClosedV2>()
        .is_some());
    assert!(handle
        .state_quotas()
        .unwrap_err()
        .downcast_ref::<HostedV2RuntimeClosedV2>()
        .is_some());
    let HostedResponseV2::Error { error } = handle.handle_request(
        &principal,
        HostedRequestV2::Status {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            query: SessionQueryV2 {
                credentials: first.capability.clone().into(),
                operation_id: None,
            },
        },
    ) else {
        panic!("closed runtime accepted a hosted request")
    };
    assert_eq!(error.code, "runtime-closed");
    assert!(!error.retryable);

    // The request handle remains alive, so this immediate successful open
    // proves owner shutdown removed the runtime-owned store/root-lock reference.
    let restarted = runtime(&state_root, node_signer, &placement_signer, state_quotas);
    let first_view = current_session_view(&restarted, &principal, &first.capability);
    let second_view = current_session_view(&restarted, &principal, &second.capability);
    let checkpoint_view = current_session_view(&restarted, &principal, &checkpoint.capability);
    assert!(matches!(
        first_view.operations["shutdown-op-a"].status,
        OperationStatusV2::Succeeded | OperationStatusV2::Failed
    ));
    assert_eq!(
        second_view.operations["shutdown-op-b"].status,
        OperationStatusV2::NotStarted
    );
    assert!(matches!(
        checkpoint_view.operations["shutdown-op-checkpoint"].status,
        OperationStatusV2::Succeeded | OperationStatusV2::Failed
    ));
    assert!(checkpoint_view.actor.checkpoint_sha256.is_some());
    assert!(checkpoint_view.actor.checkpoint_bytes.is_some());
    restarted.shutdown().unwrap();
}

#[test]
fn owner_drop_closes_surviving_handle_and_releases_root_lock() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let owner = runtime(
        &state_root,
        node_signer.clone(),
        &placement_signer,
        state_quotas.clone(),
    );
    let handle = owner.handle();

    drop(owner);

    let closed = handle.node_id().unwrap_err();
    assert!(closed.downcast_ref::<HostedV2RuntimeClosedV2>().is_some());
    let reopened = runtime(&state_root, node_signer, &placement_signer, state_quotas);
    reopened.shutdown().unwrap();
}

#[test]
fn worker_panic_shutdown_is_typed_idempotent_and_releases_root_lock() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8);
    let principal = principal_digest('c');
    let running = runtime(
        &state_root,
        node_signer.clone(),
        &placement_signer,
        state_quotas.clone(),
    );
    let handle = running.handle();
    let opened = open_session(
        &running,
        &placement_signer,
        &principal,
        "shutdown-panic-open",
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
    );
    running
        .inject_worker_panic_for_test(&opened.capability.session_id)
        .unwrap();

    let first = running.shutdown().unwrap_err();
    let first = first
        .downcast_ref::<HostedV2RuntimeShutdownErrorV2>()
        .expect("worker panic did not produce the typed shutdown error");
    assert!(first.message().contains(&opened.capability.session_id));
    let repeated = running.shutdown().unwrap_err();
    let repeated = repeated
        .downcast_ref::<HostedV2RuntimeShutdownErrorV2>()
        .expect("repeated shutdown did not preserve the typed outcome");
    assert_eq!(repeated, first);

    let closed = handle.unreadable_sessions().unwrap_err();
    assert!(closed.downcast_ref::<HostedV2RuntimeClosedV2>().is_some());
    let reopened = runtime(&state_root, node_signer, &placement_signer, state_quotas);
    let view = current_session_view(&reopened, &principal, &opened.capability);
    assert_eq!(view.operations.len(), 0);
    reopened.shutdown().unwrap();
}
