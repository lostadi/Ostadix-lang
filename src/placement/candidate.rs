use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    ActorGenerationIdV1, CanonicalPlacementRecordV1, CapacityObservationV1, NodeProfileV1,
    PlacementReservationV1, PlacementTrustPolicyV1, PlacementValidationError, PlacementWarrantV1,
    RecordAuthenticatorV1, RequirementAtomV1, RequirementFootprintV1, ResourceKindV1,
    SemanticDigestV1, UnixMillisV1, WarrantDischargeV1,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "detail")]
pub enum CandidateRejectionV1 {
    InvalidProfile(String),
    InvalidCapacity(String),
    ConservativeUnknown(Vec<String>),
    Unsatisfiable(Vec<String>),
    UnsupportedRequirement(String),
    ReservationBelowRequirement(String),
    InsufficientCapacity,
    InvalidDischarge(String),
    ActorGenerationMismatch(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementEligibilityV1 {
    target_descriptor: SemanticDigestV1,
    node_profile: SemanticDigestV1,
    capacity_observation: SemanticDigestV1,
    requirement_footprint: SemanticDigestV1,
    warrant_discharge: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    reservation: PlacementReservationV1,
}

impl PlacementEligibilityV1 {
    pub fn target_descriptor(&self) -> &SemanticDigestV1 {
        &self.target_descriptor
    }

    pub fn reservation(&self) -> &PlacementReservationV1 {
        &self.reservation
    }
}

impl CanonicalPlacementRecordV1 for PlacementEligibilityV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/eligibility/v1";
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "decision")]
pub enum CandidateDecisionV1 {
    Eligible {
        proof: PlacementEligibilityV1,
    },
    Ineligible {
        rejections: BTreeSet<CandidateRejectionV1>,
    },
}

impl CandidateDecisionV1 {
    pub fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible { .. })
    }

    pub fn rejections(&self) -> Option<&BTreeSet<CandidateRejectionV1>> {
        match self {
            Self::Eligible { .. } => None,
            Self::Ineligible { rejections } => Some(rejections),
        }
    }
}

pub struct PlacementCandidateInputV1<'a> {
    pub profile: &'a NodeProfileV1,
    pub capacity: &'a CapacityObservationV1,
    pub footprint: &'a RequirementFootprintV1,
    pub discharge: &'a WarrantDischargeV1,
    pub warrants: &'a [PlacementWarrantV1],
    pub trust_policy: &'a PlacementTrustPolicyV1,
    pub reservation: &'a PlacementReservationV1,
    pub actor_generation: Option<&'a ActorGenerationIdV1>,
}

