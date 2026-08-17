use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt};
use std::{fs::OpenOptions, io::Write};

use o_lang::backend::state::{
    sandbox_policy_sha256, BackendCheckpointV1, BackendStateTierV1, EvaluatorActorCheckpointV1,
    EvaluatorStateSnapshotV1,
};
use o_lang::backend_state as canonical_backend_state;
use o_lang::eval::{Evaluator, PlacementFragmentBindingsV2};
use o_lang::hosted_remote::v2::{
    build_local_dev_placement_proof_v2, open_capability_commitment_v2, ActorHealthV2,
    AuthorizedPlacementV2, DenyAllPlacementAuthorizerV2, DurableSessionStoreV2,
    HostedCommandBindingV2, HostedNodeSignerV2, HostedPlacementAuthorityV2,
    HostedPlacementIdentityV2, HostedResponseV2, HostedV2Runtime, HostedV2RuntimeConfig,
    JournalEntryV2, JournalEventV2, LocalDevPlacementConfigV2, OpenSessionRequestV2,
    OperationFailureStageV2, OperationOutcomeV2, OperationStatusV2,
    PinnedEd25519PlacementAuthorizerV2, PlacementAuthorizationContextV2, PlacementLeaseSignerV2,
    PlacementProofAuthorizerV2, PlacementPurposeV2, PreparedOperationV2, RecoverSessionRequestV2,
    RecoveryTriggerV2, RecoveryWarrantV2, ReplayClassV2, SessionCapabilityV2,
    SessionMutationRequestV2, SessionQueryV2, SessionStateTierV2, SessionStatusV2, SessionViewV2,
    SignedJournalEntryV2, SignedPlacementLeaseV2, SubmitOperationRequestV2,
    HOSTED_COMMAND_BINDING_SCHEMA_V2, HOSTED_JOURNAL_ENTRY_SCHEMA_V2, HOSTED_PROTOCOL_V2,
    HOSTED_RECOVERY_WARRANT_SCHEMA_V2,
};
use o_lang::hosted_remote::{canonical_hosted_sha256, unix_time_ms};
use o_lang::ir::BackendRegistry;
use o_lang::placement::{
    ActorGenerationIdV1, CanonicalPlacementRecordV1, GenerationV1, LeaseExpectationV2,
    LeaseStateBindingV2, PlacementLeaseV2, PlacementReservationV1, SemanticDigestV1,
    StateCapacityObservationV2, StateControlExpectationV2, StateControlLeaseV2, StateQuotaLimitsV2,
    StateReservationV2, StateSessionIdV2, TargetDescriptorV1, TaskAttemptIdV1, UnixMillisV1,
};
use o_lang::value::OValue;
use serde_json::json;
use sha2::{Digest, Sha256};

const NODE_ID: &str = "node-v2-recovery-test";
const CLOSE_HEADROOM: u64 = 64 * 1024;
const ACTOR_FENCE_HEADROOM: u64 = 64 * 1024;
const AUTHORITY_CONTROL_HEADROOM: u64 = 16 * 1024;

#[test]
fn backend_state_root_and_legacy_paths_share_one_type_identity() {
    let canonical = canonical_backend_state::BackendStateTierV1::SemanticSnapshot;
    let legacy: BackendStateTierV1 = canonical;
    let canonical_again: canonical_backend_state::BackendStateTierV1 = legacy;

    assert_eq!(
        canonical_again,
        canonical_backend_state::BackendStateTierV1::SemanticSnapshot
    );
}

fn digest(label: &str) -> SemanticDigestV1 {
    SemanticDigestV1::hash_bytes("ostadix/hosted-v2-recovery-test/v1", label.as_bytes())
}

fn principal_digest(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}

fn successor_actor(previous: &ActorGenerationIdV1) -> ActorGenerationIdV1 {
    ActorGenerationIdV1::new(
        previous.logical_environment().clone(),
        previous.backend_implementation().clone(),
        previous.target_descriptor().clone(),
        previous.sandbox_policy().clone(),
        previous.launch_context().clone(),
        GenerationV1::new(previous.generation().get() + 1).unwrap(),
    )
}

fn quotas(max_state_bytes_per_session: u64) -> StateQuotaLimitsV2 {
    quotas_with_sessions(16, max_state_bytes_per_session)
}

fn quotas_with_sessions(
    max_open_sessions: u32,
    max_state_bytes_per_session: u64,
) -> StateQuotaLimitsV2 {
    StateQuotaLimitsV2::new(
        max_open_sessions,
        1,
        max_state_bytes_per_session.min(4 * 1024 * 1024),
        max_state_bytes_per_session,
        64 * 1024 * 1024,
    )
    .unwrap()
}

fn runtime(store: DurableSessionStoreV2, state_quotas: StateQuotaLimitsV2) -> HostedV2Runtime {
    HostedV2Runtime::open(
        HostedV2RuntimeConfig {
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quotas,
        },
        store,
        Arc::new(DenyAllPlacementAuthorizerV2),
    )
    .unwrap()
}

fn authorized_runtime(
    store: DurableSessionStoreV2,
    state_quotas: StateQuotaLimitsV2,
    placement_signer: &PlacementLeaseSignerV2,
) -> HostedV2Runtime {
    HostedV2Runtime::open(
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
    .unwrap()
}

struct BlockingPlacementAuthorizer {
    inner: PinnedEd25519PlacementAuthorizerV2,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    block_call: usize,
    calls: AtomicUsize,
}

impl PlacementProofAuthorizerV2 for BlockingPlacementAuthorizer {
    fn authorize(
        &self,
        context: &PlacementAuthorizationContextV2,
        lease: &SignedPlacementLeaseV2,
    ) -> anyhow::Result<AuthorizedPlacementV2> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == self.block_call {
            self.entered.wait();
            self.release.wait();
        }
        self.inner.authorize(context, lease)
    }
}

fn prepared_open_operation(request_id: &str) -> PreparedOperationV2 {
    PreparedOperationV2::new(
        format!("proof-{request_id}"),
        TaskAttemptIdV1::new(
            digest(&format!("task:{request_id}")),
            GenerationV1::new(1).unwrap(),
        ),
        "bash^(\nprintf '2'\n)_bash",
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 20_000,
        4096,
    )
    .unwrap()
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

#[allow(clippy::too_many_arguments)]
fn existing_or_open_lease(
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
    operation: &PreparedOperationV2,
    recovery_warrant_sha256: Option<String>,
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
        operation_sha256: (purpose == PlacementPurposeV2::Execute)
            .then(|| operation.sha256().unwrap()),
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
                NODE_ID,
                GenerationV1::new(1).unwrap(),
                GenerationV1::new(1).unwrap(),
                state_quotas,
                0,
                0,
                UnixMillisV1::new(now.saturating_sub(1)),
                UnixMillisV1::new(now + 4_000),
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
        HostedPlacementAuthorityV2::Execution(
            PlacementLeaseV2::new(
                signer.issuer_key(),
                digest(&format!("nonce:{request_id}")),
                LeaseExpectationV2::new(
                    NODE_ID,
                    target_digest,
                    evidence.node_profile.profile_generation(),
                    evidence.capacity_observation.capacity_generation(),
                    capacity_digest,
                    eligibility_digest,
                    bindings.operation_oir().clone(),
                    footprint_digest,
                    discharge_digest,
                    bindings.placement_admission().clone(),
                    bindings.task_attempt().clone(),
                    bindings.backend_implementation_sha256().clone(),
                    bindings.realization_pipeline().clone(),
                    trust_digest,
                    evidence.reservation.clone(),
                    command_digest,
                    state_binding,
                )
                .unwrap(),
                UnixMillisV1::new(now.saturating_sub(1)),
                UnixMillisV1::new(now + 20_000),
            )
            .unwrap(),
        )
    } else {
        HostedPlacementAuthorityV2::StateControl(
            StateControlLeaseV2::new(
                signer.issuer_key(),
                digest(&format!("nonce:{request_id}")),
                StateControlExpectationV2::new(
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
                .unwrap(),
                UnixMillisV1::new(now.saturating_sub(1)),
                UnixMillisV1::new(now + 20_000),
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

fn open_capability(state_session: &StateSessionIdV2, request_id: &str) -> SessionCapabilityV2 {
    SessionCapabilityV2 {
        session_id: state_session.semantic_digest().unwrap().to_string(),
        bearer: digest(&format!("bearer:{request_id}")).to_string(),
    }
}

fn signed_open_request(
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    request_id: &str,
    state_quotas: StateQuotaLimitsV2,
    reservation: StateReservationV2,
) -> OpenSessionRequestV2 {
    signed_open_request_with_lifetime(
        signer,
        principal,
        request_id,
        state_quotas,
        reservation,
        4_000,
    )
}

fn signed_open_request_with_lifetime(
    signer: &PlacementLeaseSignerV2,
    principal: &str,
    request_id: &str,
    state_quotas: StateQuotaLimitsV2,
    reservation: StateReservationV2,
    evidence_lifetime_ms: u64,
) -> OpenSessionRequestV2 {
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest(&format!("open-session:{request_id}")),
    )
    .unwrap();
    let capability = open_capability(&state_session, request_id);
    let operation = prepared_open_operation(request_id);
    let bindings = prepare_bindings(&operation);
    let now = unix_time_ms().unwrap();
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
        None,
        None,
        true,
    )
    .unwrap();
    let command = HostedCommandBindingV2 {
        schema: HOSTED_COMMAND_BINDING_SCHEMA_V2.to_owned(),
        protocol: HOSTED_PROTOCOL_V2.to_owned(),
        node_id: NODE_ID.to_owned(),
        principal_sha256: principal.to_owned(),
        state_session: state_session.clone(),
        session_state_tier: SessionStateTierV2::Stateless,
        client_request_id: request_id.to_owned(),
        client_sequence: 0,
        purpose: PlacementPurposeV2::OpenSession,
        operation_sha256: None,
        recovery_warrant_sha256: None,
        open_capability_commitment: Some(open_capability_commitment_v2(&capability).unwrap()),
        state_quota_generation: GenerationV1::new(1).unwrap(),
        state_quota_limits: state_quotas.clone(),
        state_reservation: reservation.clone(),
        actor_generation: None,
    };
    let observation = StateCapacityObservationV2::new(
        signer.issuer_key(),
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        GenerationV1::new(1).unwrap(),
        state_quotas,
        0,
        0,
        UnixMillisV1::new(now.saturating_sub(1)),
        UnixMillisV1::new(now + evidence_lifetime_ms),
    )
    .unwrap();
    let evidence = provisional.evidence;
    let expectation = StateControlExpectationV2::new(
        NODE_ID,
        evidence.node_profile.descriptor_digest().unwrap(),
        evidence.node_profile.profile_generation(),
        evidence.capacity_observation.capacity_generation(),
        evidence.capacity_observation.semantic_digest().unwrap(),
        provisional.eligibility.semantic_digest().unwrap(),
        evidence.requirement_footprint.semantic_digest().unwrap(),
        evidence.warrant_discharge.semantic_digest().unwrap(),
        bindings.backend_implementation_sha256().clone(),
        bindings.realization_pipeline().clone(),
        evidence.trust_policy.semantic_digest().unwrap(),
        evidence.reservation.clone(),
        command.semantic_digest().unwrap(),
        LeaseStateBindingV2::open(observation.semantic_digest().unwrap(), reservation.clone()),
    )
    .unwrap();
    let authority = HostedPlacementAuthorityV2::StateControl(
        StateControlLeaseV2::new(
            signer.issuer_key(),
            digest(&format!("nonce:{request_id}")),
            expectation,
            UnixMillisV1::new(now.saturating_sub(1)),
            UnixMillisV1::new(now + 20_000),
        )
        .unwrap(),
    );
    let placement_lease: SignedPlacementLeaseV2 = signer
        .sign(authority, command, evidence, Some(observation))
        .unwrap();
    OpenSessionRequestV2 {
        client_request_id: request_id.to_owned(),
        state_tier: SessionStateTierV2::Stateless,
        proposed_capability: capability.clone(),
        capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
        placement_lease,
    }
}

fn bearer_hash(salt: &[u8; 32], bearer: &[u8; 32]) -> String {
    let mut bytes = Vec::from(b"OSTADIX/HOSTED-SESSION-BEARER/V2\0".as_slice());
    bytes.extend_from_slice(salt);
    bytes.extend_from_slice(bearer);
    hex::encode(Sha256::digest(bytes))
}

fn sign_event(
    signer: &HostedNodeSignerV2,
    session_id: &str,
    previous: Option<&SignedJournalEntryV2>,
    event: JournalEventV2,
) -> SignedJournalEntryV2 {
    signer
        .issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: session_id.to_owned(),
            sequence: previous.map_or(1, |entry| entry.entry.sequence + 1),
            previous_entry_sha256: previous.map(|entry| entry.entry_sha256.clone()),
            recorded_unix_ms: unix_time_ms().unwrap(),
            event,
        })
        .unwrap()
}

fn append_event(
    store: &DurableSessionStoreV2,
    signer: &HostedNodeSignerV2,
    session_id: &str,
    previous: &SignedJournalEntryV2,
    event: JournalEventV2,
) -> SignedJournalEntryV2 {
    let entry = sign_event(signer, session_id, Some(previous), event);
    store.append_entry(session_id, &entry).unwrap();
    entry
}

#[derive(Clone)]
struct InstalledSession {
    capability: SessionCapabilityV2,
    actor_generation: ActorGenerationIdV1,
    head: SignedJournalEntryV2,
}

fn install_open_session(
    store: &DurableSessionStoreV2,
    signer: &HostedNodeSignerV2,
    label: &str,
    tier: SessionStateTierV2,
    reservation: StateReservationV2,
) -> InstalledSession {
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest(&format!("session:{label}")),
    )
    .unwrap();
    let session_id = state_session.semantic_digest().unwrap().to_string();
    let bearer = [0x31_u8; 32];
    let capability = SessionCapabilityV2 {
        session_id: session_id.clone(),
        bearer: hex::encode(bearer),
    };
    let salt = [0x72_u8; 32];
    let placement_identity = HostedPlacementIdentityV2 {
        target_descriptor: digest(&format!("target:{label}")),
        requirement_footprint: digest(&format!("footprint:{label}")),
        backend_implementation: digest(&format!("implementation:{label}")),
        realization_pipeline: digest(&format!("pipeline:{label}")),
        trust_policy: digest(&format!("trust:{label}")),
        reservation: PlacementReservationV1::new(1, 1024 * 1024, 0).unwrap(),
    };
    let actor_generation = ActorGenerationIdV1::new(
        digest(&format!("environment:{label}")),
        placement_identity.backend_implementation.clone(),
        placement_identity.target_descriptor.clone(),
        SemanticDigestV1::from_sha256(sandbox_policy_sha256(&[]).unwrap()).unwrap(),
        digest(&format!("launch:{label}")),
        GenerationV1::new(1).unwrap(),
    );
    let head = sign_event(
        signer,
        &session_id,
        None,
        JournalEventV2::SessionOpened {
            request_sha256: "f0".repeat(32),
            principal_sha256: principal_digest('a'),
            bearer_salt: hex::encode(salt),
            bearer_hash: bearer_hash(&salt, &bearer),
            capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
            state_tier: tier,
            state_session,
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quota_limits: quotas(reservation.state_bytes()),
            state_reservation: reservation.clone(),
            placement_identity: placement_identity.clone(),
            placement_lease_sha256: digest(&format!("open-lease:{label}")).to_string(),
            placement_lease_nonce: digest(&format!("open-nonce:{label}")).to_string(),
            client_request_id: format!("open-{label}"),
        },
    );
    store.install_session(&session_id, &head).unwrap();
    InstalledSession {
        capability,
        actor_generation,
        head,
    }
}

