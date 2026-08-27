//! Authenticated, replay-fenced provider for the single Fabric V1 profile.
//!
//! This layer owns request authorization and node-local attempt lifecycle. It
//! can return only provisional M2 candidate bytes. It has no HGraph identity
//! and no publication, settlement, retry, effect, or fallback authority.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;
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

#[cfg(test)]
type BeforeRealizeObserverV1 = Arc<dyn Fn() + Send + Sync>;

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
    #[cfg(test)]
    before_realize_observer: Option<BeforeRealizeObserverV1>,
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
            #[cfg(test)]
            before_realize_observer: None,
        })
    }

    #[cfg(test)]
    fn with_before_realize_observer(mut self, observer: BeforeRealizeObserverV1) -> Self {
        self.before_realize_observer = Some(observer);
        self
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

        #[cfg(test)]
        if let Some(observer) = &self.before_realize_observer {
            observer();
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;

    use crate::backend_catalog::BackendRegistry;
    use crate::evidence::ExecutionIntentV1;
    use crate::execution_contract::Policy;
    use crate::execution_fabric::{
        encode_execution_capsule_v1, AttemptIdV1, ExecutionCapsuleV1, ExecutionIdV1,
        ExecutionLimitsV1, InputManifestV1, LogicalTaskIdV1, OutputContractV1, RendererPartV1,
        Sha256DigestV1, SourceClosedRendererV1, TrustedInlineRendererV1,
    };
    use crate::execution_fabric_authority::{
        decode_fabric_response_v1, ExecutionCellIncarnationV1, FabricResponseV1,
        FabricSourceClosureV1, FabricTargetBindingV1, PlacementLeaseV3,
        FABRIC_SOURCE_CLOSURE_DIALECT_V1, FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
        MAX_FABRIC_LEASE_LIFETIME_MS,
    };
    use crate::hgraph::solve::solve_types;
    use crate::ir::{OIr, OIrProgram};
    use crate::parser::Parser;
    use crate::world::MAX_OVALUE_RECORD_BYTES;

    use super::super::profile::trusted_inline_fabric_profile_v1;
    use super::*;

    const NODE_ID: &str = "provider-proof-node";
    const NODE_GENERATION: u64 = 7;
    const RESULT_SLOT: &str = "result";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SemanticDriftV1 {
        None,
        LexicalSource,
        Intent,
        OperationOir,
        ExecutionPlan,
        BackendCatalog,
        BackendImplementation,
    }

    struct CompiledRendererV1 {
        source_utf8: String,
        intent_sha256: Sha256DigestV1,
        operation_oir_sha256: Sha256DigestV1,
        execution_plan_sha256: Sha256DigestV1,
        backend_catalog_sha256: Sha256DigestV1,
        backend_implementation_sha256: Sha256DigestV1,
        realization_pipeline_sha256: SemanticDigestV1,
        renderer: TrustedInlineRendererV1,
    }

    #[derive(Default)]
    struct ProbeStateV1 {
        entered: bool,
        released: bool,
    }

    fn digest(seed: u8) -> Sha256DigestV1 {
        [seed; 32]
    }

    fn semantic(seed: u8) -> SemanticDigestV1 {
        SemanticDigestV1::from_sha256(hex::encode(digest(seed))).unwrap()
    }

    fn decode_sha256(value: &str) -> Sha256DigestV1 {
        let mut digest = [0_u8; 32];
        hex::decode_to_slice(value, &mut digest).unwrap();
        digest
    }

    fn drifted(mut value: Sha256DigestV1) -> Sha256DigestV1 {
        value[0] ^= 0x80;
        if value.iter().all(|byte| *byte == 0) {
            value[0] = 1;
        }
        value
    }

    fn private_tempdir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        directory
    }

    fn compile_renderer(source_literal: &str) -> CompiledRendererV1 {
        let source_utf8 = format!("text^({source_literal})_text");
        let registry = BackendRegistry::global();
        let tags = registry.registered_backend_tags();
        let mut parser = Parser::new(&source_utf8, &tags);
        let parsed = parser.parse_with_origins().unwrap();
        let program = OIrProgram::lower(parsed.nodes());
        assert_eq!(program.nodes.len(), 1);
        let OIr::Exec { backend, .. } = &program.nodes[0] else {
            panic!("provider proof source did not lower to one Exec")
        };
        let profile = trusted_inline_fabric_profile_v1(backend).unwrap();
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).unwrap();
        solve_types(&mut graph).unwrap();
        let intent = ExecutionIntentV1::compile(
            source_utf8.as_bytes(),
            &program,
            &plan,
            &graph,
            Policy::Eager,
        )
        .unwrap();
        CompiledRendererV1 {
            source_utf8,
            intent_sha256: decode_sha256(&intent.execution_intent_sha256),
            operation_oir_sha256: decode_sha256(&intent.oir_sha256),
            execution_plan_sha256: decode_sha256(&intent.plan_sha256),
            backend_catalog_sha256: decode_sha256(&intent.backend_catalog_projection_sha256),
            backend_implementation_sha256: *profile.implementation_sha256(),
            realization_pipeline_sha256: profile.realization_pipeline_sha256().clone(),
            renderer: profile.renderer(),
        }
    }

    fn open_provider(
        state_base: &Path,
        authority: &FabricSigningKeyV1,
        node_signer: &FabricSigningKeyV1,
    ) -> FabricAttemptProviderV1 {
        let mut trusted_authorities = TrustedFabricAuthoritiesV1::new();
        trusted_authorities.enroll(authority.public_key());
        FabricAttemptProviderV1::open(FabricAttemptProviderConfigV1 {
            state_base: state_base.to_path_buf(),
            node_id: NODE_ID.to_owned(),
            node_generation: GenerationV1::new(NODE_GENERATION).unwrap(),
            node_signer: node_signer.clone(),
            trusted_authorities,
        })
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn signed_submission(
        authority: &FabricSigningKeyV1,
        peer: &SemanticDigestV1,
        incarnation: ExecutionCellIncarnationV1,
        task_seed: u8,
        attempt_generation: u64,
        nonce_seed: u8,
        retained_literal: &str,
        drift: SemanticDriftV1,
    ) -> FabricSubmissionV1 {
        let source_literal = if drift == SemanticDriftV1::LexicalSource {
            format!("{retained_literal}-different-source")
        } else {
            retained_literal.to_owned()
        };
        let compiled = compile_renderer(&source_literal);
        let intent_sha256 = if drift == SemanticDriftV1::Intent {
            drifted(compiled.intent_sha256)
        } else {
            compiled.intent_sha256
        };
        let operation_oir_sha256 = if drift == SemanticDriftV1::OperationOir {
            drifted(compiled.operation_oir_sha256)
        } else {
            compiled.operation_oir_sha256
        };
        let execution_plan_sha256 = if drift == SemanticDriftV1::ExecutionPlan {
            drifted(compiled.execution_plan_sha256)
        } else {
            compiled.execution_plan_sha256
        };
        let backend_catalog_sha256 = if drift == SemanticDriftV1::BackendCatalog {
            drifted(compiled.backend_catalog_sha256)
        } else {
            compiled.backend_catalog_sha256
        };
        let backend_implementation_sha256 = if drift == SemanticDriftV1::BackendImplementation {
            drifted(compiled.backend_implementation_sha256)
        } else {
            compiled.backend_implementation_sha256
        };

        let source_closure = FabricSourceClosureV1::new(
            FABRIC_SOURCE_CLOSURE_DIALECT_V1,
            compiled.source_utf8,
            FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
            Policy::Eager.name(),
            intent_sha256,
            operation_oir_sha256,
            execution_plan_sha256,
        )
        .unwrap();
        let region = SourceClosedRendererV1::new(
            compiled.renderer,
            vec![RendererPartV1::literal(retained_literal)],
            operation_oir_sha256,
            execution_plan_sha256,
            backend_catalog_sha256,
            backend_implementation_sha256,
        )
        .unwrap();
        let execution = ExecutionIdV1::new(digest(0x31)).unwrap();
        let logical_task = LogicalTaskIdV1::new(execution, digest(task_seed)).unwrap();
        let attempt = AttemptIdV1::new(logical_task, attempt_generation).unwrap();
        let issued_at = unix_millis_now().unwrap();
        let expires_at = issued_at.checked_add(MAX_FABRIC_LEASE_LIFETIME_MS).unwrap();
        let capsule = ExecutionCapsuleV1::new(
            attempt,
            region,
            digest(0x32),
            InputManifestV1::new(Vec::new()).unwrap(),
            OutputContractV1::for_renderer(RESULT_SLOT, compiled.renderer, MAX_OVALUE_RECORD_BYTES)
                .unwrap(),
            expires_at.get(),
            ExecutionLimitsV1::new(30_000, 16 * 1024, MAX_OVALUE_RECORD_BYTES).unwrap(),
        )
        .unwrap();
        let target = FabricTargetBindingV1::new(
            peer.clone(),
            NODE_ID,
            GenerationV1::new(NODE_GENERATION).unwrap(),
            incarnation,
            semantic(0x40),
            GenerationV1::new(1).unwrap(),
            GenerationV1::new(1).unwrap(),
            semantic(0x41),
            semantic(0x42),
            semantic(0x43),
            semantic(0x44),
            semantic(0x45),
            semantic(0x46),
            compiled.realization_pipeline_sha256,
        )
        .unwrap();
        let lease = PlacementLeaseV3::new(
            authority.key_id_digest(),
            semantic(nonce_seed),
            target,
            &source_closure,
            &capsule,
            issued_at,
            expires_at,
        )
        .unwrap();
        FabricSubmissionV1::new(
            authority.sign_execution_lease(lease).unwrap(),
            source_closure,
            encode_execution_capsule_v1(&capsule).unwrap(),
        )
        .unwrap()
    }

    fn exact_terminal_parts(reply: FabricProviderReplyV1) -> (Vec<u8>, Vec<u8>) {
        match reply {
            FabricProviderReplyV1::ExactTerminal {
                header_bytes,
                candidate_bytes,
            } => (header_bytes, candidate_bytes),
            other => panic!("expected exact terminal reply, got {other:?}"),
        }
    }

    fn rejected(reply: FabricProviderReplyV1, reason_code: &str) -> FabricRejectionV1 {
        let FabricProviderReplyV1::Response(FabricResponseV1::Rejected(rejection)) = reply else {
            panic!("expected provider rejection {reason_code}")
        };
        assert_eq!(rejection.reason_code(), reason_code);
        rejection
    }

    #[test]
    fn identical_duplicate_executes_once_reports_running_and_replays_exact_terminal() {
        let directory = private_tempdir();
        let authority = FabricSigningKeyV1::from_secret_bytes([0x11; 32]);
        let node_signer = FabricSigningKeyV1::from_secret_bytes([0x12; 32]);
        let executions = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new((Mutex::new(ProbeStateV1::default()), Condvar::new()));
        let observer_executions = Arc::clone(&executions);
        let observer_gate = Arc::clone(&gate);
        let observer: BeforeRealizeObserverV1 = Arc::new(move || {
            if observer_executions.fetch_add(1, Ordering::SeqCst) == 0 {
                let (state, changed) = &*observer_gate;
                let mut state = state.lock().unwrap();
                state.entered = true;
                changed.notify_all();
                state = changed.wait_while(state, |state| !state.released).unwrap();
                assert!(state.released);
            }
        });
        let provider = Arc::new(
            open_provider(directory.path(), &authority, &node_signer)
                .with_before_realize_observer(observer),
        );
        let peer = semantic(0x21);
        let submission = signed_submission(
            &authority,
            &peer,
            provider.execution_cell_incarnation(),
            0x22,
            1,
            0x23,
            "duplicate-proof",
            SemanticDriftV1::None,
        );

        let first_provider = Arc::clone(&provider);
        let first_peer = peer.as_sha256().to_owned();
        let first_submission = submission.clone();
        let first = thread::spawn(move || {
            first_provider
                .handle_request(
                    &first_peer,
                    FabricRequestV1::SubmitPureAttempt(first_submission),
                )
                .unwrap()
        });

        let (state, changed) = &*gate;
        let state = state.lock().unwrap();
        let (state, timeout) = changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.entered)
            .unwrap();
        assert!(!timeout.timed_out(), "provider did not enter realization");
        drop(state);

        let duplicate = provider
            .handle_request(
                peer.as_sha256(),
                FabricRequestV1::SubmitPureAttempt(submission.clone()),
            )
            .unwrap();
        let FabricProviderReplyV1::Response(FabricResponseV1::Running(status)) = duplicate else {
            panic!("duplicate of a running attempt did not return Running")
        };
        assert_eq!(status, FabricAttemptStatusV1::from_submission(&submission));
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        let mut state = gate.0.lock().unwrap();
        state.released = true;
        gate.1.notify_all();
        drop(state);

        let first_terminal = exact_terminal_parts(first.join().unwrap());
        let replayed_terminal = exact_terminal_parts(
            provider
                .handle_request(
                    peer.as_sha256(),
                    FabricRequestV1::SubmitPureAttempt(submission),
                )
                .unwrap(),
        );
        assert_eq!(replayed_terminal, first_terminal);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn provider_relowers_trusted_source_and_rejects_semantic_drift_before_execution() {
        let directory = private_tempdir();
        let authority = FabricSigningKeyV1::from_secret_bytes([0x31; 32]);
        let node_signer = FabricSigningKeyV1::from_secret_bytes([0x32; 32]);
        let executions = Arc::new(AtomicUsize::new(0));
        let observer_executions = Arc::clone(&executions);
        let provider = open_provider(directory.path(), &authority, &node_signer)
            .with_before_realize_observer(Arc::new(move || {
                observer_executions.fetch_add(1, Ordering::SeqCst);
            }));
        let peer = semantic(0x33);
        let incarnation = provider.execution_cell_incarnation();

        let valid = signed_submission(
            &authority,
            &peer,
            incarnation,
            0x34,
            1,
            0x35,
            "valid-source",
            SemanticDriftV1::None,
        );
        exact_terminal_parts(
            provider
                .handle_request(peer.as_sha256(), FabricRequestV1::SubmitPureAttempt(valid))
                .unwrap(),
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        for (index, drift, expected) in [
            (
                0_u8,
                SemanticDriftV1::LexicalSource,
                "lowered lexical renderer role/order/content differs",
            ),
            (1, SemanticDriftV1::Intent, "execution intent: retained="),
            (2, SemanticDriftV1::OperationOir, "operation OIR: retained="),
            (
                3,
                SemanticDriftV1::ExecutionPlan,
                "execution plan: retained=",
            ),
            (
                4,
                SemanticDriftV1::BackendCatalog,
                "backend catalog projection: retained=",
            ),
            (
                5,
                SemanticDriftV1::BackendImplementation,
                "backend implementation: retained=",
            ),
        ] {
            let submission = signed_submission(
                &authority,
                &peer,
                incarnation,
                0x40 + index,
                1,
                0x50 + index,
                "retained-source",
                drift,
            );
            let rejection = rejected(
                provider
                    .handle_request(
                        peer.as_sha256(),
                        FabricRequestV1::SubmitPureAttempt(submission),
                    )
                    .unwrap(),
                "source-reconstruction-rejected",
            );
            assert!(
                rejection.message().contains(expected),
                "{drift:?} rejection lost its reconstruction cause: {}",
                rejection.message()
            );
            assert_eq!(executions.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn provider_maps_nonce_reuse_and_attempt_rebinding_without_reexecution() {
        let directory = private_tempdir();
        let authority = FabricSigningKeyV1::from_secret_bytes([0x51; 32]);
        let node_signer = FabricSigningKeyV1::from_secret_bytes([0x52; 32]);
        let executions = Arc::new(AtomicUsize::new(0));
        let observer_executions = Arc::clone(&executions);
        let provider = open_provider(directory.path(), &authority, &node_signer)
            .with_before_realize_observer(Arc::new(move || {
                observer_executions.fetch_add(1, Ordering::SeqCst);
            }));
        let peer = semantic(0x53);
        let incarnation = provider.execution_cell_incarnation();

        let first = signed_submission(
            &authority,
            &peer,
            incarnation,
            0x54,
            1,
            0x55,
            "first-capsule",
            SemanticDriftV1::None,
        );
        exact_terminal_parts(
            provider
                .handle_request(peer.as_sha256(), FabricRequestV1::SubmitPureAttempt(first))
                .unwrap(),
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        let reused_nonce_different_capsule = signed_submission(
            &authority,
            &peer,
            incarnation,
            0x56,
            1,
            0x55,
            "different-capsule",
            SemanticDriftV1::None,
        );
        rejected(
            provider
                .handle_request(
                    peer.as_sha256(),
                    FabricRequestV1::SubmitPureAttempt(reused_nonce_different_capsule),
                )
                .unwrap(),
            "lease-nonce-reused",
        );

        let rebound_attempt = signed_submission(
            &authority,
            &peer,
            incarnation,
            0x54,
            1,
            0x57,
            "rebound-capsule",
            SemanticDriftV1::None,
        );
        rejected(
            provider
                .handle_request(
                    peer.as_sha256(),
                    FabricRequestV1::SubmitPureAttempt(rebound_attempt),
                )
                .unwrap(),
            "attempt-rebound",
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn restart_rejects_prior_incarnation_new_work_but_replays_historical_terminal() {
        let directory = private_tempdir();
        let authority = FabricSigningKeyV1::from_secret_bytes([0x61; 32]);
        let node_signer = FabricSigningKeyV1::from_secret_bytes([0x62; 32]);
        let peer = semantic(0x63);
        let first_provider = open_provider(directory.path(), &authority, &node_signer);
        let first_incarnation = first_provider.execution_cell_incarnation();
        let historical_submission = signed_submission(
            &authority,
            &peer,
            first_incarnation,
            0x64,
            1,
            0x65,
            "historical-terminal",
            SemanticDriftV1::None,
        );
        let original_terminal = exact_terminal_parts(
            first_provider
                .handle_request(
                    peer.as_sha256(),
                    FabricRequestV1::SubmitPureAttempt(historical_submission.clone()),
                )
                .unwrap(),
        );
        drop(first_provider);

        let executions = Arc::new(AtomicUsize::new(0));
        let observer_executions = Arc::clone(&executions);
        let restarted = open_provider(directory.path(), &authority, &node_signer)
            .with_before_realize_observer(Arc::new(move || {
                observer_executions.fetch_add(1, Ordering::SeqCst);
            }));
        assert_eq!(
            restarted.execution_cell_incarnation().get(),
            first_incarnation.get() + 1
        );

        let historical_replay = exact_terminal_parts(
            restarted
                .handle_request(
                    peer.as_sha256(),
                    FabricRequestV1::SubmitPureAttempt(historical_submission),
                )
                .unwrap(),
        );
        assert_eq!(historical_replay, original_terminal);
        let FabricResponseV1::TerminalCandidate(terminal) =
            decode_fabric_response_v1(&historical_replay.0, Some(&historical_replay.1)).unwrap()
        else {
            panic!("historical exact bytes did not decode as a terminal candidate")
        };
        assert_eq!(
            terminal.signed_receipt().receipt().node_generation(),
            GenerationV1::new(NODE_GENERATION).unwrap()
        );
        assert_eq!(
            terminal
                .signed_receipt()
                .receipt()
                .execution_cell_incarnation(),
            first_incarnation
        );

        let stale_new_work = signed_submission(
            &authority,
            &peer,
            first_incarnation,
            0x66,
            1,
            0x67,
            "stale-new-work",
            SemanticDriftV1::None,
        );
        rejected(
            restarted
                .handle_request(
                    peer.as_sha256(),
                    FabricRequestV1::SubmitPureAttempt(stale_new_work),
                )
                .unwrap(),
            "target-binding-rejected",
        );
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }
}
