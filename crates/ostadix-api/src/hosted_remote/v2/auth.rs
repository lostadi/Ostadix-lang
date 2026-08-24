use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::backend_catalog::BackendRegistry;
use crate::eval::PlacementFragmentBindingsV2;
use crate::placement::{
    ActorGenerationIdV1, CandidateDecisionV1, CanonicalPlacementRecordV1, CurrentBackendCatalogV1,
    EnvironmentRequirementV1, GenerationV1, LeaseExpectationV2, LeaseStateBindingV2,
    PlacementCandidateInputV1, RecordAuthenticationV1, RecordAuthenticatorV1, RequirementAtomV1,
    SemanticDigestV1, StateControlExpectationV2, StateQuotaLimitsV2, StateReservationV2,
    StateSessionIdV2, UnixMillisV1,
};

use super::crypto::{constant_time_eq, verify_placement_lease_signature_v2};
use super::protocol::{
    HostedCommandBindingV2, HostedPlacementAuthorityV2, HostedPlacementIdentityV2,
    PlacementPurposeV2, SessionStateTierV2, SignedPlacementLeaseV2,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementAuthorizationContextV2 {
    pub node_id: String,
    pub node_generation: GenerationV1,
    pub principal_sha256: String,
    pub state_session: StateSessionIdV2,
    pub session_state_tier: SessionStateTierV2,
    pub client_request_id: String,
    pub client_sequence: u64,
    pub purpose: PlacementPurposeV2,
    pub operation_sha256: Option<String>,
    pub recovery_warrant_sha256: Option<String>,
    pub state_quota_generation: GenerationV1,
    pub state_quota_limits: StateQuotaLimitsV2,
    pub state_reservation: StateReservationV2,
    pub current_actor_generation: Option<ActorGenerationIdV1>,
    pub next_actor_generation: GenerationV1,
    pub prepared_fragment: Option<PlacementFragmentBindingsV2>,
    pub expected_session_identity: Option<HostedPlacementIdentityV2>,
    pub now_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedPlacementV2 {
    pub lease_sha256: String,
    pub lease_nonce: String,
    pub expires_at_unix_ms: u64,
    pub state_session: StateSessionIdV2,
    pub state_tier: SessionStateTierV2,
    pub state_quota_generation: GenerationV1,
    pub state_quota_limits: StateQuotaLimitsV2,
    pub state_reservation: StateReservationV2,
    pub actor_generation: Option<ActorGenerationIdV1>,
    pub placement_identity: HostedPlacementIdentityV2,
}

/// Exact production seam between the hosted transport and scheduler/registry
/// authority. Implementations must authenticate the carrier and every field;
/// the transport never infers permission from a name, catalog, or TLS client.
pub trait PlacementProofAuthorizerV2: Send + Sync {
    fn authorize(
        &self,
        context: &PlacementAuthorizationContextV2,
        lease: &SignedPlacementLeaseV2,
    ) -> Result<AuthorizedPlacementV2>;
}

pub type SharedPlacementAuthorizerV2 = Arc<dyn PlacementProofAuthorizerV2>;

/// Safe default for a node without a configured registry/scheduler adapter.
/// It permits profile/status transport setup but never compute or recovery.
#[derive(Debug, Default)]
pub struct DenyAllPlacementAuthorizerV2;

impl PlacementProofAuthorizerV2 for DenyAllPlacementAuthorizerV2 {
    fn authorize(
        &self,
        _context: &PlacementAuthorizationContextV2,
        _lease: &SignedPlacementLeaseV2,
    ) -> Result<AuthorizedPlacementV2> {
        bail!("no authenticated hosted V2 placement authority is configured")
    }
}

/// Usability-first authority for automatically discovered LAN nodes.
///
/// Any correctly formed lease signed by any key is accepted after its command
/// is matched to the live request context. Placement evidence is retained in
/// the journal as an explanatory projection, but it is not treated as an
/// authorization barrier. This intentionally makes LAN reachability plus the
/// automatically enrolled TLS identity the effective trust boundary.
#[derive(Debug, Default, Clone)]
pub struct LanOpenPlacementAuthorizerV2;

impl PlacementProofAuthorizerV2 for LanOpenPlacementAuthorizerV2 {
    fn authorize(
        &self,
        context: &PlacementAuthorizationContextV2,
        envelope: &SignedPlacementLeaseV2,
    ) -> Result<AuthorizedPlacementV2> {
        // Keep the envelope internally self-consistent and attributable even
        // though no particular authority key is privileged in LAN-open mode.
        verify_placement_lease_signature_v2(envelope)
            .context("LAN-open placement envelope signature is malformed")?;
        envelope.evidence.validate_shape()?;
        let command = &envelope.command;
        exact_command_context(context, command)?;
        let (issued_at, expires_at) = match (&envelope.authority, command.purpose) {
            (HostedPlacementAuthorityV2::Execution(lease), PlacementPurposeV2::Execute) => {
                (lease.issued_at().get(), lease.expires_at().get())
            }
            (
                HostedPlacementAuthorityV2::StateControl(lease),
                PlacementPurposeV2::OpenSession | PlacementPurposeV2::Recover,
            ) => (lease.issued_at().get(), lease.expires_at().get()),
            (HostedPlacementAuthorityV2::Execution(_), _) => {
                bail!("OpenSession and Recover require a state-control lease")
            }
            (HostedPlacementAuthorityV2::StateControl(_), PlacementPurposeV2::Execute) => {
                bail!("Execute requires an execution placement lease")
            }
        };
        if context.now_unix_ms < issued_at {
            bail!("LAN-open placement authority is not yet valid");
        }
        if context.now_unix_ms >= expires_at {
            bail!("LAN-open placement authority has expired");
        }
        if command.state_session.node_generation() != context.node_generation {
            bail!("LAN-open command names a different node generation");
        }
        if command.state_quota_generation != context.state_quota_generation
            || command.state_quota_limits != context.state_quota_limits
        {
            bail!("LAN-open command quota coordinates differ from the node");
        }
        command
            .state_reservation
            .validate_against(&context.state_quota_limits)?;
        if command.session_state_tier == SessionStateTierV2::CheckpointRestore
            && command.state_reservation.snapshot_bytes_per_actor() == 0
        {
            bail!("checkpoint/restore LAN-open session requires a nonzero snapshot reservation");
        }
        if context.state_session != command.state_session
            || context.state_reservation != command.state_reservation
        {
            bail!("LAN-open command state identity differs from the live request");
        }
        validate_command_actor_lifecycle(
            command.purpose,
            context.session_state_tier,
            context.current_actor_generation.as_ref(),
            command.actor_generation.as_ref(),
        )?;

        let evidence = &envelope.evidence;
        let target_descriptor = evidence.node_profile.descriptor_digest()?;
        let footprint = evidence.requirement_footprint.semantic_digest()?;
        let scope = evidence.warrant_discharge.exact_scope();
        let backend_implementation = scope
            .backend_implementation()
            .cloned()
            .or_else(|| {
                context
                    .prepared_fragment
                    .as_ref()
                    .map(|fragment| fragment.backend_implementation_sha256().clone())
            })
            .context("LAN-open placement evidence omits a backend implementation")?;
        let realization_pipeline = scope
            .realization_pipeline()
            .cloned()
            .or_else(|| {
                context
                    .prepared_fragment
                    .as_ref()
                    .map(|fragment| fragment.realization_pipeline().clone())
            })
            .context("LAN-open placement evidence omits a realization pipeline")?;
        let placement_identity = HostedPlacementIdentityV2 {
            target_descriptor,
            requirement_footprint: footprint,
            backend_implementation,
            realization_pipeline,
            trust_policy: evidence.trust_policy.semantic_digest()?,
            reservation: evidence.reservation.clone(),
        };
        if context
            .expected_session_identity
            .as_ref()
            .is_some_and(|expected| expected != &placement_identity)
        {
            bail!("LAN-open command attempts to switch the session placement identity");
        }

        let actor_generation = match command.purpose {
            PlacementPurposeV2::OpenSession => None,
            PlacementPurposeV2::Recover => context.current_actor_generation.clone(),
            PlacementPurposeV2::Execute
                if context.session_state_tier == SessionStateTierV2::Stateless =>
            {
                let fragment = context
                    .prepared_fragment
                    .as_ref()
                    .context("LAN-open Execute requires a locally prepared fragment")?;
                if !fragment.environment().is_fresh() {
                    bail!("stateless LAN-open session requires a fresh fragment environment");
                }
                None
            }
            PlacementPurposeV2::Execute => {
                // Stateful LAN-open sessions still bind to the node's actual
                // prepared fragment, not the advisory client-side evidence.
                if let Some(current) = &context.current_actor_generation {
                    Some(current.clone())
                } else {
                    let fragment = context
                        .prepared_fragment
                        .as_ref()
                        .context("stateful LAN-open Execute requires a prepared fragment")?;
                    if fragment.environment().is_fresh() {
                        bail!("stateful LAN-open session requires a persistent environment");
                    }
                    let logical_environment =
                        logical_environment_requirement(fragment.requirement_footprint())?
                            .context("stateful LAN-open fragment omits its logical environment")?;
                    Some(ActorGenerationIdV1::new(
                        logical_environment,
                        fragment.backend_implementation_sha256().clone(),
                        placement_identity.target_descriptor.clone(),
                        fragment.sandbox_policy_sha256().clone(),
                        fragment.backend_launch_generation().clone(),
                        context.next_actor_generation,
                    ))
                }
            }
        };

        Ok(AuthorizedPlacementV2 {
            lease_sha256: envelope.authority.semantic_digest()?.to_string(),
            lease_nonce: envelope.authority.lease_nonce().to_string(),
            expires_at_unix_ms: envelope.authority.expires_at().get(),
            state_session: command.state_session.clone(),
            state_tier: command.session_state_tier,
            state_quota_generation: command.state_quota_generation,
            state_quota_limits: command.state_quota_limits.clone(),
            state_reservation: command.state_reservation.clone(),
            actor_generation,
            placement_identity,
        })
    }
}

/// Production adapter for one explicitly pinned registry-compatible Ed25519
/// authority key.  The signature authenticates the canonical placement lease,
/// exact hosted command binding, and (for open) the state-capacity observation.
#[derive(Debug, Clone)]
pub struct PinnedEd25519PlacementAuthorizerV2 {
    trusted_public_key: [u8; 32],
}

impl PinnedEd25519PlacementAuthorizerV2 {
    pub fn new(trusted_public_key: [u8; 32]) -> Self {
        Self { trusted_public_key }
    }
}

impl PlacementProofAuthorizerV2 for PinnedEd25519PlacementAuthorizerV2 {
    fn authorize(
        &self,
        context: &PlacementAuthorizationContextV2,
        envelope: &SignedPlacementLeaseV2,
    ) -> Result<AuthorizedPlacementV2> {
        let actual_key = verify_placement_lease_signature_v2(envelope)?;
        if !constant_time_eq(&actual_key, &self.trusted_public_key) {
            bail!("hosted placement lease was not signed by the pinned authority");
        }
        let command = &envelope.command;
        exact_command_context(context, command)?;

        if command.state_session.node_generation() != context.node_generation {
            bail!("hosted command state session names a different node generation");
        }
        if command.state_quota_generation != context.state_quota_generation {
            bail!("hosted command state quota generation mismatch");
        }
        if command.state_quota_limits != context.state_quota_limits {
            bail!("hosted command state quota limits mismatch");
        }
        command
            .state_reservation
            .validate_against(&context.state_quota_limits)
            .context("hosted command state reservation exceeds node quota policy")?;
        if command.session_state_tier == SessionStateTierV2::CheckpointRestore
            && command.state_reservation.snapshot_bytes_per_actor() == 0
        {
            bail!("checkpoint/restore session requires a nonzero snapshot reservation");
        }
        if context.state_session != command.state_session {
            bail!("hosted command state session mismatch");
        }
        if context.state_reservation != command.state_reservation {
            bail!("hosted command state reservation mismatch");
        }
        validate_command_actor_lifecycle(
            command.purpose,
            context.session_state_tier,
            context.current_actor_generation.as_ref(),
            command.actor_generation.as_ref(),
        )?;

        envelope.evidence.validate_shape()?;
        let authority_digest = envelope
            .authority
            .semantic_digest()
            .context("failed to digest canonical hosted authority")?;
        let command_digest = command.semantic_digest()?;
        let issuer = envelope.authority.issuer_key();
        let authority_kind = match envelope.authority {
            HostedPlacementAuthorityV2::Execution(_) => "placement lease v2",
            HostedPlacementAuthorityV2::StateControl(_) => "state-control lease v2",
        };
        let evidence = &envelope.evidence;
        if evidence.node_profile.issuer_key() != issuer
            || evidence.capacity_observation.issuer_key() != issuer
            || evidence
                .warrants
                .iter()
                .any(|warrant| warrant.issuer_key() != issuer)
        {
            bail!("hosted V2 proof records must name the pinned envelope authority as issuer");
        }
        let profile_digest = evidence
            .node_profile
            .semantic_digest()
            .context("failed to digest placement node profile")?;
        let capacity_digest = evidence
            .capacity_observation
            .semantic_digest()
            .context("failed to digest placement capacity observation")?;
        let footprint_digest = evidence
            .requirement_footprint
            .semantic_digest()
            .context("failed to digest requirement footprint")?;
        let discharge_digest = evidence
            .warrant_discharge
            .semantic_digest()
            .context("failed to digest warrant discharge")?;
        let trust_digest = evidence
            .trust_policy
            .semantic_digest()
            .context("failed to digest placement trust policy")?;
        let mut authenticated_records = vec![
            (authority_kind, issuer.clone(), authority_digest.clone()),
            ("node profile", issuer.clone(), profile_digest),
            (
                "capacity observation",
                issuer.clone(),
                capacity_digest.clone(),
            ),
        ];
        for warrant in &evidence.warrants {
            authenticated_records.push((
                "placement warrant",
                issuer.clone(),
                warrant
                    .semantic_digest()
                    .context("failed to digest placement warrant")?,
            ));
        }

        let open_state_observation = match command.purpose {
            PlacementPurposeV2::OpenSession => {
                let observation = envelope
                    .state_capacity_observation
                    .as_ref()
                    .context("open-session envelope omits state-capacity observation")?;
                let observation_digest = observation
                    .semantic_digest()
                    .context("failed to digest state-capacity observation")?;
                if observation.issuer_key() != issuer {
                    bail!("state-capacity observation issuer differs from envelope authority");
                }
                authenticated_records.push((
                    "state capacity observation",
                    issuer.clone(),
                    observation_digest.clone(),
                ));
                let authenticator = ExactEnvelopeAuthenticator {
                    records: &authenticated_records,
                };
                observation
                    .validate_at(UnixMillisV1::new(context.now_unix_ms), &authenticator)
                    .context("state-capacity observation is not fresh and authenticated")?;
                if observation.node_id() != context.node_id
                    || observation.node_generation() != context.node_generation
                    || observation.capacity_generation() != context.state_quota_generation
                {
                    bail!("state-capacity observation identity or quota generation mismatch");
                }
                if observation.limits() != &context.state_quota_limits {
                    bail!("state-capacity observation quota limits mismatch");
                }
                if !observation.can_admit(&command.state_reservation) {
                    bail!("state-capacity observation cannot admit reservation");
                }
                Some(observation_digest)
            }
            PlacementPurposeV2::Execute | PlacementPurposeV2::Recover => {
                if envelope.state_capacity_observation.is_some() {
                    bail!("existing-session envelope unexpectedly carries a capacity observation");
                }
                None
            }
        };
        let authenticator = ExactEnvelopeAuthenticator {
            records: &authenticated_records,
        };

        let target_descriptor = evidence
            .node_profile
            .descriptor_digest()
            .context("failed to digest target descriptor")?;
        if evidence.node_profile.descriptor().node_id() != context.node_id
            || evidence.node_profile.descriptor().node_generation() != context.node_generation
        {
            bail!("placement node profile does not name this exact node generation");
        }
        let scope = evidence.warrant_discharge.exact_scope();
        let selected_backend_sha256 = scope
            .backend_implementation()
            .context("warrant discharge omits exact backend implementation")?;
        let selected_pipeline = scope
            .realization_pipeline()
            .context("warrant discharge omits exact realization pipeline")?;
        let selected_backend = evidence
            .node_profile
            .descriptor()
            .backend_implementations()
            .iter()
            .find(|implementation| {
                implementation
                    .semantic_digest()
                    .is_ok_and(|digest| &digest == selected_backend_sha256)
            })
            .context("selected backend implementation is absent from node profile")?;
        if selected_backend.realization_pipeline() != selected_pipeline {
            bail!("warrant discharge backend and realization pipeline disagree");
        }
        let state_support = BackendRegistry::global()
            .state_support_for_current_specification(selected_backend.backend_specification())
            .context("selected backend specification has no current state-support contract")?;
        command
            .session_state_tier
            .validate_backend_support(state_support)?;

        let prospective_logical_environment =
            logical_environment_requirement(&evidence.requirement_footprint)?;
        let authorized_actor = match command.purpose {
            PlacementPurposeV2::OpenSession => None,
            PlacementPurposeV2::Recover => {
                let actor = context
                    .current_actor_generation
                    .clone()
                    .context("recovery requires an established actor generation")?;
                validate_actor_proof_coordinates(
                    &actor,
                    prospective_logical_environment.as_ref(),
                    &target_descriptor,
                    selected_backend_sha256,
                )?;
                Some(actor)
            }
            PlacementPurposeV2::Execute => {
                derive_execution_actor(context, &target_descriptor, selected_backend_sha256)?
            }
        };
        let expected_state = match open_state_observation {
            Some(observation) => {
                LeaseStateBindingV2::open(observation, command.state_reservation.clone())
            }
            None => LeaseStateBindingV2::existing(
                command.state_session.clone(),
                command
                    .actor_generation
                    .as_ref()
                    .map(CanonicalPlacementRecordV1::semantic_digest)
                    .transpose()?,
            ),
        };
        let first_stateful_execute = command.purpose == PlacementPurposeV2::Execute
            && command.session_state_tier != SessionStateTierV2::Stateless
            && context.current_actor_generation.is_none();
        let candidate_prospective_environment =
            if command.purpose == PlacementPurposeV2::OpenSession || first_stateful_execute {
                prospective_logical_environment.as_ref()
            } else {
                None
            };
        let candidate = PlacementCandidateInputV1 {
            profile: &evidence.node_profile,
            capacity: &evidence.capacity_observation,
            footprint: &evidence.requirement_footprint,
            discharge: &evidence.warrant_discharge,
            warrants: &evidence.warrants,
            trust_policy: &evidence.trust_policy,
            reservation: &evidence.reservation,
            actor_generation: command.actor_generation.as_ref(),
            prospective_logical_environment: candidate_prospective_environment,
        };
        let eligibility = match candidate.evaluate_with_catalog(
            UnixMillisV1::new(context.now_unix_ms),
            &authenticator,
            BackendRegistry::global(),
        ) {
            CandidateDecisionV1::Eligible { proof } => proof,
            CandidateDecisionV1::Ineligible { rejections } => {
                bail!("hosted placement evidence is ineligible: {rejections:?}")
            }
        };
        let eligibility_digest = eligibility.semantic_digest()?;
        let placement_identity = HostedPlacementIdentityV2 {
            target_descriptor: target_descriptor.clone(),
            requirement_footprint: footprint_digest.clone(),
            backend_implementation: selected_backend_sha256.clone(),
            realization_pipeline: selected_pipeline.clone(),
            trust_policy: trust_digest.clone(),
            reservation: evidence.reservation.clone(),
        };
        if context
            .expected_session_identity
            .as_ref()
            .is_some_and(|expected| expected != &placement_identity)
        {
            bail!("hosted command attempts to switch the session placement identity");
        }

        match (&envelope.authority, command.purpose) {
            (HostedPlacementAuthorityV2::Execution(lease), PlacementPurposeV2::Execute) => {
                let fragment = context
                    .prepared_fragment
                    .as_ref()
                    .context("execute authorization requires a locally prepared fragment")?;
                if fragment.requirement_footprint() != &evidence.requirement_footprint
                    || fragment.requirement_footprint_sha256() != &footprint_digest
                    || fragment.backend_implementation_sha256() != selected_backend_sha256
                    || fragment.realization_pipeline() != selected_pipeline
                {
                    bail!("placement proof does not match the locally prepared fragment");
                }
                if scope.operation_oir() != Some(fragment.operation_oir()) {
                    bail!("warrant discharge operation does not match prepared OIR");
                }
                let expected = LeaseExpectationV2::new(
                    &context.node_id,
                    target_descriptor,
                    evidence.node_profile.profile_generation(),
                    evidence.capacity_observation.capacity_generation(),
                    capacity_digest,
                    eligibility_digest,
                    fragment.operation_oir().clone(),
                    footprint_digest,
                    discharge_digest,
                    fragment.placement_admission().clone(),
                    fragment.task_attempt().clone(),
                    fragment.backend_implementation_sha256().clone(),
                    fragment.realization_pipeline().clone(),
                    trust_digest,
                    evidence.reservation.clone(),
                    command_digest,
                    expected_state,
                )?;
                lease
                    .validate_for(
                        &expected,
                        UnixMillisV1::new(context.now_unix_ms),
                        &authenticator,
                    )
                    .context("canonical execution placement lease validation failed")?;
            }
            (
                HostedPlacementAuthorityV2::StateControl(lease),
                PlacementPurposeV2::OpenSession | PlacementPurposeV2::Recover,
            ) => {
                if context.prepared_fragment.is_some() {
                    bail!("state-control authorization unexpectedly carries an execution fragment");
                }
                let expected = StateControlExpectationV2::new(
                    &context.node_id,
                    target_descriptor,
                    evidence.node_profile.profile_generation(),
                    evidence.capacity_observation.capacity_generation(),
                    capacity_digest,
                    eligibility_digest,
                    footprint_digest,
                    discharge_digest,
                    selected_backend_sha256.clone(),
                    selected_pipeline.clone(),
                    trust_digest,
                    evidence.reservation.clone(),
                    command_digest,
                    expected_state,
                )?;
                lease
                    .validate_for(
                        &expected,
                        UnixMillisV1::new(context.now_unix_ms),
                        &authenticator,
                    )
                    .context("canonical state-control lease validation failed")?;
            }
            (HostedPlacementAuthorityV2::Execution(_), _) => {
                bail!("OpenSession and Recover require a state-control lease")
            }
            (HostedPlacementAuthorityV2::StateControl(_), PlacementPurposeV2::Execute) => {
                bail!("Execute requires an execution placement lease")
            }
        }

        Ok(AuthorizedPlacementV2 {
            lease_sha256: authority_digest.to_string(),
            lease_nonce: envelope.authority.lease_nonce().to_string(),
            expires_at_unix_ms: envelope.authority.expires_at().get(),
            state_session: command.state_session.clone(),
            state_tier: command.session_state_tier,
            state_quota_generation: command.state_quota_generation,
            state_quota_limits: command.state_quota_limits.clone(),
            state_reservation: command.state_reservation.clone(),
            actor_generation: authorized_actor,
            placement_identity,
        })
    }
}

fn logical_environment_requirement(
    footprint: &crate::placement::RequirementFootprintV1,
) -> Result<Option<SemanticDigestV1>> {
    let mut logical_environment = None;
    for requirement in footprint.require_complete()? {
        if let RequirementAtomV1::Environment(EnvironmentRequirementV1::SameLogicalEnvironment {
            identity,
        }) = requirement
        {
            if logical_environment.replace(identity.clone()).is_some() {
                bail!("placement footprint contains multiple logical environments");
            }
        }
    }
    Ok(logical_environment)
}

fn derive_execution_actor(
    context: &PlacementAuthorizationContextV2,
    target_descriptor: &SemanticDigestV1,
    selected_backend: &SemanticDigestV1,
) -> Result<Option<ActorGenerationIdV1>> {
    let fragment = context
        .prepared_fragment
        .as_ref()
        .context("execute authorization requires a locally prepared fragment")?;
    if context.session_state_tier == SessionStateTierV2::Stateless {
        if !fragment.environment().is_fresh() {
            bail!("stateless hosted session requires a fresh fragment environment");
        }
        if context.current_actor_generation.is_some() {
            bail!("stateless hosted session unexpectedly retains an actor generation");
        }
        return Ok(None);
    }
    if fragment.environment().is_fresh() {
        bail!("stateful hosted session requires an explicit persistent environment");
    }
    if fragment.backend_implementation_sha256() != selected_backend {
        bail!("prepared backend implementation differs from placement proof");
    }
    let logical_environment = logical_environment_requirement(fragment.requirement_footprint())?
        .context("stateful prepared fragment omits its logical environment identity")?;
    let actor = ActorGenerationIdV1::new(
        logical_environment,
        fragment.backend_implementation_sha256().clone(),
        target_descriptor.clone(),
        fragment.sandbox_policy_sha256().clone(),
        fragment.backend_launch_generation().clone(),
        context.next_actor_generation,
    );
    if let Some(current) = &context.current_actor_generation {
        validate_actor_exact(current, &actor)?;
    }
    Ok(Some(actor))
}

fn validate_command_actor_lifecycle(
    purpose: PlacementPurposeV2,
    state_tier: SessionStateTierV2,
    current_actor: Option<&ActorGenerationIdV1>,
    signed_actor: Option<&ActorGenerationIdV1>,
) -> Result<()> {
    match purpose {
        PlacementPurposeV2::OpenSession => {
            if current_actor.is_some() || signed_actor.is_some() {
                bail!("OpenSession cannot bind an already established actor generation");
            }
        }
        PlacementPurposeV2::Execute if state_tier == SessionStateTierV2::Stateless => {
            if current_actor.is_some() || signed_actor.is_some() {
                bail!("stateless Execute cannot bind a retained actor generation");
            }
        }
        PlacementPurposeV2::Execute => match (current_actor, signed_actor) {
            (None, None) => {}
            (None, Some(_)) => {
                bail!("first stateful Execute must let the node establish actor generation")
            }
            (Some(_), None) => {
                bail!("stateful Execute omitted the established actor generation")
            }
            (Some(current), Some(signed)) if current != signed => {
                bail!("stateful Execute binds a different actor generation")
            }
            (Some(_), Some(_)) => {}
        },
        PlacementPurposeV2::Recover => {
            let current =
                current_actor.context("recovery requires an established actor generation")?;
            if signed_actor != Some(current) {
                bail!("Recover must bind the exact established actor generation");
            }
        }
    }
    Ok(())
}

fn validate_actor_proof_coordinates(
    actor: &ActorGenerationIdV1,
    logical_environment: Option<&SemanticDigestV1>,
    target_descriptor: &SemanticDigestV1,
    backend_implementation: &SemanticDigestV1,
) -> Result<()> {
    if logical_environment != Some(actor.logical_environment()) {
        bail!("actor logical environment differs from placement footprint");
    }
    if actor.target_descriptor() != target_descriptor {
        bail!("actor target descriptor differs from placement proof");
    }
    if actor.backend_implementation() != backend_implementation {
        bail!("actor backend implementation differs from placement proof");
    }
    Ok(())
}

fn validate_actor_exact(
    expected: &ActorGenerationIdV1,
    actual: &ActorGenerationIdV1,
) -> Result<()> {
    if expected.logical_environment() != actual.logical_environment() {
        bail!("actor logical environment changed within the session");
    }
    if expected.backend_implementation() != actual.backend_implementation() {
        bail!("actor backend implementation changed within the session");
    }
    if expected.target_descriptor() != actual.target_descriptor() {
        bail!("actor target descriptor changed within the session");
    }
    if expected.sandbox_policy() != actual.sandbox_policy() {
        bail!("actor sandbox policy changed within the session");
    }
    if expected.launch_context() != actual.launch_context() {
        bail!("actor admitted launch context changed within the session");
    }
    if expected.generation() != actual.generation() {
        bail!("actor generation changed without an explicit reset/recovery transition");
    }
    Ok(())
}

fn exact_command_context(
    context: &PlacementAuthorizationContextV2,
    command: &HostedCommandBindingV2,
) -> Result<()> {
    macro_rules! exact {
        ($name:literal, $actual:expr, $expected:expr) => {
            if $actual != $expected {
                bail!("hosted command {} binding mismatch", $name);
            }
        };
    }
    exact!("node", &command.node_id, &context.node_id);
    exact!(
        "principal",
        &command.principal_sha256,
        &context.principal_sha256
    );
    exact!(
        "session state tier",
        command.session_state_tier,
        context.session_state_tier
    );
    exact!(
        "request",
        &command.client_request_id,
        &context.client_request_id
    );
    exact!("sequence", command.client_sequence, context.client_sequence);
    exact!("purpose", command.purpose, context.purpose);
    exact!(
        "operation",
        &command.operation_sha256,
        &context.operation_sha256
    );
    exact!(
        "recovery warrant",
        &command.recovery_warrant_sha256,
        &context.recovery_warrant_sha256
    );
    Ok(())
}

struct ExactEnvelopeAuthenticator<'a> {
    records: &'a [(&'static str, SemanticDigestV1, SemanticDigestV1)],
}