fn install_started_actor_operation(
    store: &DurableSessionStoreV2,
    signer: &HostedNodeSignerV2,
    session: &mut InstalledSession,
    label: &str,
) -> String {
    let prepared = PreparedOperationV2::new(
        format!("operation-{label}"),
        TaskAttemptIdV1::new(
            digest(&format!("task:{label}")),
            GenerationV1::new(1).unwrap(),
        ),
        format!("python[7]^(\nvalue = {label:?}\nvalue\n)_python[7]"),
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 60_000,
        4096,
    )
    .unwrap();
    let operation_sha256 = prepared.sha256().unwrap();
    store
        .write_operation(&session.capability.session_id, &prepared)
        .unwrap();
    session.head = append_event(
        store,
        signer,
        &session.capability.session_id,
        &session.head,
        JournalEventV2::OperationAccepted {
            client_sequence: 1,
            client_request_id: format!("execute-{label}"),
            request_sha256: digest(&format!("request:{label}")).to_string(),
            operation_id: prepared.operation_id.clone(),
            task_attempt: prepared.task_attempt.clone(),
            operation_sha256: operation_sha256.clone(),
            source_sha256: prepared.source_sha256.clone(),
            actor_id: Some(format!("actor-{label}")),
            actor_generation: Some(session.actor_generation.clone()),
            placement_lease_sha256: digest(&format!("execute-lease:{label}")).to_string(),
            placement_lease_nonce: digest(&format!("execute-nonce:{label}")).to_string(),
        },
    );
    session.head = append_event(
        store,
        signer,
        &session.capability.session_id,
        &session.head,
        JournalEventV2::OperationStarted {
            operation_id: prepared.operation_id,
            operation_sha256: operation_sha256.clone(),
            actor_generation: Some(session.actor_generation.clone()),
            started_unix_ms: unix_time_ms().unwrap(),
        },
    );
    operation_sha256
}

