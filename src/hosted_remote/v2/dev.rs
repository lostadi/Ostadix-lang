//! Co-located self-attested development-authority proof construction.
//!
//! This is deliberately not discovery or a scheduler service. It turns one
//! locally prepared, single-fragment admission into the same full proof bundle
//! that the production node re-evaluates. Target support is synthesized from
//! the admitted requirement footprint, explicitly labeled `ProviderDeclared`,
//! and paired with a trust policy that opts into declarations. The caller still
//! needs the pinned authority key and must sign the canonical lease envelope
//! separately.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use crate::eval::PlacementFragmentBindingsV2;
use crate::placement::{
    ActorGenerationIdV1, CandidateDecisionV1, CanonicalPlacementRecordV1, CapabilityAtomV1,
    CapabilityKeyV1, CapacityObservationV1, DischargedRequirementV1, EndiannessV1,
    EnvironmentRequirementV1, GenerationV1, NodeProfileV1, PlacementCandidateInputV1,
    PlacementEligibilityV1, PlacementReservationV1, PlacementTrustPolicyV1, PlacementWarrantV1,
    PlatformDescriptorV1, RecordAuthenticationV1, RecordAuthenticatorV1, RequirementAtomV1,
    SemanticDigestV1, TargetCapabilityModelV1, TargetDescriptorV1, UnixMillisV1,
    WarrantAssertionV1, WarrantDischargeV1, WarrantScopeV1, WarrantTierV1,
};

use super::{HostedPlacementEvidenceV2, SessionStateTierV2, HOSTED_PLACEMENT_EVIDENCE_SCHEMA_V2};

/// Freshness window for the co-located self-attested development evidence.
/// Production authorities derive freshness from independently observed
/// capacity records rather than this development-only constant.
pub const DEVELOPMENT_EVIDENCE_LIFETIME_MILLIS_V2: u64 = 4_000;