impl RecordAuthenticatorV1 for ExactEnvelopeAuthenticator<'_> {
    fn authenticate(&self, record: &RecordAuthenticationV1) -> bool {
        self.records.iter().any(|(kind, issuer, digest)| {
            record.record_kind() == *kind
                && record.issuer_key() == issuer
                && record.record_digest() == digest
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(generation: u64) -> Result<ActorGenerationIdV1> {
        let digest = |label: &'static [u8]| {
            SemanticDigestV1::hash_bytes("ostadix/hosted/auth-actor-lifecycle-test/v2", label)
        };
        Ok(ActorGenerationIdV1::new(
            digest(b"logical-environment"),
            digest(b"backend-implementation"),
            digest(b"target-descriptor"),
            digest(b"sandbox-policy"),
            digest(b"launch-context"),
            GenerationV1::new(generation)?,
        ))
    }

    #[test]
    fn actor_none_is_rejected_after_first_stateful_execute_establishes_actor() -> Result<()> {
        let current = actor(1)?;
        let error = validate_command_actor_lifecycle(
            PlacementPurposeV2::Execute,
            SessionStateTierV2::CheckpointRestore,
            Some(&current),
            None,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("omitted the established actor generation"));
        Ok(())
    }

    #[test]
    fn forged_actor_is_rejected_before_first_stateful_execute() -> Result<()> {
        let forged = actor(1)?;
        let error = validate_command_actor_lifecycle(
            PlacementPurposeV2::Execute,
            SessionStateTierV2::CheckpointRestore,
            None,
            Some(&forged),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("must let the node establish actor generation"));
        Ok(())
    }

    #[test]
    fn later_mismatched_actor_generation_is_rejected() -> Result<()> {
        let current = actor(1)?;
        let mismatched = actor(2)?;
        let error = validate_command_actor_lifecycle(
            PlacementPurposeV2::Execute,
            SessionStateTierV2::CheckpointRestore,
            Some(&current),
            Some(&mismatched),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("binds a different actor generation"));
        Ok(())
    }
}