fn install_operation_terminal(
    store: &DurableSessionStoreV2,
    signer: &HostedNodeSignerV2,
    session: &mut InstalledSession,
    label: &str,
    operation_sha256: String,
    terminal_message: String,
) {
    install_operation_terminal_with_state(
        store,
        signer,
        session,
        label,
        operation_sha256,
        terminal_message,
        true,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn install_operation_terminal_with_state(
    store: &DurableSessionStoreV2,
    signer: &HostedNodeSignerV2,
    session: &mut InstalledSession,
    label: &str,
    operation_sha256: String,
    terminal_message: String,
    state_durable: bool,
    actor_state_touched: bool,
) {
    session.head = append_event(
        store,
        signer,
        &session.capability.session_id,
        &session.head,
        JournalEventV2::OperationTerminal {
            operation_id: format!("operation-{label}"),
            operation_sha256,
            finished_unix_ms: unix_time_ms().unwrap(),
            outcome: OperationOutcomeV2::Failed {
                stage: OperationFailureStageV2::Evaluate,
                code: "fixture-terminal".to_owned(),
                message: terminal_message,
            },
            state_durable,
            actor_state_touched,
        },
    );
}

fn install_completed_actor_operation(
    store: &DurableSessionStoreV2,
    signer: &HostedNodeSignerV2,
    session: &mut InstalledSession,
    label: &str,
    terminal_message: String,
) {
    let operation_sha256 = install_started_actor_operation(store, signer, session, label);
    install_operation_terminal(
        store,
        signer,
        session,
        label,
        operation_sha256,
        terminal_message,
    );
}

fn session_status(runtime: &HostedV2Runtime, capability: &SessionCapabilityV2) -> HostedResponseV2 {
    runtime
        .status(
            &principal_digest('a'),
            SessionQueryV2 {
                credentials: capability.clone().into(),
                operation_id: None,
            },
        )
        .unwrap()
}

fn checkpoint_snapshot(
    backend: &str,
    codec: &str,
    actor_generation: &ActorGenerationIdV1,
    launch_generation: &str,
) -> EvaluatorStateSnapshotV1 {
    let checkpoint = BackendCheckpointV1::new(
        backend,
        BackendStateTierV1::SemanticSnapshot,
        codec,
        digest(&format!("runtime:{backend}")).to_string(),
        json!({"fixture": backend}),
        Vec::new(),
    )
    .unwrap();
    let actor =
        EvaluatorActorCheckpointV1::new(backend, 7, Vec::new(), launch_generation, checkpoint)
            .unwrap();
    assert_eq!(
        actor.sandbox_policy_sha256,
        actor_generation.sandbox_policy().as_sha256()
    );
    EvaluatorStateSnapshotV1::new(vec![actor]).unwrap()
}

fn install_checkpoint(
    store: &DurableSessionStoreV2,
    signer: &HostedNodeSignerV2,
    session: &mut InstalledSession,
    snapshot: &EvaluatorStateSnapshotV1,
    max_snapshot_payload_bytes: u64,
) {
    let snapshot_sha256 = snapshot.snapshot_sha256().unwrap();
    let snapshot_bytes = snapshot.encoded_len().unwrap() as u64;
    store
        .write_checkpoint(
            &session.capability.session_id,
            &snapshot_sha256,
            snapshot,
            max_snapshot_payload_bytes,
        )
        .unwrap();
    session.head = append_event(
        store,
        signer,
        &session.capability.session_id,
        &session.head,
        JournalEventV2::ActorCheckpointed {
            actor_generation: session.actor_generation.clone(),
            snapshot_sha256,
            snapshot_bytes,
        },
    );
}

fn python_operation(label: &str, body: &str) -> PreparedOperationV2 {
    PreparedOperationV2::new(
        format!("python-{label}"),
        TaskAttemptIdV1::new(
            digest(&format!("python-task:{label}")),
            GenerationV1::new(1).unwrap(),
        ),
        format!("python[7]^({body})_python[7]"),
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 60_000,
        4096,
    )
    .unwrap()
}

fn wait_for_operation(
    runtime: &HostedV2Runtime,
    capability: &SessionCapabilityV2,
    operation_id: &str,
) -> (SessionViewV2, SignedJournalEntryV2) {
    for _ in 0..200 {
        let HostedResponseV2::Status {
            session,
            head_receipt,
        } = runtime
            .status(
                &principal_digest('a'),
                SessionQueryV2 {
                    credentials: capability.clone().into(),
                    operation_id: None,
                },
            )
            .unwrap()
        else {
            panic!("wrong status response")
        };
        if session
            .operations
            .get(operation_id)
            .is_some_and(|operation| {
                matches!(
                    operation.status,
                    OperationStatusV2::Succeeded | OperationStatusV2::Failed
                )
            })
        {
            return (session, head_receipt);
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("operation {operation_id} did not settle")
}

struct DirectRecoveryFixture {
    runtime: HostedV2Runtime,
    store: DurableSessionStoreV2,
    placement_signer: PlacementLeaseSignerV2,
    principal: String,
    capability: SessionCapabilityV2,
    state_session: StateSessionIdV2,
    state_quotas: StateQuotaLimitsV2,
    reservation: StateReservationV2,
    target: TargetDescriptorV1,
    previous_actor: ActorGenerationIdV1,
    warrant: RecoveryWarrantV2,
    ambiguous_operation_id: String,
    ambiguous_operation_sha256: String,
    proof_operation: PreparedOperationV2,
    checkpoint_sha256: String,
}

fn direct_recovery_fixture(
    state_root: &Path,
    corrupt_backend_checkpoint: bool,
) -> DirectRecoveryFixture {
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('a');
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest(if corrupt_backend_checkpoint {
            "direct-recovery-failure-session"
        } else {
            "direct-recovery-success-session"
        }),
    )
    .unwrap();
    let open_request_id = if corrupt_backend_checkpoint {
        "direct-recovery-failure-open"
    } else {
        "direct-recovery-success-open"
    };
    let capability = open_capability(&state_session, open_request_id);
    let seed_operation = python_operation(
        if corrupt_backend_checkpoint {
            "failure-seed"
        } else {
            "success-seed"
        },
        "x = 42\nx",
    );
    let (open_lease, target) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation.clone(),
        None,
        None,
        open_request_id,
        0,
        PlacementPurposeV2::OpenSession,
        &seed_operation,
        None,
    );
    let store = DurableSessionStoreV2::open(state_root, node_signer.clone()).unwrap();
    let running = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    assert!(matches!(
        running
            .open_session(
                &principal,
                OpenSessionRequestV2 {
                    client_request_id: open_request_id.to_owned(),
                    state_tier: SessionStateTierV2::CheckpointRestore,
                    proposed_capability: capability.clone(),
                    capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
                    placement_lease: open_lease,
                },
            )
            .unwrap(),
        HostedResponseV2::SessionOpened { .. }
    ));

    let execute_request_id = if corrupt_backend_checkpoint {
        "direct-recovery-failure-seed-execute"
    } else {
        "direct-recovery-success-seed-execute"
    };
    let (execute_lease, _) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation.clone(),
        Some(&target),
        None,
        execute_request_id,
        1,
        PlacementPurposeV2::Execute,
        &seed_operation,
        None,
    );
    running
        .submit_operation(
            &principal,
            SubmitOperationRequestV2 {
                credentials: capability.clone().into(),
                client_request_id: execute_request_id.to_owned(),
                client_sequence: 1,
                operation: seed_operation.clone(),
                placement_lease: execute_lease,
            },
        )
        .unwrap();
    let (settled, mut head) =
        wait_for_operation(&running, &capability, &seed_operation.operation_id);
    let previous_actor = settled.actor.actor_generation.clone().unwrap();
    let actor_id = settled.actor.actor_id.clone().unwrap();
    let mut checkpoint_sha256 = settled.actor.checkpoint_sha256.clone().unwrap();
    let checkpoint_bytes = settled.actor.checkpoint_bytes.unwrap();
    running.shutdown().unwrap();
    drop(running);

    if corrupt_backend_checkpoint {
        let snapshot = store
            .read_checkpoint(&capability.session_id, &checkpoint_sha256, checkpoint_bytes)
            .unwrap();
        let mut actors = snapshot.actors;
        let actor = actors.first_mut().unwrap();
        let original = actor.checkpoint.clone();
        let mut payload = original.payload;
        payload
            .as_object_mut()
            .unwrap()
            .insert("ambient_sha256".to_owned(), json!("00".repeat(32)));
        actor.checkpoint = BackendCheckpointV1::new(
            original.backend,
            original.tier,
            original.codec,
            original.runtime_binding_sha256,
            payload,
            original.external_resources,
        )
        .unwrap();
        actor.runtime_binding_sha256 = actor.checkpoint.runtime_binding_sha256.clone();
        let snapshot = EvaluatorStateSnapshotV1::new(actors).unwrap();
        checkpoint_sha256 = snapshot.snapshot_sha256().unwrap();
        let snapshot_bytes = snapshot.encoded_len().unwrap() as u64;
        store
            .write_checkpoint(
                &capability.session_id,
                &checkpoint_sha256,
                &snapshot,
                reservation.snapshot_bytes_per_actor(),
            )
            .unwrap();
        head = append_event(
            &store,
            &node_signer,
            &capability.session_id,
            &head,
            JournalEventV2::ActorCheckpointed {
                actor_generation: previous_actor.clone(),
                snapshot_sha256: checkpoint_sha256.clone(),
                snapshot_bytes,
            },
        );
    }

    let ambiguous_operation = python_operation(
        if corrupt_backend_checkpoint {
            "failure-ambiguous"
        } else {
            "success-ambiguous"
        },
        "x += 100\nx",
    );
    let ambiguous_sha256 = ambiguous_operation.sha256().unwrap();
    store
        .write_operation(&capability.session_id, &ambiguous_operation)
        .unwrap();
    head = append_event(
        &store,
        &node_signer,
        &capability.session_id,
        &head,
        JournalEventV2::OperationAccepted {
            client_sequence: 2,
            client_request_id: format!("{}-accept", ambiguous_operation.operation_id),
            request_sha256: digest(&format!("{}-request", ambiguous_operation.operation_id))
                .to_string(),
            operation_id: ambiguous_operation.operation_id.clone(),
            task_attempt: ambiguous_operation.task_attempt.clone(),
            operation_sha256: ambiguous_sha256.clone(),
            source_sha256: ambiguous_operation.source_sha256.clone(),
            actor_id: Some(actor_id),
            actor_generation: Some(previous_actor.clone()),
            placement_lease_sha256: digest(&format!("{}-lease", ambiguous_operation.operation_id))
                .to_string(),
            placement_lease_nonce: digest(&format!("{}-nonce", ambiguous_operation.operation_id))
                .to_string(),
        },
    );
    append_event(
        &store,
        &node_signer,
        &capability.session_id,
        &head,
        JournalEventV2::OperationStarted {
            operation_id: ambiguous_operation.operation_id.clone(),
            operation_sha256: ambiguous_sha256.clone(),
            actor_generation: Some(previous_actor.clone()),
            started_unix_ms: unix_time_ms().unwrap(),
        },
    );

    let runtime = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    let HostedResponseV2::Status { session, .. } = session_status(&runtime, &capability) else {
        panic!("wrong status response")
    };
    assert_eq!(session.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(session.next_client_sequence, 3);
    assert_eq!(
        session.operations[&ambiguous_operation.operation_id].status,
        OperationStatusV2::Ambiguous
    );
    assert_eq!(
        session.actor.checkpoint_sha256.as_deref(),
        Some(checkpoint_sha256.as_str())
    );

    let warrant = RecoveryWarrantV2 {
        schema: HOSTED_RECOVERY_WARRANT_SCHEMA_V2.to_owned(),
        warrant_id: format!("warrant-{}", ambiguous_operation.operation_id),
        session_id: capability.session_id.clone(),
        trigger: RecoveryTriggerV2::AmbiguousOperation {
            operation_id: ambiguous_operation.operation_id.clone(),
            operation_sha256: ambiguous_sha256.clone(),
            replay_class: ReplayClassV2::Pure,
            stable_publication_id: None,
        },
        evidence_sha256: session.journal_head_sha256.clone(),
    };
    let proof_operation = python_operation(
        if corrupt_backend_checkpoint {
            "failure-recovery-proof"
        } else {
            "success-recovery-proof"
        },
        "x",
    );

    DirectRecoveryFixture {
        runtime,
        store,
        placement_signer,
        principal,
        capability,
        state_session,
        state_quotas,
        reservation,
        target,
        previous_actor,
        warrant,
        ambiguous_operation_id: ambiguous_operation.operation_id,
        ambiguous_operation_sha256: ambiguous_sha256,
        proof_operation,
        checkpoint_sha256,
    }
}

fn recovery_request(fixture: &DirectRecoveryFixture, request_id: &str) -> RecoverSessionRequestV2 {
    let (placement_lease, _) = existing_or_open_lease(
        &fixture.placement_signer,
        &fixture.principal,
        fixture.state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        fixture.state_quotas.clone(),
        fixture.reservation.clone(),
        Some(&fixture.target),
        Some(&fixture.previous_actor),
        request_id,
        3,
        PlacementPurposeV2::Recover,
        &fixture.proof_operation,
        Some(fixture.warrant.sha256().unwrap()),
    );
    RecoverSessionRequestV2 {
        credentials: fixture.capability.clone().into(),
        client_request_id: request_id.to_owned(),
        client_sequence: 3,
        warrant: fixture.warrant.clone(),
        placement_lease,
    }
}

#[test]
fn explicit_recovery_commits_only_after_backend_ack_and_advances_one_generation() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = direct_recovery_fixture(&directory.path().join("state"), false);
    let request = recovery_request(&fixture, "direct-recovery-success");
    let HostedResponseV2::Committed { receipt } = fixture
        .runtime
        .recover_session(&fixture.principal, request)
        .unwrap()
    else {
        panic!("wrong recovery response")
    };
    receipt.verify().unwrap();
    let JournalEventV2::RecoveryCommitted {
        previous_actor_generation,
        actor_generation,
        checkpoint_sha256,
        ..
    } = &receipt.entry.event
    else {
        panic!("successful recovery did not publish RecoveryCommitted")
    };
    assert_eq!(previous_actor_generation, &fixture.previous_actor);
    assert_eq!(
        actor_generation.generation().get(),
        fixture.previous_actor.generation().get() + 1
    );
    assert_eq!(
        checkpoint_sha256.as_deref(),
        Some(fixture.checkpoint_sha256.as_str())
    );

    let HostedResponseV2::Status { session, .. } =
        session_status(&fixture.runtime, &fixture.capability)
    else {
        panic!("wrong status response")
    };
    assert_eq!(session.status, SessionStatusV2::Ready);
    assert_eq!(
        session.actor.actor_generation.as_ref(),
        Some(actor_generation)
    );
    assert_eq!(
        session.actor.next_actor_generation,
        actor_generation.generation()
    );

    let followup = python_operation("post-recovery-read", "x");
    let (lease, _) = existing_or_open_lease(
        &fixture.placement_signer,
        &fixture.principal,
        fixture.state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        fixture.state_quotas.clone(),
        fixture.reservation.clone(),
        Some(&fixture.target),
        Some(actor_generation),
        "post-recovery-read",
        4,
        PlacementPurposeV2::Execute,
        &followup,
        None,
    );
    fixture
        .runtime
        .submit_operation(
            &fixture.principal,
            SubmitOperationRequestV2 {
                credentials: fixture.capability.clone().into(),
                client_request_id: "post-recovery-read".to_owned(),
                client_sequence: 4,
                operation: followup.clone(),
                placement_lease: lease,
            },
        )
        .unwrap();
    let (settled, _) = wait_for_operation(
        &fixture.runtime,
        &fixture.capability,
        &followup.operation_id,
    );
    assert_eq!(
        settled.operations[&followup.operation_id].outcome,
        Some(OperationOutcomeV2::Succeeded {
            value: OValue::int(42),
        }),
        "the internal recovery probe must not alter restored Python state"
    );
    assert_eq!(
        settled.actor.actor_generation.as_ref(),
        Some(actor_generation)
    );
    assert_eq!(
        settled.actor.next_actor_generation,
        actor_generation.generation()
    );

    let journal = fixture
        .store
        .read_journal(&fixture.capability.session_id)
        .unwrap();
    assert_eq!(
        journal
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry.event, JournalEventV2::RecoveryCommitted { .. }))
            .count(),
        1
    );
    assert!(journal
        .entries
        .iter()
        .all(|entry| !matches!(entry.entry.event, JournalEventV2::ActorRestored { .. })));
    let started_generation = journal
        .entries
        .iter()
        .find_map(|entry| match &entry.entry.event {
            JournalEventV2::OperationStarted {
                operation_id,
                actor_generation,
                ..
            } if operation_id == &followup.operation_id => actor_generation.as_ref(),
            _ => None,
        });
    assert_eq!(started_generation, Some(actor_generation));
}