impl<'a> PlacementCandidateInputV1<'a> {
    pub fn evaluate(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> CandidateDecisionV1 {
        let mut rejections = BTreeSet::new();
        if let Err(error) = self.profile.validate_at(now, authenticator) {
            rejections.insert(CandidateRejectionV1::InvalidProfile(error.to_string()));
        }
        if let Err(error) = self
            .capacity
            .validate_for_profile(self.profile, now, authenticator)
        {
            rejections.insert(CandidateRejectionV1::InvalidCapacity(error.to_string()));
        }

        let requirements = match self.footprint.require_complete() {
            Ok(requirements) => Some(requirements),
            Err(PlacementValidationError::ConservativeUnknown(reasons)) => {
                rejections.insert(CandidateRejectionV1::ConservativeUnknown(reasons));
                None
            }
            Err(PlacementValidationError::Unsatisfiable(reasons)) => {
                rejections.insert(CandidateRejectionV1::Unsatisfiable(reasons));
                None
            }
            Err(error) => {
                rejections.insert(CandidateRejectionV1::InvalidDischarge(error.to_string()));
                None
            }
        };

        if let Some(requirements) = requirements {
            for requirement in requirements {
                match requirement {
                    RequirementAtomV1::ResourceMinimum { resource, amount } => {
                        let reserved = match resource {
                            ResourceKindV1::CpuSlots => u64::from(self.reservation.cpu_slots()),
                            ResourceKindV1::MemoryBytes => self.reservation.memory_bytes(),
                            ResourceKindV1::ScratchBytes => self.reservation.scratch_bytes(),
                        };
                        if reserved < *amount {
                            rejections.insert(CandidateRejectionV1::ReservationBelowRequirement(
                                requirement.label(),
                            ));
                        }
                    }
                    RequirementAtomV1::Environment(environment) => {
                        use super::EnvironmentRequirementV1;
                        let matched = match environment {
                            EnvironmentRequirementV1::Stateless
                            | EnvironmentRequirementV1::Ephemeral => true,
                            EnvironmentRequirementV1::SameLogicalEnvironment { identity } => self
                                .actor_generation
                                .is_some_and(|actor| actor.logical_environment() == identity),
                            EnvironmentRequirementV1::SameActorGeneration { identity } => self
                                .actor_generation
                                .and_then(|actor| actor.semantic_digest().ok())
                                .is_some_and(|actual| &actual == identity),
                        };
                        if !matched {
                            rejections.insert(CandidateRejectionV1::ActorGenerationMismatch(
                                requirement.label(),
                            ));
                        }
                    }
                    _ => match self.profile.descriptor().supports_requirement(requirement) {
                        Ok(true) => {}
                        Ok(false) => {
                            rejections.insert(CandidateRejectionV1::UnsupportedRequirement(
                                requirement.label(),
                            ));
                        }
                        Err(error) => {
                            rejections.insert(CandidateRejectionV1::UnsupportedRequirement(
                                error.to_string(),
                            ));
                        }
                    },
                }
            }
        }

        if !self.capacity.fits(self.reservation) {
            rejections.insert(CandidateRejectionV1::InsufficientCapacity);
        }
        if let Err(error) = self.discharge.validate(
            self.footprint,
            self.warrants,
            self.trust_policy,
            now,
            authenticator,
        ) {
            rejections.insert(CandidateRejectionV1::InvalidDischarge(error.to_string()));
        }

        let target_digest = match self.profile.descriptor_digest() {
            Ok(digest) => digest,
            Err(error) => {
                rejections.insert(CandidateRejectionV1::InvalidProfile(error.to_string()));
                return CandidateDecisionV1::Ineligible { rejections };
            }
        };
        if self.discharge.exact_scope().target_descriptor() != Some(&target_digest) {
            rejections.insert(CandidateRejectionV1::InvalidDischarge(
                "discharge target does not match the candidate descriptor".to_owned(),
            ));
        }

        if !rejections.is_empty() {
            return CandidateDecisionV1::Ineligible { rejections };
        }

        let proof = (|| {
            Ok::<_, PlacementValidationError>(PlacementEligibilityV1 {
                target_descriptor: target_digest,
                node_profile: self.profile.semantic_digest()?,
                capacity_observation: self.capacity.semantic_digest()?,
                requirement_footprint: self.footprint.semantic_digest()?,
                warrant_discharge: self.discharge.semantic_digest()?,
                trust_policy: self.trust_policy.semantic_digest()?,
                reservation: self.reservation.clone(),
            })
        })();
        match proof {
            Ok(proof) => CandidateDecisionV1::Eligible { proof },
            Err(error) => CandidateDecisionV1::Ineligible {
                rejections: [CandidateRejectionV1::InvalidDischarge(error.to_string())]
                    .into_iter()
                    .collect(),
            },
        }
    }
}

/// Canonically ordered decisions for one requirement/policy pair.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSetV1 {
    requirement_footprint: SemanticDigestV1,
    trust_policy: SemanticDigestV1,
    candidates: BTreeMap<SemanticDigestV1, CandidateDecisionV1>,
}

impl CandidateSetV1 {
    pub fn new(
        footprint: &RequirementFootprintV1,
        policy: &PlacementTrustPolicyV1,
        candidates: impl IntoIterator<Item = (SemanticDigestV1, CandidateDecisionV1)>,
    ) -> Result<Self, PlacementValidationError> {
        let mut ordered = BTreeMap::new();
        for (target, decision) in candidates {
            if ordered.insert(target.clone(), decision).is_some() {
                return Err(PlacementValidationError::Duplicate {
                    kind: "placement candidate",
                    value: target.to_string(),
                });
            }
        }
        Ok(Self {
            requirement_footprint: footprint.semantic_digest()?,
            trust_policy: policy.semantic_digest()?,
            candidates: ordered,
        })
    }

    pub fn candidates(&self) -> &BTreeMap<SemanticDigestV1, CandidateDecisionV1> {
        &self.candidates
    }

    pub fn eligible_targets(&self) -> impl Iterator<Item = &SemanticDigestV1> {
        self.candidates
            .iter()
            .filter_map(|(target, decision)| decision.is_eligible().then_some(target))
    }
}

impl CanonicalPlacementRecordV1 for CandidateSetV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/candidate-set/v1";
}
