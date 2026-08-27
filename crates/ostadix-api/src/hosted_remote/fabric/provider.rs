//! Authenticated, replay-fenced provider for the single Fabric V1 profile.
//!
//! This layer owns request authorization and node-local attempt lifecycle. It
//! can return only provisional M2 candidate bytes. It has no HGraph identity
//! and no publication, settlement, retry, effect, or fallback authority.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::execution_fabric_authority::{
    validate_node_id, ExecutionCellIncarnationV1, FabricAbandonmentV1, FabricAttemptQueryV1,
    FabricAttemptStatusV1, FabricRejectionV1, FabricRequestV1, FabricResponseV1,
    FabricSigningKeyV1, FabricSubmissionV1, TrustedFabricAuthoritiesV1,
};
use crate::placement_protocol::{GenerationV1, SemanticDigestV1, UnixMillisV1};

use super::super::tls::{HostedServerStream, DEFAULT_IO_TIMEOUT};
use super::ledger::{
    FabricAttemptBindingV1, FabricAttemptLedgerV1, FabricLedgerConflictKindV1,
    FabricLedgerCurrentResponseV1, FabricLedgerEntryV1, FabricLedgerQueryOutcomeV1,
};
use super::realizer::TrustedInlineRealizerV1;
use super::wire::{
    read_fabric_server_request_v1, write_fabric_server_encoded_response_parts_v1,
    write_fabric_server_response_v1,
};

const MAX_PROVIDER_REASON_MESSAGE_BYTES_V1: usize = 768;

#[derive(Clone, Debug)]
pub struct FabricAttemptProviderConfigV1 {
    pub state_base: PathBuf,
    pub node_id: String,
    pub node_generation: GenerationV1,
    pub node_signer: FabricSigningKeyV1,
    pub trusted_authorities: TrustedFabricAuthoritiesV1,
}

#[derive(Clone)]
pub struct FabricAttemptProviderV1 {
    node_id: String,
    node_generation: GenerationV1,
    execution_cell_incarnation: ExecutionCellIncarnationV1,
    node_signer: FabricSigningKeyV1,
    trusted_authorities: TrustedFabricAuthoritiesV1,
    ledger: FabricAttemptLedgerV1,
    realizer: TrustedInlineRealizerV1,
}

impl std::fmt::Debug for FabricAttemptProviderV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FabricAttemptProviderV1")
            .field("node_id", &self.node_id)
            .field("node_generation", &self.node_generation)
            .field(
                "execution_cell_incarnation",
                &self.execution_cell_incarnation,
            )
            .field("node_key_id", &self.node_signer.key_id_hex())
            .field("trusted_authority_count", &self.trusted_authorities.len())
            .field("ledger_root", &self.ledger.root())
            .finish_non_exhaustive()
    }
}