#[test]
fn explicit_recovery_without_backend_ack_is_refused_and_preserves_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = direct_recovery_fixture(&directory.path().join("state"), true);
    let request = recovery_request(&fixture, "direct-recovery-failure");
    let HostedResponseV2::Committed { receipt } = fixture
        .runtime
        .recover_session(&fixture.principal, request.clone())
        .unwrap()
    else {
        panic!("wrong recovery response")
    };
    receipt.verify().unwrap();
    let JournalEventV2::RecoveryRefused { code, message, .. } = &receipt.entry.event else {
        panic!("unacknowledged recovery did not publish RecoveryRefused")
    };
    assert_eq!(code, "state-restore-failed");
    assert!(message.contains("ambient process binding"), "{message}");

    let HostedResponseV2::Status { session, .. } =
        session_status(&fixture.runtime, &fixture.capability)
    else {
        panic!("wrong status response")
    };
    assert_eq!(session.status, SessionStatusV2::RecoveryRequired);
    let first_attempted = session.actor.actor_generation.clone().unwrap();
    assert_eq!(
        first_attempted.generation().get(),
        fixture.previous_actor.generation().get() + 1
    );
    assert_eq!(
        session.actor.next_actor_generation.get(),
        first_attempted.generation().get() + 1
    );
    assert_eq!(
        session.actor.checkpoint_sha256.as_deref(),
        Some(fixture.checkpoint_sha256.as_str())
    );
    assert_eq!(
        session.operations[&fixture.ambiguous_operation_id].status,
        OperationStatusV2::Ambiguous
    );

    let HostedResponseV2::Committed { receipt: duplicate } = fixture
        .runtime
        .recover_session(&fixture.principal, request)
        .unwrap()
    else {
        panic!("wrong duplicate recovery response")
    };
    assert_eq!(duplicate, receipt);

    let retry_warrant = RecoveryWarrantV2 {
        schema: HOSTED_RECOVERY_WARRANT_SCHEMA_V2.to_owned(),
        warrant_id: "direct-recovery-failure-retry-warrant".to_owned(),
        session_id: fixture.capability.session_id.clone(),
        trigger: RecoveryTriggerV2::AmbiguousOperation {
            operation_id: fixture.ambiguous_operation_id.clone(),
            operation_sha256: fixture.ambiguous_operation_sha256.clone(),
            replay_class: ReplayClassV2::Pure,
            stable_publication_id: None,
        },
        evidence_sha256: receipt.entry_sha256.clone(),
    };
    let (retry_lease, _) = existing_or_open_lease(
        &fixture.placement_signer,
        &fixture.principal,
        fixture.state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        fixture.state_quotas.clone(),
        fixture.reservation.clone(),
        Some(&fixture.target),
        Some(&first_attempted),
        "direct-recovery-failure-retry",
        4,
        PlacementPurposeV2::Recover,
        &fixture.proof_operation,
        Some(retry_warrant.sha256().unwrap()),
    );
    let HostedResponseV2::Committed {
        receipt: retry_refusal,
    } = fixture
        .runtime
        .recover_session(
            &fixture.principal,
            RecoverSessionRequestV2 {
                credentials: fixture.capability.clone().into(),
                client_request_id: "direct-recovery-failure-retry".to_owned(),
                client_sequence: 4,
                warrant: retry_warrant,
                placement_lease: retry_lease,
            },
        )
        .unwrap()
    else {
        panic!("wrong retry recovery response")
    };
    let JournalEventV2::RecoveryRefused {
        attempted_actor_generation: Some(second_attempted),
        ..
    } = &retry_refusal.entry.event
    else {
        panic!("retry refusal did not durably consume its spawned actor generation")
    };
    assert_eq!(
        second_attempted.generation().get(),
        first_attempted.generation().get() + 1
    );
    let HostedResponseV2::Status { session, .. } =
        session_status(&fixture.runtime, &fixture.capability)
    else {
        panic!("wrong retry status response")
    };
    assert_eq!(
        session.actor.actor_generation.as_ref(),
        Some(second_attempted)
    );
    assert_eq!(
        session.actor.next_actor_generation.get(),
        second_attempted.generation().get() + 1
    );
    let journal = fixture
        .store
        .read_journal(&fixture.capability.session_id)
        .unwrap();
    assert!(journal.entries.iter().all(|entry| !matches!(
        entry.entry.event,
        JournalEventV2::RecoveryCommitted { .. } | JournalEventV2::ActorRestored { .. }
    )));
    assert_eq!(
        journal
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry.event, JournalEventV2::RecoveryRefused { .. }))
            .count(),
        2
    );
}

#[test]
fn restart_terminates_durable_recovery_attempt_and_never_reuses_its_generation() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = direct_recovery_fixture(&directory.path().join("state"), false);
    let request = recovery_request(&fixture, "recovery-crash-after-attempt-start");
    let journal = fixture
        .store
        .read_journal(&fixture.capability.session_id)
        .unwrap();
    let head = journal.entries.last().unwrap();
    let attempted = successor_actor(&fixture.previous_actor);
    let checkpoint_bytes = match session_status(&fixture.runtime, &fixture.capability) {
        HostedResponseV2::Status { session, .. } => session.actor.checkpoint_bytes.unwrap(),
        _ => panic!("wrong status response"),
    };
    let attempt = fixture
        .store
        .signer()
        .issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_owned(),
            session_id: fixture.capability.session_id.clone(),
            sequence: head.entry.sequence + 1,
            previous_entry_sha256: Some(head.entry_sha256.clone()),
            recorded_unix_ms: unix_time_ms().unwrap(),
            event: JournalEventV2::RecoveryAttemptStarted {
                client_sequence: request.client_sequence,
                client_request_id: request.client_request_id.clone(),
                request_sha256: canonical_hosted_sha256(&request).unwrap(),
                warrant_sha256: request.warrant.sha256().unwrap(),
                placement_lease_sha256: request
                    .placement_lease
                    .authority
                    .semantic_digest()
                    .unwrap()
                    .to_string(),
                placement_lease_nonce: request.placement_lease.authority.lease_nonce().to_string(),
                trigger: request.warrant.trigger.clone(),
                previous_actor_generation: fixture.previous_actor.clone(),
                attempted_actor_generation: attempted.clone(),
                checkpoint_sha256: fixture.checkpoint_sha256.clone(),
                checkpoint_bytes,
            },
        })
        .unwrap();
    fixture
        .store
        .append_entry(&fixture.capability.session_id, &attempt)
        .unwrap();
    let store = fixture.store.clone();
    let quotas = fixture.state_quotas.clone();
    let placement_key = fixture.placement_signer.public_key();
    let principal = fixture.principal.clone();
    let capability = fixture.capability.clone();
    drop(fixture.runtime);

    let restarted = HostedV2Runtime::open(
        HostedV2RuntimeConfig {
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quotas: quotas,
        },
        store,
        Arc::new(PinnedEd25519PlacementAuthorizerV2::new(placement_key)),
    )
    .unwrap();
    let HostedResponseV2::Status { session, .. } = session_status(&restarted, &capability) else {
        panic!("wrong restart status response")
    };
    assert_eq!(session.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(session.next_client_sequence, request.client_sequence + 1);
    assert_eq!(session.actor.actor_generation.as_ref(), Some(&attempted));
    assert_eq!(
        session.actor.next_actor_generation.get(),
        attempted.generation().get() + 1
    );
    let HostedResponseV2::Committed { receipt } =
        restarted.recover_session(&principal, request).unwrap()
    else {
        panic!("exact interrupted-attempt retry returned the wrong response")
    };
    let JournalEventV2::RecoveryRefused {
        recovery_attempt_sha256: Some(attempt_sha256),
        attempted_actor_generation: Some(refused_generation),
        code,
        ..
    } = receipt.entry.event
    else {
        panic!("restart did not publish a terminal recovery refusal")
    };
    assert_eq!(attempt_sha256, attempt.entry_sha256);
    assert_eq!(refused_generation, attempted);
    assert_eq!(code, "recovery-attempt-interrupted");
}

#[cfg(unix)]
#[test]
fn failed_recovery_attempt_append_quarantines_without_exposing_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = direct_recovery_fixture(&directory.path().join("state"), true);
    let request = recovery_request(&fixture, "direct-recovery-refusal-append-failure");
    let journal_path = fixture
        .store
        .root()
        .join("sessions")
        .join(&fixture.capability.session_id)
        .join("journal.cborseq");
    let original_permissions = fs::metadata(&journal_path).unwrap().permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_mode(0o400);
    fs::set_permissions(&journal_path, read_only).unwrap();

    let refusal = fixture.runtime.recover_session(&fixture.principal, request);
    fs::set_permissions(&journal_path, original_permissions).unwrap();
    assert!(refusal.is_err(), "the read-only journal accepted a refusal");

    let HostedResponseV2::Status { session, .. } =
        session_status(&fixture.runtime, &fixture.capability)
    else {
        panic!("wrong status response")
    };
    assert_eq!(session.status, SessionStatusV2::Quarantined);
    assert_eq!(session.next_client_sequence, 3);
    let journal = fixture
        .store
        .read_journal(&fixture.capability.session_id)
        .unwrap();
    assert!(!journal.entries.iter().any(|entry| matches!(
        entry.entry.event,
        JournalEventV2::RecoveryAttemptStarted { .. }
            | JournalEventV2::RecoveryCommitted { .. }
            | JournalEventV2::RecoveryRefused { .. }
    )));
}