#[derive(Debug, Clone)]
pub struct LocalDevPlacementConfigV2 {
    pub node_id: String,
    pub node_generation: GenerationV1,
    pub profile_generation: GenerationV1,
    pub capacity_generation: GenerationV1,
    pub reservation: PlacementReservationV1,
    pub now_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub struct LocalDevPlacementProofV2 {
    pub evidence: HostedPlacementEvidenceV2,
    pub eligibility: PlacementEligibilityV1,
}

pub fn validate_local_dev_session_tier_v2(
    bindings: &PlacementFragmentBindingsV2,
    tier: SessionStateTierV2,
) -> Result<()> {
    let support = crate::backend_catalog::BackendRegistry::global()
        .state_support_for(bindings.canonical_backend())
        .context("prepared backend is absent from the current state-support catalog")?;
    tier.validate_backend_support(support)?;
    match tier {
        SessionStateTierV2::Stateless if !bindings.environment().is_fresh() => {
            bail!("Stateless development session requires a fresh fragment environment")
        }
        SessionStateTierV2::CheckpointRestore | SessionStateTierV2::LiveActorOnly
            if bindings.environment().is_fresh() =>
        {
            bail!("stateful development session requires an explicit persistent environment")
        }
        _ => Ok(()),
    }
}

pub fn local_dev_actor_generation_v2(
    bindings: &PlacementFragmentBindingsV2,
    target: &TargetDescriptorV1,
    generation: GenerationV1,
) -> Result<ActorGenerationIdV1> {
    let logical_environment =
        logical_environment(bindings.requirement_footprint().require_complete()?)
            .context("stateful prepared fragment omits its logical environment identity")?;
    Ok(ActorGenerationIdV1::new(
        logical_environment,
        bindings.backend_implementation_sha256().clone(),
        target.semantic_digest()?,
        bindings.sandbox_policy_sha256().clone(),
        bindings.backend_launch_generation().clone(),
        generation,
    ))
}

/// Build a short-lived proof from a co-located self-attested development
/// authority.
///
/// This helper does not probe the host. It synthesizes target support from the
/// exact admitted footprint, labels that support as provider-declared, and
/// records the corresponding opt-in trust policy in the evidence bundle.
///
/// `established_target` is used by later session commands to preserve the
/// target descriptor fixed by Open while refreshing profile/capacity records.
/// An actor is supplied only when the node reports an established stateful
/// actor. Open and the first stateful Execute instead use the prospective
/// logical environment already present in the footprint; they never claim a
/// physical actor identity that only the execution node can establish.
pub fn build_local_dev_placement_proof_v2(
    bindings: &PlacementFragmentBindingsV2,
    issuer_key: SemanticDigestV1,
    config: LocalDevPlacementConfigV2,
    established_target: Option<&TargetDescriptorV1>,
    actor_generation: Option<&ActorGenerationIdV1>,
    establishing_logical_environment: bool,
) -> Result<LocalDevPlacementProofV2> {
    if establishing_logical_environment && actor_generation.is_some() {
        bail!("prospective logical environment cannot claim an existing actor generation");
    }
    let requirements = bindings
        .requirement_footprint()
        .require_complete()
        .context("development placement requires a complete fragment footprint")?;
    let descriptor = match established_target {
        Some(descriptor) => {
            if descriptor.node_id() != config.node_id
                || descriptor.node_generation() != config.node_generation
            {
                bail!("established target belongs to a different node-state epoch");
            }
            if !descriptor
                .backend_implementations()
                .contains(bindings.backend_implementation())
            {
                bail!("established target omits the locally prepared backend implementation");
            }
            descriptor.clone()
        }
        None => local_target_descriptor(bindings, requirements, &config)?,
    };
    for requirement in requirements {
        if matches!(requirement, RequirementAtomV1::Environment(_)) {
            continue;
        }
        if !descriptor.supports_requirement(requirement)? {
            bail!(
                "co-located self-attested development authority does not declare support for {}",
                requirement.label()
            );
        }
    }

    let issued_at = UnixMillisV1::new(config.now_unix_ms.saturating_sub(1));
    let profile_expires = UnixMillisV1::new(
        config
            .now_unix_ms
            .checked_add(30_000)
            .context("development profile expiry overflow")?,
    );
    let short_expires = UnixMillisV1::new(
        config
            .now_unix_ms
            .checked_add(DEVELOPMENT_EVIDENCE_LIFETIME_MILLIS_V2)
            .context("development capacity expiry overflow")?,
    );
    let target_descriptor = descriptor.semantic_digest()?;
    let profile = NodeProfileV1::new(
        issuer_key.clone(),
        descriptor,
        config.profile_generation,
        issued_at,
        profile_expires,
    )?;
    let capacity = CapacityObservationV1::new(
        issuer_key.clone(),
        &config.node_id,
        target_descriptor.clone(),
        config.profile_generation,
        config.capacity_generation,
        config.reservation.cpu_slots().max(1),
        config.reservation.cpu_slots(),
        config.reservation.memory_bytes(),
        config.reservation.memory_bytes(),
        config.reservation.scratch_bytes(),
        config.reservation.scratch_bytes(),
        issued_at,
        short_expires,
    )?;

    let exact_scope = WarrantScopeV1::exact(
        bindings.operation_oir().clone(),
        target_descriptor,
        bindings.backend_implementation_sha256().clone(),
        bindings.realization_pipeline().clone(),
        SemanticDigestV1::from_sha256(bindings.source_sha256())?,
    );
    let mut entries = BTreeMap::new();
    let mut warrants = Vec::with_capacity(requirements.len().saturating_mul(2));
    for requirement in requirements {
        let static_warrant = PlacementWarrantV1::new(
            issuer_key.clone(),
            WarrantTierV1::StaticFootprint,
            WarrantScopeV1::new(
                Some(bindings.operation_oir().clone()),
                None,
                None,
                None,
                None,
            ),
            WarrantAssertionV1::OperationRequires(requirement.clone()),
            None,
            issued_at,
            None,
        )?;
        let target_warrant = PlacementWarrantV1::new(
            issuer_key.clone(),
            WarrantTierV1::ProviderDeclared,
            WarrantScopeV1::new(
                None,
                exact_scope.target_descriptor().cloned(),
                None,
                None,
                None,
            ),
            WarrantAssertionV1::TargetSupports(requirement.clone()),
            None,
            issued_at,
            Some(short_expires),
        )?;
        entries.insert(
            requirement.clone(),
            DischargedRequirementV1::new(static_warrant.id()?, target_warrant.id()?),
        );
        warrants.extend([static_warrant, target_warrant]);
    }
    warrants.sort_by_key(|warrant| {
        warrant
            .id()
            .expect("constructed development warrants have canonical identities")
    });
    let discharge = WarrantDischargeV1::new(exact_scope, entries)?;
    let trust_policy = PlacementTrustPolicyV1::declared();
    let prospective_logical_environment = establishing_logical_environment
        .then(|| logical_environment(requirements))
        .flatten();
    let evidence = HostedPlacementEvidenceV2 {
        schema: HOSTED_PLACEMENT_EVIDENCE_SCHEMA_V2.to_owned(),
        node_profile: profile,
        capacity_observation: capacity,
        requirement_footprint: bindings.requirement_footprint().clone(),
        warrant_discharge: discharge,
        warrants,
        trust_policy,
        reservation: config.reservation,
    };
    evidence.validate_shape()?;
    let candidate = PlacementCandidateInputV1 {
        profile: &evidence.node_profile,
        capacity: &evidence.capacity_observation,
        footprint: &evidence.requirement_footprint,
        discharge: &evidence.warrant_discharge,
        warrants: &evidence.warrants,
        trust_policy: &evidence.trust_policy,
        reservation: &evidence.reservation,
        actor_generation,
        prospective_logical_environment: prospective_logical_environment.as_ref(),
    };
    let eligibility = match candidate.evaluate_with_catalog(
        UnixMillisV1::new(config.now_unix_ms),
        &SelfAttestedDevelopmentAuthenticator,
        crate::backend_catalog::BackendRegistry::global(),
    ) {
        CandidateDecisionV1::Eligible { proof } => proof,
        CandidateDecisionV1::Ineligible { rejections } => {
            bail!("co-located self-attested development placement is ineligible: {rejections:?}")
        }
    };
    Ok(LocalDevPlacementProofV2 {
        evidence,
        eligibility,
    })
}

fn local_target_descriptor(
    bindings: &PlacementFragmentBindingsV2,
    requirements: &std::collections::BTreeSet<RequirementAtomV1>,
    config: &LocalDevPlacementConfigV2,
) -> Result<TargetDescriptorV1> {
    let mut capabilities = Vec::new();
    for requirement in requirements {
        match requirement {
            RequirementAtomV1::Capability(capability) => capabilities.push(capability.clone()),
            RequirementAtomV1::PortableValueKind(kind) => capabilities.push(CapabilityAtomV1::new(
                CapabilityKeyV1::new("portable-value", kind)?,
                1,
            )?),
            RequirementAtomV1::Preservation(property) => capabilities.push(CapabilityAtomV1::new(
                CapabilityKeyV1::new("preservation", property)?,
                1,
            )?),
            _ => {}
        }
    }
    TargetDescriptorV1::new(
        &config.node_id,
        "co-located self-attested development target",
        config.node_generation,
        TargetCapabilityModelV1::DownwardClosedIdeal,
        PlatformDescriptorV1::new(
            std::env::consts::OS,
            std::env::consts::ARCH,
            local_abi(),
            if cfg!(target_endian = "little") {
                EndiannessV1::Little
            } else {
                EndiannessV1::Big
            },
            usize::BITS as u16,
        )?,
        capabilities,
        Vec::<String>::new(),
        [bindings.backend_implementation().clone()],
    )
    .map_err(Into::into)
}

fn local_abi() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "msvc") {
        "msvc"
    } else {
        "gnu"
    }
}