impl FabricAttemptProviderV1 {
    pub fn open(config: FabricAttemptProviderConfigV1) -> Result<Self> {
        validate_node_id(&config.node_id).map_err(anyhow::Error::new)?;
        if config.trusted_authorities.is_empty() {
            bail!("Fabric provider requires at least one explicitly trusted execution authority");
        }
        let ledger = FabricAttemptLedgerV1::open(&config.state_base).with_context(|| {
            format!(
                "failed to open Fabric provider state beneath `{}`",
                config.state_base.display()
            )
        })?;
        let execution_cell_incarnation = ledger.execution_cell_incarnation()?;
        Ok(Self {
            node_id: config.node_id,
            node_generation: config.node_generation,
            execution_cell_incarnation,
            node_signer: config.node_signer,
            trusted_authorities: config.trusted_authorities,
            ledger,
            realizer: TrustedInlineRealizerV1::new(),
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn node_generation(&self) -> GenerationV1 {
        self.node_generation
    }

    pub fn execution_cell_incarnation(&self) -> ExecutionCellIncarnationV1 {
        self.execution_cell_incarnation
    }

    pub fn node_public_key(&self) -> [u8; 32] {
        self.node_signer.public_key()
    }

    pub fn node_key_id(&self) -> String {
        self.node_signer.key_id_hex()
    }

    pub(crate) fn handle_request(
        &self,
        tls_peer_principal_sha256: &str,
        request: FabricRequestV1,
    ) -> Result<FabricProviderReplyV1> {
        let peer = SemanticDigestV1::from_sha256(tls_peer_principal_sha256.to_owned())
            .context("Fabric TLS peer principal is not a canonical SHA-256 identity")?;
        match request {
            FabricRequestV1::SubmitPureAttempt(submission) => {
                self.handle_submission(peer, submission)
            }
            FabricRequestV1::QueryAttempt(query) => self.handle_query(peer, query),
        }
    }

    fn handle_submission(
        &self,
        peer: SemanticDigestV1,
        submission: FabricSubmissionV1,
    ) -> Result<FabricProviderReplyV1> {
        let status = FabricAttemptStatusV1::from_submission(&submission);
        let signed_lease = submission.header().lease();
        if let Err(error) = submission.validate() {
            return rejection_reply(status, "submission-binding-rejected", error);
        }
        if let Err(error) = self
            .trusted_authorities
            .authenticate_execution_lease(signed_lease)
        {
            return rejection_reply(status, "execution-authority-rejected", error);
        }

        let query = FabricAttemptQueryV1::from_submission(&submission);
        let initial_binding = FabricAttemptBindingV1::from_submission(&submission)?;
        let mut durable_preaccept = false;
        match self.ledger.query(&query, &peer)? {
            FabricLedgerQueryOutcomeV1::Found(entry) => {
                if !matches!(
                    entry.current_response(),
                    FabricLedgerCurrentResponseV1::Received
                        | FabricLedgerCurrentResponseV1::Validated
                ) {
                    return reply_from_entry(&query, entry);
                }
                durable_preaccept = true;
            }
            FabricLedgerQueryOutcomeV1::Conflict(conflict) => {
                return rejection_reply(
                    status,
                    conflict_reason_code(conflict.kind()),
                    "submission conflicts with a durable Fabric attempt",
                )
            }
            FabricLedgerQueryOutcomeV1::Unknown => {}
        }

        if let Err(error) = signed_lease.lease().validate_at(unix_millis_now()?) {
            if durable_preaccept {
                return self.record_preaccept_rejection(
                    &query,
                    initial_binding,
                    "execution-authority-expired",
                    error,
                );
            }
            return rejection_reply(status, "execution-authority-expired", error);
        }
        if let Err(error) = self.validate_target(&peer, &submission) {
            if durable_preaccept {
                return self.record_preaccept_rejection(
                    &query,
                    initial_binding,
                    "target-binding-stale",
                    error,
                );
            }
            return rejection_reply(status, "target-binding-rejected", error);
        }

        let received = match self.ledger.record_received(initial_binding.clone()) {
            Ok(received) => received,
            Err(error) => match self.ledger.query(&query, &peer) {
                Ok(FabricLedgerQueryOutcomeV1::Found(entry)) => {
                    return reply_from_entry(&query, entry)
                }
                Ok(FabricLedgerQueryOutcomeV1::Conflict(conflict)) => {
                    return rejection_reply(
                        status,
                        conflict_reason_code(conflict.kind()),
                        "submission conflicts with a concurrently received Fabric attempt",
                    )
                }
                Ok(FabricLedgerQueryOutcomeV1::Unknown) | Err(_) => {
                    return Err(error).context("failed to durably record received Fabric attempt")
                }
            },
        };
        if !matches!(
            received.entry().current_response(),
            FabricLedgerCurrentResponseV1::Received | FabricLedgerCurrentResponseV1::Validated
        ) {
            return reply_from_entry(&query, received.entry().clone());
        }

        let prepared = match self.realizer.prepare(&submission) {
            Ok(prepared) => prepared,
            Err(error) => {
                return self.record_preaccept_rejection(
                    &query,
                    initial_binding,
                    "source-reconstruction-rejected",
                    error,
                )
            }
        };
        let retained_submission = prepared.submission().clone();
        let binding = FabricAttemptBindingV1::from_submission(prepared.submission())?;
        if binding != initial_binding {
            bail!("prepared Fabric submission changed its immutable acceptance binding");
        }
        drop(submission);

        let validated = self.ledger.record_validated(&binding)?;
        if !matches!(
            validated.entry().current_response(),
            FabricLedgerCurrentResponseV1::Validated
        ) {
            return reply_from_entry(&query, validated.entry().clone());
        }

        if let Err(error) = self
            .trusted_authorities
            .verify_execution_lease(retained_submission.header().lease(), unix_millis_now()?)
        {
            return self.record_preaccept_rejection(
                &query,
                binding,
                "execution-authority-expired",
                error,
            );
        }
        if let Err(error) = self.validate_target(&peer, &retained_submission) {
            return self.record_preaccept_rejection(&query, binding, "target-binding-stale", error);
        }

        let acceptance = match self.ledger.consume_and_accept(binding.clone()) {
            Ok(acceptance) => acceptance,
            Err(error) => {
                if let Ok(FabricLedgerQueryOutcomeV1::Conflict(conflict)) =
                    self.ledger.query(&query, &peer)
                {
                    return rejection_reply(
                        FabricAttemptStatusV1::from_submission(&retained_submission),
                        conflict_reason_code(conflict.kind()),
                        "submission conflicts with a concurrently accepted Fabric attempt",
                    );
                }
                return Err(error).context("failed to durably accept Fabric attempt");
            }
        };
        if !acceptance.may_execute() {
            return reply_from_entry(&query, acceptance.entry().clone());
        }

        let running = self.ledger.mark_running(&binding)?;
        if !running.was_applied() {
            return reply_from_entry(&query, running.entry().clone());
        }

        if let Err(error) = self
            .trusted_authorities
            .verify_execution_lease(retained_submission.header().lease(), unix_millis_now()?)
        {
            return self.record_execution_rejection(
                &query,
                binding,
                "execution-authority-expired-before-start",
                error,
            );
        }
        if let Err(error) = self.validate_target(&peer, &retained_submission) {
            return self.record_execution_rejection(
                &query,
                binding,
                "target-binding-stale-before-start",
                error,
            );
        }

        let started = Instant::now();
        let candidate_bytes = match self.realizer.realize(prepared) {
            Ok(candidate) => candidate,
            Err(error) => {
                return self.record_execution_rejection(
                    &query,
                    binding,
                    "realization-failed",
                    error,
                )
            }
        };
        let elapsed = started.elapsed();
        let maximum_runtime = Duration::from_millis(
            retained_submission
                .header()
                .lease()
                .lease()
                .maximum_runtime_ms(),
        );
        if elapsed > maximum_runtime {
            return self.record_execution_rejection(
                &query,
                binding,
                "runtime-budget-exceeded",
                format!("provider realization took {elapsed:?}, maximum is {maximum_runtime:?}"),
            );
        }
        let runtime_observation_ms = u64::try_from(elapsed.as_millis())
            .context("Fabric runtime observation exceeds u64 milliseconds")?;
        let terminal = match self.node_signer.sign_terminal_candidate(
            &retained_submission,
            candidate_bytes,
            runtime_observation_ms,
        ) {
            Ok(terminal) => terminal,
            Err(error) => {
                return self.record_execution_rejection(
                    &query,
                    binding,
                    "terminal-signing-failed",
                    error,
                )
            }
        };
        let stored = self.ledger.record_terminal_candidate(&binding, &terminal)?;
        reply_from_entry(&query, stored.entry().clone())
    }

    fn handle_query(
        &self,
        peer: SemanticDigestV1,
        query: FabricAttemptQueryV1,
    ) -> Result<FabricProviderReplyV1> {
        let current_status = FabricAttemptStatusV1::from_query(
            &query,
            self.node_id.clone(),
            self.node_generation,
            self.execution_cell_incarnation,
        )?;
        if !self
            .trusted_authorities
            .contains_key_id(query.issuer_key_id().as_sha256())
        {
            return rejection_reply(
                current_status,
                "unknown-execution-authority",
                "query names an issuer that this provider does not trust",
            );
        }
        match self.ledger.query(&query, &peer)? {
            FabricLedgerQueryOutcomeV1::Unknown => rejection_reply(
                current_status,
                "unknown-attempt",
                "no durable Fabric attempt matches this query",
            ),
            FabricLedgerQueryOutcomeV1::Conflict(conflict) => rejection_reply(
                current_status,
                conflict_reason_code(conflict.kind()),
                "query conflicts with a durable Fabric attempt binding",
            ),
            FabricLedgerQueryOutcomeV1::Found(entry) => reply_from_entry(&query, entry),
        }
    }

    fn validate_target(
        &self,
        peer: &SemanticDigestV1,
        submission: &FabricSubmissionV1,
    ) -> Result<()> {
        let target = submission.header().lease().lease().target();
        if target.tls_client_principal_sha256() != peer {
            bail!("execution lease targets a different TLS client principal");
        }
        if target.node_id() != self.node_id
            || target.node_generation() != self.node_generation
            || target.execution_cell_incarnation() != self.execution_cell_incarnation
        {
            bail!(
                "execution lease targets node {}/{}/{}, provider is {}/{}/{}",
                target.node_id(),
                target.node_generation().get(),
                target.execution_cell_incarnation().get(),
                self.node_id,
                self.node_generation.get(),
                self.execution_cell_incarnation.get()
            );
        }
        Ok(())
    }

    fn record_preaccept_rejection(
        &self,
        query: &FabricAttemptQueryV1,
        binding: FabricAttemptBindingV1,
        reason_code: &'static str,
        error: impl std::fmt::Display,
    ) -> Result<FabricProviderReplyV1> {
        let message = bounded_reason_message(error);
        let outcome = self
            .ledger
            .record_preaccept_rejected(binding, reason_code, message)?;
        reply_from_entry(query, outcome.entry().clone())
    }

    fn record_execution_rejection(
        &self,
        query: &FabricAttemptQueryV1,
        binding: FabricAttemptBindingV1,
        reason_code: &'static str,
        error: impl std::fmt::Display,
    ) -> Result<FabricProviderReplyV1> {
        let message = bounded_reason_message(error);
        let outcome = self.ledger.record_rejected(binding, reason_code, message)?;
        reply_from_entry(query, outcome.entry().clone())
    }
}

#[derive(Debug)]
pub(crate) enum FabricProviderReplyV1 {
    Response(FabricResponseV1),
    ExactTerminal {
        header_bytes: Vec<u8>,
        candidate_bytes: Vec<u8>,
    },
}

pub(crate) fn serve_fabric_stream_v1(
    stream: &mut HostedServerStream,
    provider: &FabricAttemptProviderV1,
    tls_peer_principal_sha256: &str,
) -> Result<()> {
    let request = read_fabric_server_request_v1(stream, DEFAULT_IO_TIMEOUT)?
        .context("authenticated Fabric client closed before sending one request")?;
    match provider.handle_request(tls_peer_principal_sha256, request)? {
        FabricProviderReplyV1::Response(response) => {
            write_fabric_server_response_v1(stream, &response, DEFAULT_IO_TIMEOUT)
        }
        FabricProviderReplyV1::ExactTerminal {
            header_bytes,
            candidate_bytes,
        } => write_fabric_server_encoded_response_parts_v1(
            stream,
            &header_bytes,
            Some(&candidate_bytes),
            DEFAULT_IO_TIMEOUT,
        ),
    }
}

fn reply_from_entry(
    query: &FabricAttemptQueryV1,
    entry: FabricLedgerEntryV1,
) -> Result<FabricProviderReplyV1> {
    let status = FabricAttemptStatusV1::from_query(
        query,
        entry.binding().node_id().to_owned(),
        entry.binding().node_generation(),
        entry.binding().execution_cell_incarnation(),
    )?;
    Ok(match entry.current_response() {
        FabricLedgerCurrentResponseV1::Received | FabricLedgerCurrentResponseV1::Validated => {
            FabricProviderReplyV1::Response(FabricResponseV1::Running(status))
        }
        FabricLedgerCurrentResponseV1::Accepted => {
            FabricProviderReplyV1::Response(FabricResponseV1::Accepted(status))
        }
        FabricLedgerCurrentResponseV1::Running => {
            FabricProviderReplyV1::Response(FabricResponseV1::Running(status))
        }
        FabricLedgerCurrentResponseV1::TerminalCandidate(terminal) => {
            FabricProviderReplyV1::ExactTerminal {
                header_bytes: terminal.header_bytes().to_vec(),
                candidate_bytes: terminal.candidate_bytes().to_vec(),
            }
        }
        FabricLedgerCurrentResponseV1::Rejected {
            reason_code,
            message,
        } => FabricProviderReplyV1::Response(FabricResponseV1::Rejected(
            FabricRejectionV1::new(status, reason_code, message).map_err(anyhow::Error::new)?,
        )),
        FabricLedgerCurrentResponseV1::Abandoned {
            reason_code,
            message,
        } => FabricProviderReplyV1::Response(FabricResponseV1::Abandoned(
            FabricAbandonmentV1::new(status, reason_code, message).map_err(anyhow::Error::new)?,
        )),
    })
}

fn rejection_reply(
    status: FabricAttemptStatusV1,
    reason_code: &'static str,
    message: impl std::fmt::Display,
) -> Result<FabricProviderReplyV1> {
    let rejection = FabricRejectionV1::new(status, reason_code, bounded_reason_message(message))
        .map_err(anyhow::Error::new)?;
    Ok(FabricProviderReplyV1::Response(FabricResponseV1::Rejected(
        rejection,
    )))
}

fn conflict_reason_code(kind: FabricLedgerConflictKindV1) -> &'static str {
    match kind {
        FabricLedgerConflictKindV1::NonceReused => "lease-nonce-reused",
        FabricLedgerConflictKindV1::AttemptRebound => "attempt-rebound",
        FabricLedgerConflictKindV1::QueryBindingMismatch => "submission-binding-mismatch",
        FabricLedgerConflictKindV1::TlsPrincipalMismatch => "tls-principal-mismatch",
    }
}

fn bounded_reason_message(message: impl std::fmt::Display) -> String {
    let raw = message.to_string();
    let mut bounded = String::new();
    for character in raw.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if bounded.len() + character.len_utf8() > MAX_PROVIDER_REASON_MESSAGE_BYTES_V1 {
            break;
        }
        bounded.push(character);
    }
    let normalized = bounded.trim();
    if normalized.is_empty() {
        "Fabric request rejected".to_owned()
    } else {
        normalized.to_owned()
    }
}

fn unix_millis_now() -> Result<UnixMillisV1> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("Fabric provider wall clock precedes the Unix epoch")?;
    Ok(UnixMillisV1::new(
        u64::try_from(elapsed.as_millis())
            .context("Fabric provider wall clock exceeds u64 milliseconds")?,
    ))
}