#[test]
fn open_recaptures_wall_time_and_rejects_evidence_that_expired_during_setup() {
    let directory = tempfile::tempdir().unwrap();
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('b');
    let request = signed_open_request_with_lifetime(
        &placement_signer,
        &principal,
        "open-expiry-during-setup",
        state_quotas.clone(),
        reservation,
        150,
    );
    let store = DurableSessionStoreV2::open(directory.path().join("state"), node_signer).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runtime = HostedV2Runtime::open(
        HostedV2RuntimeConfig {
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quotas,
        },
        store.clone(),
        Arc::new(BlockingPlacementAuthorizer {
            inner: PinnedEd25519PlacementAuthorizerV2::new(placement_signer.public_key()),
            entered: entered.clone(),
            release: release.clone(),
            block_call: 0,
            calls: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let open_runtime = runtime.clone();
    let open_principal = principal.clone();
    let opened = thread::spawn(move || open_runtime.open_session(&open_principal, request));
    entered.wait();
    thread::sleep(Duration::from_millis(225));
    release.wait();

    let error = opened.join().unwrap().unwrap_err();
    assert!(
        format!("{error:#}").contains("expired"),
        "expired Open evidence reached durable commit: {error:#}"
    );
    assert!(store.list_session_ids().unwrap().is_empty());
}

#[test]
fn final_open_authorization_does_not_hold_global_state_lock() {
    let directory = tempfile::tempdir().unwrap();
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('c');
    let request = signed_open_request(
        &placement_signer,
        &principal,
        "open-final-authorizer-lock",
        state_quotas.clone(),
        reservation,
    );
    let store = DurableSessionStoreV2::open(directory.path().join("state"), node_signer).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runtime = HostedV2Runtime::open(
        HostedV2RuntimeConfig {
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quotas,
        },
        store,
        Arc::new(BlockingPlacementAuthorizer {
            inner: PinnedEd25519PlacementAuthorizerV2::new(placement_signer.public_key()),
            entered: entered.clone(),
            release: release.clone(),
            block_call: 1,
            calls: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let open_runtime = runtime.clone();
    let opened = thread::spawn(move || open_runtime.open_session(&principal, request));
    entered.wait();

    let (probe_sender, probe_receiver) = mpsc::channel();
    let probe_runtime = runtime.clone();
    thread::spawn(move || {
        let _ = probe_sender.send(probe_runtime.unreadable_sessions());
    });
    let probe = probe_receiver.recv_timeout(Duration::from_secs(1));
    if probe.is_err() {
        release.wait();
        let _ = opened.join();
        panic!("global state lock was held during final Open authorization")
    }
    assert!(probe.unwrap().unwrap().is_empty());
    release.wait();
    assert!(matches!(
        opened.join().unwrap().unwrap(),
        HostedResponseV2::SessionOpened { .. }
    ));
}

#[test]
fn submit_rejects_evidence_expiring_inside_final_authorizer_without_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('d');
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest("submit-final-evidence-expiry-session"),
    )
    .unwrap();
    let capability = open_capability(&state_session, "submit-final-evidence-expiry-open");
    let proof_operation = prepared_open_operation("submit-expiry-open-proof");
    let (open_lease, target) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session.clone(),
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
        reservation.clone(),
        None,
        None,
        "submit-final-evidence-expiry-open",
        0,
        PlacementPurposeV2::OpenSession,
        &proof_operation,
        None,
    );
    let store = DurableSessionStoreV2::open(directory.path().join("state"), node_signer).unwrap();
    let initial = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    initial
        .open_session(
            &principal,
            OpenSessionRequestV2 {
                client_request_id: "submit-final-evidence-expiry-open".to_owned(),
                state_tier: SessionStateTierV2::Stateless,
                proposed_capability: capability.clone(),
                capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
                placement_lease: open_lease,
            },
        )
        .unwrap();
    drop(initial);

    let operation = prepared_open_operation("submit-final-evidence-expiry");
    let (execute_lease, _) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session,
        SessionStateTierV2::Stateless,
        state_quotas.clone(),
        reservation,
        Some(&target),
        None,
        "submit-final-evidence-expiry",
        1,
        PlacementPurposeV2::Execute,
        &operation,
        None,
    );
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runtime = HostedV2Runtime::open(
        HostedV2RuntimeConfig {
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quotas,
        },
        store.clone(),
        Arc::new(BlockingPlacementAuthorizer {
            inner: PinnedEd25519PlacementAuthorizerV2::new(placement_signer.public_key()),
            entered: entered.clone(),
            release: release.clone(),
            block_call: 1,
            calls: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let operation_id = operation.operation_id.clone();
    let submit_runtime = runtime.clone();
    let submit_principal = principal.clone();
    let submit_capability = capability.clone();
    let submitted = thread::spawn(move || {
        submit_runtime.submit_operation(
            &submit_principal,
            SubmitOperationRequestV2 {
                credentials: submit_capability.into(),
                client_request_id: "submit-final-evidence-expiry".to_owned(),
                client_sequence: 1,
                operation,
                placement_lease: execute_lease,
            },
        )
    });
    entered.wait();
    thread::sleep(Duration::from_millis(4_200));
    release.wait();
    let error = submitted.join().unwrap().unwrap_err();
    assert!(format!("{error:#}").contains("expired"), "{error:#}");
    let journal = store.read_journal(&capability.session_id).unwrap();
    assert!(journal.entries.iter().all(|entry| !matches!(
        &entry.entry.event,
        JournalEventV2::OperationAccepted { operation_id: accepted, .. }
            if accepted == &operation_id
    )));
}

#[test]
fn recovery_authorization_does_not_hold_global_state_lock() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = direct_recovery_fixture(&directory.path().join("state"), false);
    let request = recovery_request(&fixture, "direct-recovery-blocked-authorizer");
    let store = fixture.store.clone();
    let state_quotas = fixture.state_quotas.clone();
    let principal = fixture.principal.clone();
    let capability = fixture.capability.clone();
    let public_key = fixture.placement_signer.public_key();
    drop(fixture.runtime);

    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runtime = HostedV2Runtime::open(
        HostedV2RuntimeConfig {
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quotas,
        },
        store,
        Arc::new(BlockingPlacementAuthorizer {
            inner: PinnedEd25519PlacementAuthorizerV2::new(public_key),
            entered: entered.clone(),
            release: release.clone(),
            block_call: 0,
            calls: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let recovery_runtime = runtime.clone();
    let recovery_principal = principal.clone();
    let recovery =
        thread::spawn(move || recovery_runtime.recover_session(&recovery_principal, request));
    entered.wait();

    let (status_sender, status_receiver) = mpsc::channel();
    let status_runtime = runtime.clone();
    let status_principal = principal.clone();
    let status_capability = capability.clone();
    thread::spawn(move || {
        let response = status_runtime.status(
            &status_principal,
            SessionQueryV2 {
                credentials: status_capability.into(),
                operation_id: None,
            },
        );
        let _ = status_sender.send(response);
    });
    let started = Instant::now();
    let status = status_receiver.recv_timeout(Duration::from_secs(1));
    if status.is_err() {
        release.wait();
        let _ = recovery.join();
        panic!("Status blocked behind recovery placement authorization")
    }
    assert!(matches!(
        status.unwrap().unwrap(),
        HostedResponseV2::Status { .. }
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    let close_started = Instant::now();
    let close_error = runtime
        .close_session(
            &principal,
            SessionMutationRequestV2 {
                credentials: capability.into(),
                client_request_id: "close-during-recovery-auth".to_owned(),
                client_sequence: 3,
            },
        )
        .unwrap_err();
    assert!(
        format!("{close_error:#}").contains("close refuses while an operation is being prepared"),
        "{close_error:#}"
    );
    assert!(close_started.elapsed() < Duration::from_secs(1));

    release.wait();
    let HostedResponseV2::Committed { receipt } = recovery.join().unwrap().unwrap() else {
        panic!("wrong recovery response")
    };
    assert!(matches!(
        receipt.entry.event,
        JournalEventV2::RecoveryCommitted { .. }
    ));
}

#[test]
fn recovery_rechecks_expiry_after_checkpoint_work_before_allocating_generation() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = direct_recovery_fixture(&directory.path().join("state"), false);
    let request = recovery_request(&fixture, "recovery-final-evidence-expiry");
    let store = fixture.store.clone();
    let state_quotas = fixture.state_quotas.clone();
    let principal = fixture.principal.clone();
    let capability = fixture.capability.clone();
    let previous_actor = fixture.previous_actor.clone();
    let public_key = fixture.placement_signer.public_key();
    drop(fixture.runtime);

    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runtime = HostedV2Runtime::open(
        HostedV2RuntimeConfig {
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(1).unwrap(),
            shim_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            runtime_executable: Path::new(env!("CARGO_BIN_EXE_O")).to_path_buf(),
            state_quota_generation: GenerationV1::new(1).unwrap(),
            state_quotas,
        },
        store.clone(),
        Arc::new(BlockingPlacementAuthorizer {
            inner: PinnedEd25519PlacementAuthorizerV2::new(public_key),
            entered: entered.clone(),
            release: release.clone(),
            block_call: 2,
            calls: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let recover_runtime = runtime.clone();
    let recover_principal = principal.clone();
    let recovery =
        thread::spawn(move || recover_runtime.recover_session(&recover_principal, request));
    entered.wait();
    thread::sleep(Duration::from_millis(4_200));
    release.wait();
    let error = recovery.join().unwrap().unwrap_err();
    assert!(format!("{error:#}").contains("expired"), "{error:#}");
    let HostedResponseV2::Status { session, .. } = session_status(&runtime, &capability) else {
        panic!("wrong recovery-expiry status response")
    };
    assert_eq!(session.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(
        session.actor.actor_generation.as_ref(),
        Some(&previous_actor)
    );
    assert_eq!(
        session.actor.next_actor_generation.get(),
        previous_actor.generation().get() + 1
    );
    let journal = store.read_journal(&capability.session_id).unwrap();
    assert!(journal.entries.iter().all(|entry| !matches!(
        entry.entry.event,
        JournalEventV2::RecoveryAttemptStarted { .. }
    )));
}

#[test]
fn live_actor_restart_requires_signed_reset_generation_fence_before_ready() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let signer = HostedNodeSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let capability;
    let previous_generation;
    {
        let store = DurableSessionStoreV2::open(&state_root, signer.clone()).unwrap();
        let mut session = install_open_session(
            &store,
            &signer,
            "live-restart",
            SessionStateTierV2::LiveActorOnly,
            StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap(),
        );
        install_completed_actor_operation(
            &store,
            &signer,
            &mut session,
            "live-restart",
            "settled".to_owned(),
        );
        capability = session.capability;
        previous_generation = session.actor_generation;
    }

    let reopened_store = DurableSessionStoreV2::open(&state_root, signer).unwrap();
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status { session, .. } = session_status(&restarted, &capability) else {
        panic!("wrong status response")
    };
    assert_eq!(session.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(session.actor.health, ActorHealthV2::RecoveryRequired);
    assert_eq!(
        session.actor.actor_generation,
        Some(previous_generation.clone())
    );
    assert_eq!(session.actor.next_actor_generation.get(), 2);

    let reset = restarted
        .reset_session(
            &principal_digest('a'),
            SessionMutationRequestV2 {
                credentials: capability.clone().into(),
                client_request_id: "reset-live-restart".to_owned(),
                client_sequence: 2,
            },
        )
        .unwrap();
    let HostedResponseV2::Committed { receipt } = reset else {
        panic!("wrong reset response")
    };
    receipt.verify().unwrap();
    let JournalEventV2::SessionReset {
        previous_actor_generation,
        next_actor_generation,
        ..
    } = receipt.entry.event
    else {
        panic!("reset did not publish a SessionReset fence")
    };
    assert_eq!(previous_actor_generation, Some(previous_generation));
    assert_eq!(next_actor_generation.get(), 2);
    let HostedResponseV2::Status { session, .. } = session_status(&restarted, &capability) else {
        panic!("wrong status response")
    };
    assert_eq!(session.status, SessionStatusV2::Ready);
    assert_eq!(session.actor.health, ActorHealthV2::Ready);
    assert!(session.actor.actor_generation.is_none());
    assert_eq!(session.actor.next_actor_generation.get(), 2);
}

#[test]
fn restart_uses_signed_terminal_state_disposition_without_inventing_actor_state() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let signer = HostedNodeSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let store = DurableSessionStoreV2::open(&state_root, signer.clone()).unwrap();

    let mut untouched = install_open_session(
        &store,
        &signer,
        "untouched-terminal",
        SessionStateTierV2::CheckpointRestore,
        StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
    );
    let untouched_sha256 =
        install_started_actor_operation(&store, &signer, &mut untouched, "untouched-terminal");
    install_operation_terminal_with_state(
        &store,
        &signer,
        &mut untouched,
        "untouched-terminal",
        untouched_sha256,
        "admission ended before backend dispatch".to_owned(),
        true,
        false,
    );
    let untouched_capability = untouched.capability;

    let mut nondurable = install_open_session(
        &store,
        &signer,
        "nondurable-terminal",
        SessionStateTierV2::LiveActorOnly,
        StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap(),
    );
    let nondurable_actor = nondurable.actor_generation.clone();
    let nondurable_sha256 =
        install_started_actor_operation(&store, &signer, &mut nondurable, "nondurable-terminal");
    install_operation_terminal_with_state(
        &store,
        &signer,
        &mut nondurable,
        "nondurable-terminal",
        nondurable_sha256,
        "backend state was touched but could not be retained".to_owned(),
        false,
        true,
    );
    let nondurable_capability = nondurable.capability;
    drop(store);

    let reopened_store = DurableSessionStoreV2::open(&state_root, signer).unwrap();
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status {
        session: untouched, ..
    } = session_status(&restarted, &untouched_capability)
    else {
        panic!("wrong untouched status response")
    };
    assert_eq!(untouched.status, SessionStatusV2::Ready);
    assert!(untouched.actor.actor_id.is_none());
    assert!(untouched.actor.actor_generation.is_none());
    assert!(untouched.actor.checkpoint_sha256.is_none());
    assert_eq!(untouched.actor.next_actor_generation.get(), 2);
    let untouched_journal = reopened_store
        .read_journal(&untouched_capability.session_id)
        .unwrap();
    assert!(matches!(
        untouched_journal.entries.last().unwrap().entry.event,
        JournalEventV2::ActorGenerationRetired { .. }
    ));

    let HostedResponseV2::Status {
        session: nondurable,
        ..
    } = session_status(&restarted, &nondurable_capability)
    else {
        panic!("wrong nondurable status response")
    };
    assert_eq!(nondurable.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(
        nondurable.actor.actor_generation.as_ref(),
        Some(&nondurable_actor)
    );
    assert_eq!(nondurable.actor.next_actor_generation.get(), 2);
    let nondurable_journal = reopened_store
        .read_journal(&nondurable_capability.session_id)
        .unwrap();
    assert!(matches!(
        nondurable_journal.entries.last().unwrap().entry.event,
        JournalEventV2::ActorStateLost { .. }
    ));
}

#[test]
fn near_total_tail_repair_runtime_open_and_clean_reopen_preserve_remaining_headroom() {
    const SIGNED_HARD_TOTAL: u64 = 1024 * 1024;

    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let initial_root_bytes = store.durable_bytes().unwrap();
    let reservation_bytes = SIGNED_HARD_TOTAL
        .checked_sub(initial_root_bytes)
        .and_then(|bytes| bytes.checked_sub(AUTHORITY_CONTROL_HEADROOM))
        .expect("test state root leaves no ordinary session capacity");
    let state_quotas =
        StateQuotaLimitsV2::new(1, 1, 0, reservation_bytes, SIGNED_HARD_TOTAL).unwrap();
    let reservation = StateReservationV2::new(1, 0, reservation_bytes).unwrap();
    assert_eq!(
        initial_root_bytes + reservation_bytes + AUTHORITY_CONTROL_HEADROOM,
        SIGNED_HARD_TOTAL
    );
    let principal = principal_digest('a');
    let open_request = signed_open_request(
        &placement_signer,
        &principal,
        "near-total-runtime-repair",
        state_quotas.clone(),
        reservation,
    );
    let capability = open_request.proposed_capability.clone();
    let running = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    assert!(matches!(
        running.open_session(&principal, open_request).unwrap(),
        HostedResponseV2::SessionOpened { .. }
    ));
    running.shutdown().unwrap();
    drop(running);
    drop(store);

    let journal_path = state_root
        .join("sessions")
        .join(&capability.session_id)
        .join("journal.cborseq");
    let mut journal = OpenOptions::new().append(true).open(&journal_path).unwrap();
    journal.write_all(&[0, 1]).unwrap();
    journal.sync_all().unwrap();
    drop(journal);

    let repaired_store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let remaining_after_repair = repaired_store.remaining_authority_control_headroom_bytes();
    assert!(remaining_after_repair < AUTHORITY_CONTROL_HEADROOM);
    let durable_after_repair = repaired_store.durable_bytes().unwrap();
    let repaired_runtime = authorized_runtime(
        repaired_store.clone(),
        state_quotas.clone(),
        &placement_signer,
    );
    let HostedResponseV2::Status { session, .. } = session_status(&repaired_runtime, &capability)
    else {
        panic!("wrong repaired-runtime status response")
    };
    assert_eq!(session.status, SessionStatusV2::Ready);
    repaired_runtime.shutdown().unwrap();
    drop(repaired_runtime);
    drop(repaired_store);

    let clean_store = DurableSessionStoreV2::open(&state_root, node_signer).unwrap();
    assert_eq!(clean_store.durable_bytes().unwrap(), durable_after_repair);
    assert_eq!(
        clean_store.remaining_authority_control_headroom_bytes(),
        remaining_after_repair
    );
    let clean_runtime = authorized_runtime(clean_store, state_quotas, &placement_signer);
    let HostedResponseV2::Status { session, .. } = session_status(&clean_runtime, &capability)
    else {
        panic!("wrong clean-runtime status response")
    };
    assert_eq!(session.status, SessionStatusV2::Ready);
}

#[cfg(debug_assertions)]
#[test]
fn current_head_views_recheck_store_poison_after_the_prelock_window() {
    for view_kind in ["status", "actors"] {
        let directory = tempfile::tempdir().unwrap();
        let state_root = directory.path().join("state");
        let node_signer = HostedNodeSignerV2::generate().unwrap();
        let state_quotas = quotas(8 * 1024 * 1024);
        let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
        let installed = install_open_session(
            &store,
            &node_signer,
            &format!("poison-race-{view_kind}"),
            SessionStateTierV2::Stateless,
            StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap(),
        );
        let running = runtime(store.clone(), state_quotas);
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        running
            .inject_current_view_prelock_barrier_for_test(entered.clone(), release.clone())
            .unwrap();
        let view_runtime = running.clone();
        let credentials = installed.capability.into();
        let principal = principal_digest('a');
        let view = thread::spawn(move || {
            let query = SessionQueryV2 {
                credentials,
                operation_id: None,
            };
            match view_kind {
                "status" => view_runtime.status(&principal, query),
                "actors" => view_runtime.actors(&principal, query),
                _ => unreachable!(),
            }
        });

        entered.wait();
        store.inject_reopen_required_for_test();
        release.wait();
        let error = view
            .join()
            .expect("current-head view thread panicked")
            .expect_err("a view paused before the state lock must observe store poison");
        assert!(
            format!("{error:#}").contains("durable store state is indeterminate"),
            "{view_kind}: {error:#}"
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn checkpoint_failure_gap_stays_executing_and_refuses_reset_or_close() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('a');
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest("checkpoint-failure-gap-session"),
    )
    .unwrap();
    let capability = open_capability(&state_session, "checkpoint-failure-gap-open");
    let operation = python_operation("checkpoint-failure-gap", "x = 42\nx");
    let (open_lease, target) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation.clone(),
        None,
        None,
        "checkpoint-failure-gap-open",
        0,
        PlacementPurposeV2::OpenSession,
        &operation,
        None,
    );
    let store = DurableSessionStoreV2::open(&state_root, node_signer).unwrap();
    let running = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    running
        .open_session(
            &principal,
            OpenSessionRequestV2 {
                client_request_id: "checkpoint-failure-gap-open".to_owned(),
                state_tier: SessionStateTierV2::CheckpointRestore,
                capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
                proposed_capability: capability.clone(),
                placement_lease: open_lease,
            },
        )
        .unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    running
        .inject_checkpoint_failure_gap_for_test(
            &capability.session_id,
            entered.clone(),
            release.clone(),
        )
        .unwrap();
    let (execute_lease, _) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session,
        SessionStateTierV2::CheckpointRestore,
        state_quotas,
        reservation,
        Some(&target),
        None,
        "checkpoint-failure-gap-execute",
        1,
        PlacementPurposeV2::Execute,
        &operation,
        None,
    );
    assert!(matches!(
        running
            .submit_operation(
                &principal,
                SubmitOperationRequestV2 {
                    credentials: capability.clone().into(),
                    client_request_id: "checkpoint-failure-gap-execute".to_owned(),
                    client_sequence: 1,
                    placement_lease: execute_lease,
                    operation: operation.clone(),
                },
            )
            .unwrap(),
        HostedResponseV2::Committed { .. }
    ));

    entered.wait();
    let gap_status = session_status(&running, &capability);
    let reset = running.reset_session(
        &principal,
        SessionMutationRequestV2 {
            credentials: capability.clone().into(),
            client_request_id: "checkpoint-failure-gap-reset".to_owned(),
            client_sequence: 2,
        },
    );
    let close = running.close_session(
        &principal,
        SessionMutationRequestV2 {
            credentials: capability.clone().into(),
            client_request_id: "checkpoint-failure-gap-close".to_owned(),
            client_sequence: 2,
        },
    );
    release.wait();

    let HostedResponseV2::Status { session: gap, .. } = gap_status else {
        panic!("wrong checkpoint-failure gap status response")
    };
    assert_eq!(gap.status, SessionStatusV2::Executing);
    assert_eq!(
        gap.operations[&operation.operation_id].status,
        OperationStatusV2::Running
    );
    for (mutation, result) in [("reset", reset), ("close", close)] {
        let error = result.expect_err("active operation mutation must be refused");
        assert!(
            format!("{error:#}").contains("accepted or running"),
            "{mutation}: {error:#}"
        );
    }
    let (settled, _) = wait_for_operation(&running, &capability, &operation.operation_id);
    assert_eq!(settled.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(
        settled.operations[&operation.operation_id].status,
        OperationStatusV2::Failed
    );
    let journal = store.read_journal(&capability.session_id).unwrap();
    let tail = &journal.entries[journal.entries.len() - 2..];
    assert!(matches!(
        tail[0].entry.event,
        JournalEventV2::ActorCheckpointFailed { .. }
    ));
    assert!(matches!(
        tail[1].entry.event,
        JournalEventV2::OperationTerminal {
            state_durable: false,
            actor_state_touched: true,
            ..
        }
    ));
}

#[test]
fn reconstruction_rejects_every_event_after_session_closed() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let signer = HostedNodeSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let store = DurableSessionStoreV2::open(&state_root, signer.clone()).unwrap();
    let mut installed = install_open_session(
        &store,
        &signer,
        "post-close-terminal",
        SessionStateTierV2::LiveActorOnly,
        StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap(),
    );
    let operation_sha256 =
        install_started_actor_operation(&store, &signer, &mut installed, "post-close-terminal");
    install_operation_terminal(
        &store,
        &signer,
        &mut installed,
        "post-close-terminal",
        operation_sha256.clone(),
        "settled-before-close".to_owned(),
    );
    installed.head = append_event(
        &store,
        &signer,
        &installed.capability.session_id,
        &installed.head,
        JournalEventV2::SessionClosed {
            client_sequence: 2,
            client_request_id: "post-close-terminal-close".to_owned(),
            request_sha256: digest("post-close-terminal-close-request").to_string(),
            actor_generation: Some(installed.actor_generation.clone()),
        },
    );
    append_event(
        &store,
        &signer,
        &installed.capability.session_id,
        &installed.head,
        JournalEventV2::OperationTerminal {
            operation_id: "operation-post-close-terminal".to_owned(),
            operation_sha256,
            finished_unix_ms: unix_time_ms().unwrap(),
            outcome: OperationOutcomeV2::failed(
                OperationFailureStageV2::Infrastructure,
                "impossible-post-close-terminal",
                "must not resurrect a closed session",
            ),
            state_durable: true,
            actor_state_touched: true,
        },
    );
    let session_id = installed.capability.session_id;
    drop(store);

    let reopened = DurableSessionStoreV2::open(&state_root, signer).unwrap();
    let restarted = runtime(reopened, state_quotas);
    let unreadable = restarted.unreadable_sessions().unwrap().join("\n");
    assert!(unreadable.contains(&session_id), "{unreadable}");
    assert!(
        unreadable.contains("event after SessionClosed"),
        "{unreadable}"
    );
}

#[cfg(debug_assertions)]
#[test]
fn failed_second_actor_loss_append_keeps_first_frame_head_and_generation_fence() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('a');
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest("second-actor-loss-frame-session"),
    )
    .unwrap();
    let capability = open_capability(&state_session, "second-actor-loss-frame-open");
    let operation = python_operation("second-actor-loss-frame", "import os\nos._exit(29)\n0");
    let (open_lease, target) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation.clone(),
        None,
        None,
        "second-actor-loss-frame-open",
        0,
        PlacementPurposeV2::OpenSession,
        &operation,
        None,
    );
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let running = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    running
        .open_session(
            &principal,
            OpenSessionRequestV2 {
                client_request_id: "second-actor-loss-frame-open".to_owned(),
                state_tier: SessionStateTierV2::CheckpointRestore,
                capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
                proposed_capability: capability.clone(),
                placement_lease: open_lease,
            },
        )
        .unwrap();
    let (execute_lease, _) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session,
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation,
        Some(&target),
        None,
        "second-actor-loss-frame-execute",
        1,
        PlacementPurposeV2::Execute,
        &operation,
        None,
    );
    // OperationAccepted, OperationStarted, and ActorStateLost succeed. The
    // following OperationInterrupted append fails before opening the journal.
    store
        .inject_append_failure_after_successes_for_test(3)
        .unwrap();
    let HostedResponseV2::Committed { receipt } = running
        .submit_operation(
            &principal,
            SubmitOperationRequestV2 {
                credentials: capability.clone().into(),
                client_request_id: "second-actor-loss-frame-execute".to_owned(),
                client_sequence: 1,
                placement_lease: execute_lease,
                operation: operation.clone(),
            },
        )
        .unwrap()
    else {
        panic!("wrong operation-accepted response")
    };
    let previous_actor = match receipt.entry.event {
        JournalEventV2::OperationAccepted {
            actor_generation: Some(actor_generation),
            ..
        } => actor_generation,
        other => panic!("accepted operation has no actor generation: {other:?}"),
    };

    let live = (0..200)
        .find_map(|_| {
            let HostedResponseV2::Status { session, .. } = session_status(&running, &capability)
            else {
                panic!("wrong live status response")
            };
            if session.status == SessionStatusV2::Quarantined {
                Some(session)
            } else {
                thread::sleep(Duration::from_millis(10));
                None
            }
        })
        .expect("injected second append failure did not quarantine the session");
    let journal = store.read_journal(&capability.session_id).unwrap();
    let durable_head = journal.entries.last().unwrap();
    assert!(matches!(
        durable_head.entry.event,
        JournalEventV2::ActorStateLost { .. }
    ));
    assert_eq!(live.journal_head_sha256, durable_head.entry_sha256);
    assert_eq!(live.actor.actor_generation.as_ref(), Some(&previous_actor));
    assert_eq!(live.actor.next_actor_generation.get(), 2);
    assert!(live.actor.actor_id.is_none());
    let bytes_before_restart = store.session_durable_bytes(&capability.session_id).unwrap();
    let (runtime_root_bytes, runtime_session_bytes, runtime_reserved_bytes) = running
        .durable_accounting_for_test(&capability.session_id)
        .unwrap();
    assert_eq!(runtime_root_bytes, store.durable_bytes().unwrap());
    assert_eq!(runtime_session_bytes, bytes_before_restart);
    assert_eq!(runtime_reserved_bytes, 0);
    running.shutdown().unwrap();
    drop(running);
    drop(store);

    let reopened_store = DurableSessionStoreV2::open(&state_root, node_signer).unwrap();
    assert_eq!(
        reopened_store
            .session_durable_bytes(&capability.session_id)
            .unwrap(),
        bytes_before_restart
    );
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status {
        session: restarted, ..
    } = session_status(&restarted, &capability)
    else {
        panic!("wrong restart status response")
    };
    assert_eq!(restarted.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(
        restarted.operations[&operation.operation_id].status,
        OperationStatusV2::Ambiguous
    );
    assert_eq!(
        restarted.actor.actor_generation.as_ref(),
        Some(&previous_actor)
    );
    assert_eq!(restarted.actor.next_actor_generation.get(), 2);
    assert!(restarted.actor.actor_id.is_none());
}

#[cfg(debug_assertions)]
#[test]
fn failed_first_prestart_append_quarantines_until_restart_fences_generation() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('a');
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest("first-prestart-frame-session"),
    )
    .unwrap();
    let capability = open_capability(&state_session, "first-prestart-frame-open");
    let operation = python_operation("first-prestart-frame", "40 + 2");
    let (open_lease, target) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation.clone(),
        None,
        None,
        "first-prestart-frame-open",
        0,
        PlacementPurposeV2::OpenSession,
        &operation,
        None,
    );
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let running = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    running
        .open_session(
            &principal,
            OpenSessionRequestV2 {
                client_request_id: "first-prestart-frame-open".to_owned(),
                state_tier: SessionStateTierV2::CheckpointRestore,
                capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
                proposed_capability: capability.clone(),
                placement_lease: open_lease,
            },
        )
        .unwrap();
    let (execute_lease, _) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session,
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation,
        Some(&target),
        None,
        "first-prestart-frame-execute",
        1,
        PlacementPurposeV2::Execute,
        &operation,
        None,
    );
    running
        .inject_actor_close_before_execute_for_test(&capability.session_id)
        .unwrap();
    // OperationAccepted succeeds. The first lifecycle append,
    // OperationInterrupted, then fails before opening/writing the journal.
    store
        .inject_append_failure_after_successes_for_test(1)
        .unwrap();
    let request = SubmitOperationRequestV2 {
        credentials: capability.clone().into(),
        client_request_id: "first-prestart-frame-execute".to_owned(),
        client_sequence: 1,
        placement_lease: execute_lease,
        operation: operation.clone(),
    };
    let error = running
        .submit_operation(&principal, request.clone())
        .expect_err("injected first interruption append failure must fail this response");
    assert!(
        format!("{error:#}")
            .contains("accepted operation interruption append failed before actor fencing"),
        "{error:#}"
    );

    let journal = store.read_journal(&capability.session_id).unwrap();
    let accepted = journal.entries.last().unwrap();
    let previous_actor = match &accepted.entry.event {
        JournalEventV2::OperationAccepted {
            actor_generation: Some(actor),
            ..
        } => actor.clone(),
        other => panic!("durable head is not the accepted stateful operation: {other:?}"),
    };
    let HostedResponseV2::Status { session: live, .. } = session_status(&running, &capability)
    else {
        panic!("wrong live status response")
    };
    assert_eq!(live.status, SessionStatusV2::Quarantined);
    assert_eq!(live.journal_head_sha256, accepted.entry_sha256);
    assert_eq!(
        live.operations[&operation.operation_id].status,
        OperationStatusV2::Accepted
    );
    assert!(live.actor.actor_id.is_none());
    assert_eq!(live.actor.actor_generation.as_ref(), Some(&previous_actor));
    assert_eq!(live.actor.next_actor_generation.get(), 2);
    assert!(!running.has_worker_for_test(&capability.session_id).unwrap());
    let (runtime_root_bytes, runtime_session_bytes, runtime_reserved_bytes) = running
        .durable_accounting_for_test(&capability.session_id)
        .unwrap();
    assert_eq!(runtime_root_bytes, store.durable_bytes().unwrap());
    assert_eq!(
        runtime_session_bytes,
        store.session_durable_bytes(&capability.session_id).unwrap()
    );
    assert_eq!(runtime_reserved_bytes, 0);
    let HostedResponseV2::Committed { receipt: duplicate } = running
        .submit_operation(&principal, request)
        .expect("exact retry must return the durable accepted receipt")
    else {
        panic!("wrong exact-retry response")
    };
    assert_eq!(duplicate.entry_sha256, accepted.entry_sha256);
    running.shutdown().unwrap();
    drop(running);
    drop(store);

    let reopened_store = DurableSessionStoreV2::open(&state_root, node_signer).unwrap();
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status {
        session: restarted, ..
    } = session_status(&restarted, &capability)
    else {
        panic!("wrong restart status response")
    };
    assert_eq!(restarted.status, SessionStatusV2::Ready);
    assert_eq!(
        restarted.operations[&operation.operation_id].status,
        OperationStatusV2::NotStarted
    );
    assert!(restarted.actor.actor_id.is_none());
    assert!(restarted.actor.actor_generation.is_none());
    assert_eq!(restarted.actor.next_actor_generation.get(), 2);
    let restarted_journal = reopened_store.read_journal(&capability.session_id).unwrap();
    assert!(matches!(
        restarted_journal.entries.last().unwrap().entry.event,
        JournalEventV2::ActorGenerationRetired { .. }
    ));
}

#[cfg(debug_assertions)]
#[test]
fn failed_second_prestart_append_keeps_interruption_head_and_quarantines_actor() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('a');
    let state_session = StateSessionIdV2::new(
        NODE_ID,
        GenerationV1::new(1).unwrap(),
        digest("second-prestart-frame-session"),
    )
    .unwrap();
    let capability = open_capability(&state_session, "second-prestart-frame-open");
    let operation = python_operation("second-prestart-frame", "40 + 2");
    let (open_lease, target) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session.clone(),
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation.clone(),
        None,
        None,
        "second-prestart-frame-open",
        0,
        PlacementPurposeV2::OpenSession,
        &operation,
        None,
    );
    let store = DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap();
    let running = authorized_runtime(store.clone(), state_quotas.clone(), &placement_signer);
    running
        .open_session(
            &principal,
            OpenSessionRequestV2 {
                client_request_id: "second-prestart-frame-open".to_owned(),
                state_tier: SessionStateTierV2::CheckpointRestore,
                capability_commitment: open_capability_commitment_v2(&capability).unwrap(),
                proposed_capability: capability.clone(),
                placement_lease: open_lease,
            },
        )
        .unwrap();
    let (execute_lease, _) = existing_or_open_lease(
        &placement_signer,
        &principal,
        state_session,
        SessionStateTierV2::CheckpointRestore,
        state_quotas.clone(),
        reservation,
        Some(&target),
        None,
        "second-prestart-frame-execute",
        1,
        PlacementPurposeV2::Execute,
        &operation,
        None,
    );
    running
        .inject_actor_close_before_execute_for_test(&capability.session_id)
        .unwrap();
    // OperationAccepted and OperationInterrupted succeed. The following
    // state-empty ActorGenerationRetired frame fails without writing bytes.
    store
        .inject_append_failure_after_successes_for_test(2)
        .unwrap();
    let error = running
        .submit_operation(
            &principal,
            SubmitOperationRequestV2 {
                credentials: capability.clone().into(),
                client_request_id: "second-prestart-frame-execute".to_owned(),
                client_sequence: 1,
                placement_lease: execute_lease,
                operation: operation.clone(),
            },
        )
        .expect_err("injected actor-fence append failure must fail this response");
    assert!(
        format!("{error:#}").contains("actor-generation fence append failed"),
        "{error:#}"
    );

    let journal = store.read_journal(&capability.session_id).unwrap();
    let durable_head = journal.entries.last().unwrap();
    assert!(matches!(
        durable_head.entry.event,
        JournalEventV2::OperationInterrupted {
            classification: OperationStatusV2::NotStarted,
            ..
        }
    ));
    let previous_actor = journal
        .entries
        .iter()
        .find_map(|entry| match &entry.entry.event {
            JournalEventV2::OperationAccepted {
                actor_generation: Some(actor),
                ..
            } => Some(actor.clone()),
            _ => None,
        })
        .expect("accepted stateful operation has no actor generation");
    let HostedResponseV2::Status { session: live, .. } = session_status(&running, &capability)
    else {
        panic!("wrong live status response")
    };
    assert_eq!(live.status, SessionStatusV2::Quarantined);
    assert_eq!(live.journal_head_sha256, durable_head.entry_sha256);
    assert_eq!(live.actor.actor_generation.as_ref(), Some(&previous_actor));
    assert!(!running.has_worker_for_test(&capability.session_id).unwrap());
    let session_bytes = store.session_durable_bytes(&capability.session_id).unwrap();
    let (runtime_root_bytes, runtime_session_bytes, runtime_reserved_bytes) = running
        .durable_accounting_for_test(&capability.session_id)
        .unwrap();
    assert_eq!(runtime_root_bytes, store.durable_bytes().unwrap());
    assert_eq!(runtime_session_bytes, session_bytes);
    assert_eq!(runtime_reserved_bytes, 0);
    running.shutdown().unwrap();
    drop(running);
    drop(store);

    let reopened_store = DurableSessionStoreV2::open(&state_root, node_signer).unwrap();
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status {
        session: restarted, ..
    } = session_status(&restarted, &capability)
    else {
        panic!("wrong restart status response")
    };
    assert_eq!(restarted.status, SessionStatusV2::Ready);
    assert_eq!(
        restarted.operations[&operation.operation_id].status,
        OperationStatusV2::NotStarted
    );
    assert!(restarted.actor.actor_id.is_none());
    assert!(restarted.actor.actor_generation.is_none());
    assert_eq!(restarted.actor.next_actor_generation.get(), 2);
    let restarted_journal = reopened_store.read_journal(&capability.session_id).unwrap();
    assert!(matches!(
        restarted_journal.entries.last().unwrap().entry.event,
        JournalEventV2::ActorGenerationRetired { .. }
    ));
}

#[test]
fn accepted_before_start_restart_retires_empty_generation_without_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let signer = HostedNodeSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let store = DurableSessionStoreV2::open(&state_root, signer.clone()).unwrap();
    let mut installed = install_open_session(
        &store,
        &signer,
        "accepted-before-start",
        SessionStateTierV2::LiveActorOnly,
        StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap(),
    );
    let prepared = PreparedOperationV2::new(
        "operation-accepted-before-start",
        TaskAttemptIdV1::new(
            digest("task:accepted-before-start"),
            GenerationV1::new(1).unwrap(),
        ),
        "python[7]^(41 + 1)_python[7]",
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 60_000,
        4096,
    )
    .unwrap();
    let operation_sha256 = prepared.sha256().unwrap();
    store
        .write_operation(&installed.capability.session_id, &prepared)
        .unwrap();
    installed.head = append_event(
        &store,
        &signer,
        &installed.capability.session_id,
        &installed.head,
        JournalEventV2::OperationAccepted {
            client_sequence: 1,
            client_request_id: "execute-accepted-before-start".to_owned(),
            request_sha256: digest("request:accepted-before-start").to_string(),
            operation_id: prepared.operation_id.clone(),
            task_attempt: prepared.task_attempt.clone(),
            operation_sha256,
            source_sha256: prepared.source_sha256.clone(),
            actor_id: Some("actor-accepted-before-start".to_owned()),
            actor_generation: Some(installed.actor_generation.clone()),
            placement_lease_sha256: digest("lease:accepted-before-start").to_string(),
            placement_lease_nonce: digest("nonce:accepted-before-start").to_string(),
        },
    );
    let capability = installed.capability;
    drop(store);

    let reopened_store = DurableSessionStoreV2::open(&state_root, signer).unwrap();
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status { session, .. } = session_status(&restarted, &capability) else {
        panic!("wrong restart status response")
    };
    assert_eq!(session.status, SessionStatusV2::Ready);
    assert_eq!(
        session.operations[&prepared.operation_id].status,
        OperationStatusV2::NotStarted
    );
    assert!(session.actor.actor_id.is_none());
    assert!(session.actor.actor_generation.is_none());
    assert_eq!(session.actor.next_actor_generation.get(), 2);
    let journal = reopened_store.read_journal(&capability.session_id).unwrap();
    assert!(matches!(
        journal.entries.last().unwrap().entry.event,
        JournalEventV2::ActorGenerationRetired { .. }
    ));
}

#[test]
fn accepted_before_start_restart_loses_preexisting_state_and_requires_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let signer = HostedNodeSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let store = DurableSessionStoreV2::open(&state_root, signer.clone()).unwrap();
    let mut installed = install_open_session(
        &store,
        &signer,
        "stateful-accepted-before-start",
        SessionStateTierV2::LiveActorOnly,
        StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap(),
    );
    install_completed_actor_operation(
        &store,
        &signer,
        &mut installed,
        "stateful-seed",
        "settled".to_owned(),
    );
    let prepared = PreparedOperationV2::new(
        "operation-stateful-accepted-before-start",
        TaskAttemptIdV1::new(
            digest("task:stateful-accepted-before-start"),
            GenerationV1::new(1).unwrap(),
        ),
        "python[7]^(42)_python[7]",
        BackendRegistry::global().catalog_sha256(),
        unix_time_ms().unwrap() + 60_000,
        4096,
    )
    .unwrap();
    let operation_sha256 = prepared.sha256().unwrap();
    store
        .write_operation(&installed.capability.session_id, &prepared)
        .unwrap();
    installed.head = append_event(
        &store,
        &signer,
        &installed.capability.session_id,
        &installed.head,
        JournalEventV2::OperationAccepted {
            client_sequence: 2,
            client_request_id: "execute-stateful-accepted-before-start".to_owned(),
            request_sha256: digest("request:stateful-accepted-before-start").to_string(),
            operation_id: prepared.operation_id.clone(),
            task_attempt: prepared.task_attempt.clone(),
            operation_sha256,
            source_sha256: prepared.source_sha256.clone(),
            actor_id: Some("actor-stateful-seed".to_owned()),
            actor_generation: Some(installed.actor_generation.clone()),
            placement_lease_sha256: digest("lease:stateful-accepted-before-start").to_string(),
            placement_lease_nonce: digest("nonce:stateful-accepted-before-start").to_string(),
        },
    );
    let capability = installed.capability;
    let previous_actor = installed.actor_generation;
    drop(store);

    let reopened_store = DurableSessionStoreV2::open(&state_root, signer).unwrap();
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status { session, .. } = session_status(&restarted, &capability) else {
        panic!("wrong restart status response")
    };
    assert_eq!(session.status, SessionStatusV2::RecoveryRequired);
    assert_eq!(
        session.operations[&prepared.operation_id].status,
        OperationStatusV2::NotStarted
    );
    assert!(session.actor.actor_id.is_none());
    assert_eq!(
        session.actor.actor_generation.as_ref(),
        Some(&previous_actor)
    );
    assert_eq!(session.actor.next_actor_generation.get(), 2);
    let journal = reopened_store.read_journal(&capability.session_id).unwrap();
    assert!(matches!(
        journal.entries.last().unwrap().entry.event,
        JournalEventV2::ActorStateLost { .. }
    ));
}