fn logical_environment(
    requirements: &std::collections::BTreeSet<RequirementAtomV1>,
) -> Option<SemanticDigestV1> {
    requirements
        .iter()
        .find_map(|requirement| match requirement {
            RequirementAtomV1::Environment(EnvironmentRequirementV1::SameLogicalEnvironment {
                identity,
            }) => Some(identity.clone()),
            _ => None,
        })
}

struct SelfAttestedDevelopmentAuthenticator;

impl RecordAuthenticatorV1 for SelfAttestedDevelopmentAuthenticator {
    fn authenticate(&self, _record: &RecordAuthenticationV1) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::backend_catalog::BackendRegistry;
    use crate::eval::Evaluator;
    use crate::placement::{TaskAttemptIdV1, WarrantAssertionV1};

    use super::*;

    #[test]
    fn synthesized_support_is_provider_declared_with_explicit_trust() -> Result<()> {
        let mut evaluator = Evaluator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
            .with_registered_backends(BackendRegistry::global().registered_backend_tags());
        let task = SemanticDigestV1::hash_bytes(
            "ostadix/hosted/self-attested-development-test/v2",
            b"provider-declared-support",
        );
        let prepared = evaluator.prepare_placement_fragment(
            "bash^(\nprintf '2'\n)_bash",
            TaskAttemptIdV1::new(task, GenerationV1::new(1)?),
        )?;
        let proof = build_local_dev_placement_proof_v2(
            prepared.bindings(),
            SemanticDigestV1::hash_bytes(
                "ostadix/hosted/self-attested-development-authority/v2",
                b"test-authority",
            ),
            LocalDevPlacementConfigV2 {
                node_id: "self-attested-development-node".to_owned(),
                node_generation: GenerationV1::new(1)?,
                profile_generation: GenerationV1::new(1)?,
                capacity_generation: GenerationV1::new(1)?,
                reservation: PlacementReservationV1::new(1, 1024 * 1024, 0)?,
                now_unix_ms: 1_000_000,
            },
            None,
            None,
            true,
        )?;

        let target_support = proof
            .evidence
            .warrants
            .iter()
            .filter(|warrant| matches!(warrant.assertion(), WarrantAssertionV1::TargetSupports(_)))
            .collect::<Vec<_>>();
        assert!(!target_support.is_empty());
        assert!(target_support
            .iter()
            .all(|warrant| warrant.tier() == WarrantTierV1::ProviderDeclared));
        assert_eq!(
            proof.evidence.trust_policy,
            PlacementTrustPolicyV1::declared()
        );
        assert!(proof
            .evidence
            .trust_policy
            .allows_positive(WarrantTierV1::ProviderDeclared));
        Ok(())
    }
}
