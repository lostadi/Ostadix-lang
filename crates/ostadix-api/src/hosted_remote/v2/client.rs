use std::time::Duration;

use anyhow::{bail, Context, Result};
use thiserror::Error;

use crate::placement::{
    ActorGenerationIdV1, CanonicalPlacementRecordV1, EnvironmentRequirementV1, RequirementAtomV1,
};

use super::super::protocol::{canonical_hosted_sha256, read_hosted_frame, write_hosted_frame};
use super::super::tls::{
    connect_mutual_tls_v2, ClientTlsIdentity, DEFAULT_CONNECT_TIMEOUT, DEFAULT_IO_TIMEOUT,
};
use super::crypto::{
    constant_time_eq, decode_fixed_hex, salted_bearer_hash, verify_placement_lease_signature_v2,
};
use super::protocol::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedV2ClientFailureDisposition {
    /// No bytes from this attempt were written. Attempt-local resources may be
    /// cleaned up, but durable retry material can still belong to an earlier
    /// ambiguous attempt and must remain caller-controlled.
    PreSend,
    /// A mutually authenticated node returned an explicit but unsigned
    /// protocol refusal. This is not proof that an Open did not commit.
    ServerRejected,
    /// Request bytes may have committed; preserve all retry material.
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct HostedV2ClientRequestError {
    disposition: HostedV2ClientFailureDisposition,
    code: Option<String>,
    message: String,
}

impl HostedV2ClientRequestError {
    pub fn disposition(&self) -> HostedV2ClientFailureDisposition {
        self.disposition
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
}

pub fn hosted_v2_client_failure_disposition(
    error: &anyhow::Error,
) -> Option<HostedV2ClientFailureDisposition> {
    error
        .downcast_ref::<HostedV2ClientRequestError>()
        .map(HostedV2ClientRequestError::disposition)
}

fn client_failure(
    disposition: HostedV2ClientFailureDisposition,
    code: Option<String>,
    message: impl Into<String>,
) -> anyhow::Error {
    HostedV2ClientRequestError {
        disposition,
        code,
        message: message.into(),
    }
    .into()
}

#[derive(Debug, Clone)]
pub struct HostedNodeClientV2 {
    pub address: String,
    pub tls_identity: ClientTlsIdentity,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub expected_node_public_key: [u8; 32],
}

impl HostedNodeClientV2 {
    pub fn new(
        address: impl Into<String>,
        tls_identity: ClientTlsIdentity,
        expected_node_public_key: [u8; 32],
    ) -> Self {
        Self {
            address: address.into(),
            tls_identity,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            io_timeout: DEFAULT_IO_TIMEOUT,
            expected_node_public_key,
        }
    }

    pub fn open_session(&self, request: OpenSessionRequestV2) -> Result<HostedResponseV2> {
        self.request(HostedRequestV2::OpenSession {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request,
        })
    }

    pub fn submit_operation(&self, request: SubmitOperationRequestV2) -> Result<HostedResponseV2> {
        self.request(HostedRequestV2::SubmitOperation {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request,
        })
    }

    pub fn status(&self, query: SessionQueryV2) -> Result<HostedResponseV2> {
        self.request(HostedRequestV2::Status {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            query,
        })
    }

    pub fn actors(&self, query: SessionQueryV2) -> Result<HostedResponseV2> {
        self.request(HostedRequestV2::Actors {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            query,
        })
    }

    pub fn reset_session(&self, request: SessionMutationRequestV2) -> Result<HostedResponseV2> {
        self.request(HostedRequestV2::ResetSession {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request,
        })
    }

    pub fn recover_session(&self, request: RecoverSessionRequestV2) -> Result<HostedResponseV2> {
        self.request(HostedRequestV2::RecoverSession {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request,
        })
    }

    pub fn close_session(&self, request: SessionMutationRequestV2) -> Result<HostedResponseV2> {
        self.request(HostedRequestV2::CloseSession {
            protocol: HOSTED_PROTOCOL_V2.to_owned(),
            request,
        })
    }

    pub fn request(&self, request: HostedRequestV2) -> Result<HostedResponseV2> {
        request.validate().map_err(|error| {
            client_failure(
                HostedV2ClientFailureDisposition::PreSend,
                None,
                format!("invalid hosted V2 request before send: {error:#}"),
            )
        })?;
        let mut stream = connect_mutual_tls_v2(
            &self.address,
            &self.tls_identity,
            self.connect_timeout,
            self.io_timeout,
        )
        .map_err(|error| {
            client_failure(
                HostedV2ClientFailureDisposition::PreSend,
                None,
                format!(
                    "failed to connect to hosted V2 node `{}` before sending request: {error:#}",
                    self.address
                ),
            )
        })?;
        write_hosted_frame(&mut stream, &request).map_err(|error| {
            client_failure(
                HostedV2ClientFailureDisposition::Ambiguous,
                None,
                format!(
                    "hosted V2 request write to `{}` may have been partially delivered: {error:#}",
                    self.address
                ),
            )
        })?;
        let response = read_hosted_frame(&mut stream)
            .map_err(|error| {
                client_failure(
                    HostedV2ClientFailureDisposition::Ambiguous,
                    None,
                    format!(
                        "hosted V2 node `{}` did not return a valid response after request delivery: {error:#}",
                        self.address
                    ),
                )
            })?
            .ok_or_else(|| {
                client_failure(
                    HostedV2ClientFailureDisposition::Ambiguous,
                    None,
                    format!(
                        "hosted V2 node `{}` closed after request delivery and before returning a response",
                        self.address
                    ),
                )
            })?;
        validate_hosted_response_v2(&request, &response, &self.expected_node_public_key).map_err(
            |error| {
                client_failure(
                    HostedV2ClientFailureDisposition::Ambiguous,
                    None,
                    format!(
                        "hosted V2 response could not be correlated to the delivered request: {error:#}"
                    ),
                )
            },
        )?;
        if let HostedResponseV2::Error { error } = &response {
            return Err(client_failure(
                HostedV2ClientFailureDisposition::ServerRejected,
                Some(error.code.clone()),
                format!(
                    "node rejected hosted V2 request [{}]{}: {}",
                    error.code,
                    if error.retryable { " (retryable)" } else { "" },
                    error.message
                ),
            ));
        }
        Ok(response)
    }
}

/// Verify both the node signature and the response-to-request correlation.
/// Status and Actors deliberately validate only the exact session/head pair;
/// their remaining view fields are TLS-protected convenience projections, not
/// claims contained in the signed journal receipt.
pub fn validate_hosted_response_v2(
    request: &HostedRequestV2,
    response: &HostedResponseV2,
    expected_node_public_key: &[u8; 32],
) -> Result<()> {
    request.validate()?;
    match (request, response) {
        (_, HostedResponseV2::Error { .. }) => Ok(()),
        (
            HostedRequestV2::OpenSession { request, .. },
            HostedResponseV2::SessionOpened {
                capability,
                receipt,
            },
        ) => {
            verify_response_receipt(receipt, expected_node_public_key)?;
            validate_open_receipt(request, capability, receipt)
        }
        (
            HostedRequestV2::SubmitOperation { request, .. },
            HostedResponseV2::Committed { receipt },
        ) => {
            verify_response_receipt(receipt, expected_node_public_key)?;
            validate_submit_receipt(request, receipt)
        }
        (
            HostedRequestV2::ResetSession { request, .. },
            HostedResponseV2::Committed { receipt },
        ) => {
            verify_response_receipt(receipt, expected_node_public_key)?;
            validate_simple_mutation_receipt(request, receipt, "reset")
        }
        (
            HostedRequestV2::RecoverSession { request, .. },
            HostedResponseV2::Committed { receipt },
        ) => {
            verify_response_receipt(receipt, expected_node_public_key)?;
            validate_recovery_receipt(request, receipt)
        }
        (
            HostedRequestV2::CloseSession { request, .. },
            HostedResponseV2::Committed { receipt },
        ) => {
            verify_response_receipt(receipt, expected_node_public_key)?;
            validate_simple_mutation_receipt(request, receipt, "close")
        }
        (
            HostedRequestV2::Status { query, .. },
            HostedResponseV2::Status {
                session,
                head_receipt,
            },
        ) => {
            verify_response_receipt(head_receipt, expected_node_public_key)?;
            if session.schema != HOSTED_SESSION_SCHEMA_V2
                || session.session_id != query.credentials.session_id
                || head_receipt.entry.session_id != query.credentials.session_id
                || session.journal_head_sha256 != head_receipt.entry_sha256
            {
                bail!(
                    "status response does not bind the requested session and signed journal head"
                );
            }
            if let Some(operation_id) = &query.operation_id {
                if session.operations.len() != 1 || !session.operations.contains_key(operation_id) {
                    bail!("status response does not contain only the requested operation");
                }
            }
            Ok(())
        }
        (
            HostedRequestV2::Actors { query, .. },
            HostedResponseV2::Actors {
                session_id,
                journal_head_sha256,
                head_receipt,
                ..
            },
        ) => {
            verify_response_receipt(head_receipt, expected_node_public_key)?;
            if session_id != &query.credentials.session_id
                || head_receipt.entry.session_id != query.credentials.session_id
                || journal_head_sha256 != &head_receipt.entry_sha256
            {
                bail!(
                    "actors response does not bind the requested session and signed journal head"
                );
            }
            Ok(())
        }
        _ => bail!("hosted V2 node returned a response kind that does not match the request"),
    }
}

fn verify_response_receipt(
    receipt: &SignedJournalEntryV2,
    expected_node_public_key: &[u8; 32],
) -> Result<()> {
    receipt.verify()?;
    let actual = hex::decode(&receipt.signer_public_key)
        .context("hosted receipt signer key is not hexadecimal")?;
    if actual.as_slice() != expected_node_public_key {
        bail!("hosted receipt was not signed by the pinned node identity");
    }
    Ok(())
}

fn validate_open_receipt(
    request: &OpenSessionRequestV2,
    capability: &SessionCapabilityV2,
    receipt: &SignedJournalEntryV2,
) -> Result<()> {
    request.validate()?;
    if capability != &request.proposed_capability
        || receipt.entry.session_id != request.proposed_capability.session_id
        || receipt.entry.sequence != 1
        || receipt.entry.previous_entry_sha256.is_some()
    {
        bail!("SessionOpened response differs from the proposed capability or first journal coordinate");
    }
    let command = &request.placement_lease.command;
    let lease_sha256 = request
        .placement_lease
        .authority
        .semantic_digest()?
        .to_string();
    let lease_nonce = request.placement_lease.authority.lease_nonce().to_string();
    let expected_identity = placement_identity_from_evidence(&request.placement_lease.evidence)?;
    let expected_request_sha256 = canonical_hosted_sha256(request)?;
    let JournalEventV2::SessionOpened {
        request_sha256,
        principal_sha256,
        bearer_salt,
        bearer_hash,
        capability_commitment,
        state_tier,
        state_session,
        state_quota_generation,
        state_quota_limits,
        state_reservation,
        placement_identity,
        placement_lease_sha256,
        placement_lease_nonce,
        client_request_id,
    } = &receipt.entry.event
    else {
        bail!("SessionOpened response receipt carries a different journal event");
    };
    if request_sha256 != &expected_request_sha256
        || principal_sha256 != &command.principal_sha256
        || client_request_id != &request.client_request_id
        || state_tier != &request.state_tier
        || state_session != &command.state_session
        || state_quota_generation != &command.state_quota_generation
        || state_quota_limits != &command.state_quota_limits
        || state_reservation != &command.state_reservation
        || placement_identity != &expected_identity
        || placement_lease_sha256 != &lease_sha256
        || placement_lease_nonce != &lease_nonce
        || capability_commitment != &request.capability_commitment
    {
        bail!("SessionOpened receipt differs from the exact signed open request");
    }
    let salt = decode_fixed_hex::<32>("SessionOpened bearer salt", bearer_salt)?;
    let bearer = decode_fixed_hex::<32>("proposed session bearer", &capability.bearer)?;
    let expected_hash = salted_bearer_hash(&salt, &bearer);
    if !constant_time_eq(expected_hash.as_bytes(), bearer_hash.as_bytes()) {
        bail!("SessionOpened receipt does not commit the proposed bearer");
    }
    Ok(())
}

fn validate_submit_receipt(
    request: &SubmitOperationRequestV2,
    receipt: &SignedJournalEntryV2,
) -> Result<()> {
    let request_sha256 = canonical_hosted_sha256(request)?;
    let operation_sha256 = request.operation.sha256()?;
    let lease_sha256 = request
        .placement_lease
        .authority
        .semantic_digest()?
        .to_string();
    let lease_nonce = request.placement_lease.authority.lease_nonce().to_string();
    let JournalEventV2::OperationAccepted {
        client_sequence,
        client_request_id,
        request_sha256: committed_request,
        operation_id,
        task_attempt,
        operation_sha256: committed_operation,
        source_sha256,
        actor_id,
        actor_generation,
        placement_lease_sha256,
        placement_lease_nonce,
        ..
    } = &receipt.entry.event
    else {
        bail!("execute commit response carries a different journal event");
    };
    if receipt.entry.session_id != request.credentials.session_id
        || client_sequence != &request.client_sequence
        || client_request_id != &request.client_request_id
        || committed_request != &request_sha256
        || operation_id != &request.operation.operation_id
        || task_attempt != &request.operation.task_attempt
        || committed_operation != &operation_sha256
        || source_sha256 != &request.operation.source_sha256
        || placement_lease_sha256 != &lease_sha256
        || placement_lease_nonce != &lease_nonce
    {
        bail!("execute commit receipt differs from the exact submitted request");
    }
    validate_submit_actor_receipt(request, actor_generation)?;
    match (actor_id, actor_generation) {
        (None, None) => {}
        (Some(actor_id), Some(_)) => validate_identifier_v2("receipt actor_id", actor_id)?,
        _ => bail!("execute commit receipt has inconsistent actor identity fields"),
    }
    Ok(())
}

fn validate_submit_actor_receipt(
    request: &SubmitOperationRequestV2,
    receipt_actor: &Option<ActorGenerationIdV1>,
) -> Result<()> {
    let envelope = &request.placement_lease;
    verify_placement_lease_signature_v2(envelope)
        .context("execute response request carries an invalid placement signature")?;
    envelope.command.validate()?;
    envelope.evidence.validate_shape()?;
    let operation_sha256 = request.operation.sha256()?;
    if envelope.command.purpose != PlacementPurposeV2::Execute
        || envelope.command.client_request_id != request.client_request_id
        || envelope.command.client_sequence != request.client_sequence
        || envelope.command.operation_sha256.as_deref() != Some(operation_sha256.as_str())
        || envelope
            .command
            .state_session
            .semantic_digest()?
            .as_sha256()
            != request.credentials.session_id
    {
        bail!("execute placement command differs from the exact submitted request");
    }
    let command_actor = envelope.command.actor_generation.as_ref();
    match (
        envelope.command.session_state_tier,
        command_actor,
        receipt_actor.as_ref(),
    ) {
        (SessionStateTierV2::Stateless, None, None) => Ok(()),
        (SessionStateTierV2::Stateless, _, _) => {
            bail!("stateless Execute receipt unexpectedly establishes an actor")
        }
        (_, Some(expected), Some(actual)) if expected == actual => Ok(()),
        (_, Some(_), _) => {
            bail!("Execute receipt does not bind the exact established actor generation")
        }
        (_, None, None) => {
            bail!("first stateful Execute receipt omits the node-established actor generation")
        }
        (_, None, Some(actor)) => validate_first_stateful_receipt_actor(envelope, actor),
    }
}

fn validate_first_stateful_receipt_actor(
    envelope: &SignedPlacementLeaseV2,
    actor: &ActorGenerationIdV1,
) -> Result<()> {
    let mut logical_environment = None;
    for requirement in envelope.evidence.requirement_footprint.require_complete()? {
        if let RequirementAtomV1::Environment(EnvironmentRequirementV1::SameLogicalEnvironment {
            identity,
        }) = requirement
        {
            if logical_environment.replace(identity).is_some() {
                bail!("first stateful Execute footprint carries multiple logical environments");
            }
        }
    }
    let logical_environment = logical_environment
        .context("first stateful Execute footprint omits its logical environment")?;
    if actor.logical_environment() != logical_environment {
        bail!("node-established actor differs from the signed logical environment");
    }

    let target_descriptor = envelope.evidence.node_profile.descriptor_digest()?;
    if actor.target_descriptor() != &target_descriptor {
        bail!("node-established actor differs from the signed target descriptor");
    }
    let selected_backend = envelope
        .evidence
        .warrant_discharge
        .exact_scope()
        .backend_implementation()
        .context("first stateful Execute warrant omits its backend implementation")?;
    if actor.backend_implementation() != selected_backend {
        bail!("node-established actor differs from the signed backend implementation");
    }
    if !envelope
        .evidence
        .node_profile
        .descriptor()
        .backend_implementations()
        .iter()
        .any(|implementation| {
            implementation
                .semantic_digest()
                .is_ok_and(|digest| &digest == selected_backend)
        })
    {
        bail!("signed node profile omits the node-established actor backend");
    }

    // Sandbox, launch context, and the initial physical generation are node
    // establishment facts attested by this pinned-node receipt. Every later
    // command carries the complete ActorGenerationIdV1 and is checked exactly.
    actor.semantic_digest()?;
    Ok(())
}

fn validate_simple_mutation_receipt(
    request: &SessionMutationRequestV2,
    receipt: &SignedJournalEntryV2,
    expected_kind: &str,
) -> Result<()> {
    let request_sha256 = canonical_hosted_sha256(request)?;
    let coordinates = receipt.entry.event.client_commit();
    if receipt.entry.session_id != request.credentials.session_id
        || coordinates
            != Some((
                request.client_sequence,
                request.client_request_id.as_str(),
                request_sha256.as_str(),
            ))
        || (expected_kind == "reset"
            && !matches!(receipt.entry.event, JournalEventV2::SessionReset { .. }))
        || (expected_kind == "close"
            && !matches!(receipt.entry.event, JournalEventV2::SessionClosed { .. }))
    {
        bail!("{expected_kind} commit receipt differs from the exact mutation request");
    }
    Ok(())
}

fn validate_recovery_receipt(
    request: &RecoverSessionRequestV2,
    receipt: &SignedJournalEntryV2,
) -> Result<()> {
    let request_sha256 = canonical_hosted_sha256(request)?;
    let warrant_sha256 = request.warrant.sha256()?;
    let lease_sha256 = request
        .placement_lease
        .authority
        .semantic_digest()?
        .to_string();
    let lease_nonce = request.placement_lease.authority.lease_nonce().to_string();
    let common_matches = |client_sequence: &u64,
                          client_request_id: &String,
                          committed_request: &String,
                          committed_warrant: &String,
                          committed_lease: &String,
                          committed_nonce: &String| {
        *client_sequence == request.client_sequence
            && client_request_id == &request.client_request_id
            && committed_request == &request_sha256
            && committed_warrant == &warrant_sha256
            && committed_lease == &lease_sha256
            && committed_nonce == &lease_nonce
    };
    let matches = match &receipt.entry.event {
        JournalEventV2::RecoveryCommitted {
            client_sequence,
            client_request_id,
            request_sha256,
            warrant_sha256,
            placement_lease_sha256,
            placement_lease_nonce,
            recovery_attempt_sha256,
            trigger,
            previous_actor_generation,
            actor_generation,
            actor_id,
            checkpoint_sha256,
            checkpoint_bytes,
            ..
        } => {
            let common = common_matches(
                client_sequence,
                client_request_id,
                request_sha256,
                warrant_sha256,
                placement_lease_sha256,
                placement_lease_nonce,
            ) && trigger == &request.warrant.trigger
                && Some(previous_actor_generation)
                    == request.placement_lease.command.actor_generation.as_ref();
            if common {
                validate_sha256_v2("recovery_attempt_sha256", recovery_attempt_sha256)?;
                if receipt.entry.previous_entry_sha256.as_ref() != Some(recovery_attempt_sha256) {
                    bail!("recovery commit does not follow its signed attempt allocation");
                }
                validate_recovery_transition(
                    previous_actor_generation,
                    actor_generation,
                    actor_id,
                    checkpoint_sha256,
                    checkpoint_bytes,
                )?;
                validate_recovery_trigger_receipt(
                    trigger,
                    previous_actor_generation,
                    checkpoint_sha256,
                    checkpoint_bytes,
                )?;
            }
            common
        }
        JournalEventV2::RecoveryRefused {
            client_sequence,
            client_request_id,
            request_sha256,
            warrant_sha256,
            placement_lease_sha256,
            placement_lease_nonce,
            recovery_attempt_sha256,
            attempted_actor_generation,
            ..
        } => {
            let common = common_matches(
                client_sequence,
                client_request_id,
                request_sha256,
                warrant_sha256,
                placement_lease_sha256,
                placement_lease_nonce,
            );
            if common {
                match (recovery_attempt_sha256, attempted_actor_generation) {
                    (Some(attempt_sha256), Some(attempted)) => {
                        validate_sha256_v2("recovery_attempt_sha256", attempt_sha256)?;
                        if receipt.entry.previous_entry_sha256.as_ref() != Some(attempt_sha256) {
                            bail!("recovery refusal does not follow its signed attempt allocation");
                        }
                        let previous = request
                            .placement_lease
                            .command
                            .actor_generation
                            .as_ref()
                            .context("spawned recovery refusal has no prior actor generation")?;
                        validate_actor_generation_successor(previous, attempted)?;
                    }
                    (None, None) => {
                        if receipt.entry.previous_entry_sha256.as_deref()
                            != Some(request.warrant.evidence_sha256.as_str())
                        {
                            bail!("pre-attempt recovery refusal does not follow the warranted journal head");
                        }
                    }
                    _ => bail!(
                        "recovery refusal must carry both attempt hash and attempted generation or neither"
                    ),
                }
            }
            common
        }
        _ => false,
    };
    if receipt.entry.session_id != request.credentials.session_id || !matches {
        bail!("recovery commit receipt differs from the exact recovery request");
    }
    Ok(())
}

fn validate_recovery_trigger_receipt(
    trigger: &RecoveryTriggerV2,
    previous_actor_generation: &ActorGenerationIdV1,
    checkpoint_sha256: &Option<String>,
    checkpoint_bytes: &Option<u64>,
) -> Result<()> {
    if let RecoveryTriggerV2::ActorLost {
        previous_actor_generation: trigger_previous,
        checkpoint_sha256: trigger_checkpoint,
        checkpoint_bytes: trigger_bytes,
        recovery_required_head_sha256: _,
    } = trigger
    {
        if trigger_previous != previous_actor_generation
            || checkpoint_sha256.as_ref() != Some(trigger_checkpoint)
            || *checkpoint_bytes != Some(*trigger_bytes)
        {
            bail!("actor-loss recovery receipt differs from the warrant's exact fenced checkpoint");
        }
    }
    Ok(())
}

fn validate_recovery_transition(
    previous: &ActorGenerationIdV1,
    recovered: &ActorGenerationIdV1,
    actor_id: &str,
    checkpoint_sha256: &Option<String>,
    checkpoint_bytes: &Option<u64>,
) -> Result<()> {
    validate_identifier_v2("recovery receipt actor_id", actor_id)?;
    validate_actor_generation_successor(previous, recovered)?;
    let (Some(checkpoint_sha256), Some(checkpoint_bytes)) =
        (checkpoint_sha256.as_deref(), *checkpoint_bytes)
    else {
        bail!("recovery receipt omits its acknowledged checkpoint");
    };
    validate_sha256_v2("recovery receipt checkpoint_sha256", checkpoint_sha256)?;
    if checkpoint_bytes == 0 {
        bail!("recovery receipt carries an empty acknowledged checkpoint");
    }
    Ok(())
}

fn validate_actor_generation_successor(
    previous: &ActorGenerationIdV1,
    recovered: &ActorGenerationIdV1,
) -> Result<()> {
    let expected_generation = previous
        .generation()
        .get()
        .checked_add(1)
        .context("recovery actor generation overflow")?;
    if recovered.logical_environment() != previous.logical_environment()
        || recovered.backend_implementation() != previous.backend_implementation()
        || recovered.target_descriptor() != previous.target_descriptor()
        || recovered.sandbox_policy() != previous.sandbox_policy()
        || recovered.launch_context() != previous.launch_context()
        || recovered.generation().get() != expected_generation
    {
        bail!("recovery receipt carries an invalid actor-generation transition");
    }
    Ok(())
}

fn placement_identity_from_evidence(
    evidence: &HostedPlacementEvidenceV2,
) -> Result<HostedPlacementIdentityV2> {
    let scope = evidence.warrant_discharge.exact_scope();
    Ok(HostedPlacementIdentityV2 {
        target_descriptor: evidence.node_profile.descriptor_digest()?,
        requirement_footprint: evidence.requirement_footprint.semantic_digest()?,
        backend_implementation: scope
            .backend_implementation()
            .context("open placement evidence omits exact backend implementation")?
            .clone(),
        realization_pipeline: scope
            .realization_pipeline()
            .context("open placement evidence omits exact realization pipeline")?
            .clone(),
        trust_policy: evidence.trust_policy.semantic_digest()?,
        reservation: evidence.reservation.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::{GenerationV1, SemanticDigestV1};

    fn digest(label: &str) -> SemanticDigestV1 {
        SemanticDigestV1::hash_bytes("ostadix/hosted/client-recovery-test/v2", label.as_bytes())
    }

    fn actor(generation: u64) -> ActorGenerationIdV1 {
        ActorGenerationIdV1::new(
            digest("logical-environment"),
            digest("backend-implementation"),
            digest("target-descriptor"),
            digest("sandbox-policy"),
            digest("launch-context"),
            GenerationV1::new(generation).unwrap(),
        )
    }

    #[test]
    fn recovery_receipt_requires_one_exact_physical_generation_advance() {
        let previous = actor(7);
        let recovered = actor(8);
        let checkpoint = Some("ab".repeat(32));
        let bytes = Some(1_u64);

        validate_recovery_transition(&previous, &recovered, "restored-actor", &checkpoint, &bytes)
            .unwrap();

        assert!(validate_recovery_transition(
            &previous,
            &actor(9),
            "restored-actor",
            &checkpoint,
            &bytes,
        )
        .is_err());
        let wrong_target = ActorGenerationIdV1::new(
            previous.logical_environment().clone(),
            previous.backend_implementation().clone(),
            digest("wrong-target"),
            previous.sandbox_policy().clone(),
            previous.launch_context().clone(),
            GenerationV1::new(8).unwrap(),
        );
        assert!(validate_recovery_transition(
            &previous,
            &wrong_target,
            "restored-actor",
            &checkpoint,
            &bytes,
        )
        .is_err());
        assert!(validate_recovery_transition(
            &previous,
            &recovered,
            "restored-actor",
            &None,
            &None,
        )
        .is_err());
    }
}