#[test]
fn near_full_session_durably_fences_lost_actor_and_remains_closeable() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let signer = HostedNodeSignerV2::generate().unwrap();
    let reservation_bytes = 512 * 1024;
    let state_quotas = quotas(reservation_bytes);
    let capability;
    let restart_head;
    {
        let store = DurableSessionStoreV2::open(&state_root, signer.clone()).unwrap();
        let mut session = install_open_session(
            &store,
            &signer,
            "near-full",
            SessionStateTierV2::LiveActorOnly,
            StateReservationV2::new(1, 0, reservation_bytes).unwrap(),
        );
        let current = store
            .session_durable_bytes(&session.capability.session_id)
            .unwrap();
        let close_probe = sign_event(
            &signer,
            &session.capability.session_id,
            Some(&session.head),
            JournalEventV2::SessionClosed {
                client_sequence: 2,
                client_request_id: "close-near-full".to_owned(),
                request_sha256: digest("close-near-full-request").to_string(),
                actor_generation: Some(session.actor_generation.clone()),
            },
        );
        let close_bytes = store.encoded_frame_bytes(&close_probe).unwrap();
        // The immutable operation blob contributes its own framed filesystem
        // allocation in addition to the signed Accepted/Started/Terminal
        // records estimated below. Leave that exact 64KiB fixture component
        // outside the message-padding budget.
        let target = reservation_bytes - close_bytes - ACTOR_FENCE_HEADROOM - 80 * 1024;
        let empty_terminal = sign_event(
            &signer,
            &session.capability.session_id,
            Some(&session.head),
            JournalEventV2::OperationTerminal {
                operation_id: "unused".to_owned(),
                operation_sha256: digest("unused-operation").to_string(),
                finished_unix_ms: unix_time_ms().unwrap(),
                outcome: OperationOutcomeV2::failed(
                    OperationFailureStageV2::Evaluate,
                    "fixture-terminal",
                    "",
                ),
                state_durable: true,
                actor_state_touched: true,
            },
        );
        let terminal_base = store.encoded_frame_bytes(&empty_terminal).unwrap();
        let prefix_budget = target
            .checked_sub(current + terminal_base + 4096)
            .expect("fixture reservation is too small") as usize;
        install_completed_actor_operation(
            &store,
            &signer,
            &mut session,
            "near-full",
            "x".repeat(prefix_budget),
        );
        let used = store
            .session_durable_bytes(&session.capability.session_id)
            .unwrap();
        assert!(
            used + CLOSE_HEADROOM + ACTOR_FENCE_HEADROOM <= reservation_bytes,
            "used={used} close={CLOSE_HEADROOM} fence={ACTOR_FENCE_HEADROOM} reservation={reservation_bytes}"
        );
        assert!(
            used + CLOSE_HEADROOM + ACTOR_FENCE_HEADROOM + 32 * 1024 > reservation_bytes,
            "fixture did not reach the actor-fence admission boundary"
        );
        restart_head = session.head.entry_sha256.clone();
        capability = session.capability;
    }

    let reopened_store = DurableSessionStoreV2::open(&state_root, signer).unwrap();
    let restarted = runtime(reopened_store.clone(), state_quotas);
    let HostedResponseV2::Status { session, .. } = session_status(&restarted, &capability) else {
        panic!("wrong status response")
    };
    assert_eq!(session.status, SessionStatusV2::RecoveryRequired);
    assert_ne!(session.journal_head_sha256, restart_head);
    assert!(!restarted
        .unreadable_sessions()
        .unwrap()
        .iter()
        .any(|message| message.contains(&capability.session_id)));
    let journal = reopened_store.read_journal(&capability.session_id).unwrap();
    assert!(matches!(
        journal.entries.last().unwrap().entry.event,
        JournalEventV2::ActorStateLost { .. }
    ));
    let closed = restarted
        .close_session(
            &principal_digest('a'),
            SessionMutationRequestV2 {
                credentials: capability.into(),
                client_request_id: "close-near-full".to_owned(),
                client_sequence: 2,
            },
        )
        .unwrap();
    assert!(matches!(closed, HostedResponseV2::Committed { .. }));
}

#[test]
fn checkpoint_restore_accepts_current_python_and_sql_codecs_and_isolates_mismatches() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let signer = HostedNodeSignerV2::generate().unwrap();
    let state_quotas = quotas(8 * 1024 * 1024);
    let store = DurableSessionStoreV2::open(&state_root, signer.clone()).unwrap();
    let cases = [
        ("python-good", "python", "ostadix.python-graph/v1", false),
        ("sql-good", "sql", "ostadix.sqlite-cli-main/v1", false),
        (
            "python-bad-codec",
            "python",
            "ostadix.python-wrong/v1",
            false,
        ),
        (
            "sql-bad-implementation",
            "sql",
            "ostadix.sqlite-cli-main/v1",
            true,
        ),
    ];
    let mut capabilities = Vec::new();
    for (label, backend, codec, mismatched_implementation) in cases {
        let mut session = install_open_session(
            &store,
            &signer,
            label,
            SessionStateTierV2::CheckpointRestore,
            StateReservationV2::new(1, 4 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        );
        let operation_sha256 =
            install_started_actor_operation(&store, &signer, &mut session, label);
        let launch = if mismatched_implementation {
            digest(&format!("wrong-launch:{label}")).to_string()
        } else {
            session.actor_generation.launch_context().to_string()
        };
        let snapshot = checkpoint_snapshot(backend, codec, &session.actor_generation, &launch);
        install_checkpoint(
            &store,
            &signer,
            &mut session,
            &snapshot,
            state_quotas.max_snapshot_bytes_per_actor(),
        );
        install_operation_terminal(
            &store,
            &signer,
            &mut session,
            label,
            operation_sha256,
            "settled".to_owned(),
        );
        capabilities.push((label, session.capability, session.head.entry_sha256.clone()));
    }
    let running = runtime(store, state_quotas);
    for (label, capability, restart_head) in capabilities {
        let HostedResponseV2::Status { session, .. } = session_status(&running, &capability) else {
            panic!("wrong status response")
        };
        assert_eq!(
            session.status,
            SessionStatusV2::RecoveryRequired,
            "case {label}"
        );
        if label.ends_with("good") {
            assert_ne!(session.journal_head_sha256, restart_head, "case {label}");
            assert!(session.actor.actor_id.is_none());
        } else {
            assert_eq!(session.journal_head_sha256, restart_head, "case {label}");
        }
    }
    let isolated = running.unreadable_sessions().unwrap().join("\n");
    assert!(isolated.contains("python-wrong"), "{isolated}");
    assert!(
        isolated.contains("exact session implementation"),
        "{isolated}"
    );
}

#[test]
fn refused_open_nonce_remains_consumed_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("state");
    let node_signer = HostedNodeSignerV2::generate().unwrap();
    let placement_signer = PlacementLeaseSignerV2::generate().unwrap();
    let state_quotas = quotas_with_sessions(1, 8 * 1024 * 1024);
    let reservation = StateReservationV2::new(1, 0, 8 * 1024 * 1024).unwrap();
    let principal = principal_digest('b');
    let refused_request;
    {
        let running = authorized_runtime(
            DurableSessionStoreV2::open(&state_root, node_signer.clone()).unwrap(),
            state_quotas.clone(),
            &placement_signer,
        );
        let first = signed_open_request(
            &placement_signer,
            &principal,
            "open-capacity-owner",
            state_quotas.clone(),
            reservation.clone(),
        );
        assert!(matches!(
            running.open_session(&principal, first).unwrap(),
            HostedResponseV2::SessionOpened { .. }
        ));
        refused_request = signed_open_request(
            &placement_signer,
            &principal,
            "open-refused-replay",
            state_quotas.clone(),
            reservation,
        );
        let error = running
            .open_session(&principal, refused_request.clone())
            .unwrap_err();
        assert!(format!("{error:#}").contains("quota"), "{error:#}");
    }

    let restarted = authorized_runtime(
        DurableSessionStoreV2::open(&state_root, node_signer).unwrap(),
        state_quotas,
        &placement_signer,
    );
    let error = restarted
        .open_session(&principal, refused_request)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("already consumed"),
        "{error:#}"
    );
}
