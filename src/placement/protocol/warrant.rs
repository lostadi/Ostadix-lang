use std::collections::BTreeMap;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::world::ArtifactId;

use super::digest::{validate_fresh, validate_window};
use super::records::scope_mismatch;
use super::{
    CanonicalPlacementRecordV1, PlacementValidationError, RecordAuthenticationV1,
    RecordAuthenticatorV1, RequirementAtomV1, RequirementFootprintV1, SemanticDigestV1,
    UnixMillisV1,
};

pub const MAX_DISCOVERED_WARRANT_LIFETIME_MS: u64 = 60_000;
pub const MAX_DECLARED_WARRANT_LIFETIME_MS: u64 = 5 * 60_000;
pub const MAX_HISTORICAL_WARRANT_LIFETIME_MS: u64 = 24 * 60 * 60_000;
pub const MIN_HISTORICAL_OBSERVATIONS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WarrantTierV1 {
    StaticFootprint,
    RuntimeDiscovered,
    ProviderDeclared,
    HistoricalObservation,
}

impl std::fmt::Display for WarrantTierV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}", self)
    }
}

/// Independent trust controls.  The tiers are not encoded as one scalar order:
/// compiler-static facts and runtime-discovered target facts have distinct
/// roles, while declared and historical positive facts require opt-in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementTrustPolicyV1 {
    allow_provider_declared_positive: bool,
    allow_historical_positive: bool,
}

impl PlacementTrustPolicyV1 {
    pub const fn strict() -> Self {
        Self {
            allow_provider_declared_positive: false,
            allow_historical_positive: false,
        }
    }

    pub const fn declared() -> Self {
        Self {
            allow_provider_declared_positive: true,
            allow_historical_positive: false,
        }
    }

    pub const fn historical() -> Self {
        Self {
            allow_provider_declared_positive: false,
            allow_historical_positive: true,
        }
    }

    pub const fn declared_and_historical() -> Self {
        Self {
            allow_provider_declared_positive: true,
            allow_historical_positive: true,
        }
    }

    pub fn allows_positive(&self, tier: WarrantTierV1) -> bool {
        match tier {
            WarrantTierV1::RuntimeDiscovered => true,
            WarrantTierV1::ProviderDeclared => self.allow_provider_declared_positive,
            WarrantTierV1::HistoricalObservation => self.allow_historical_positive,
            WarrantTierV1::StaticFootprint => false,
        }
    }
}

impl Default for PlacementTrustPolicyV1 {
    fn default() -> Self {
        Self::strict()
    }
}

impl CanonicalPlacementRecordV1 for PlacementTrustPolicyV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/trust-policy/v1";
}

/// Scope coordinates of a warrant.  `None` means that coordinate is general;
/// historical evidence is never general and must bind every coordinate.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarrantScopeV1 {
    operation_oir: Option<ArtifactId>,
    target_descriptor: Option<SemanticDigestV1>,
    backend_implementation: Option<SemanticDigestV1>,
    realization_pipeline: Option<SemanticDigestV1>,
    input_equivalence_class: Option<SemanticDigestV1>,
}

impl WarrantScopeV1 {
    pub fn new(
        operation_oir: Option<ArtifactId>,
        target_descriptor: Option<SemanticDigestV1>,
        backend_implementation: Option<SemanticDigestV1>,
        realization_pipeline: Option<SemanticDigestV1>,
        input_equivalence_class: Option<SemanticDigestV1>,
    ) -> Self {
        Self {
            operation_oir,
            target_descriptor,
            backend_implementation,
            realization_pipeline,
            input_equivalence_class,
        }
    }

    pub fn exact(
        operation_oir: ArtifactId,
        target_descriptor: SemanticDigestV1,
        backend_implementation: SemanticDigestV1,
        realization_pipeline: SemanticDigestV1,
        input_equivalence_class: SemanticDigestV1,
    ) -> Self {
        Self::new(
            Some(operation_oir),
            Some(target_descriptor),
            Some(backend_implementation),
            Some(realization_pipeline),
            Some(input_equivalence_class),
        )
    }

    pub fn operation_oir(&self) -> Option<&ArtifactId> {
        self.operation_oir.as_ref()
    }

    pub fn target_descriptor(&self) -> Option<&SemanticDigestV1> {
        self.target_descriptor.as_ref()
    }

    pub fn backend_implementation(&self) -> Option<&SemanticDigestV1> {
        self.backend_implementation.as_ref()
    }

    pub fn realization_pipeline(&self) -> Option<&SemanticDigestV1> {
        self.realization_pipeline.as_ref()
    }

    pub fn input_equivalence_class(&self) -> Option<&SemanticDigestV1> {
        self.input_equivalence_class.as_ref()
    }

    fn covers(&self, exact: &Self) -> Result<(), PlacementValidationError> {
        compare_optional_artifact(
            "warrant operation",
            &self.operation_oir,
            &exact.operation_oir,
        )?;
        compare_optional(
            "warrant target descriptor",
            &self.target_descriptor,
            &exact.target_descriptor,
        )?;
        compare_optional(
            "warrant backend implementation",
            &self.backend_implementation,
            &exact.backend_implementation,
        )?;
        compare_optional(
            "warrant realization pipeline",
            &self.realization_pipeline,
            &exact.realization_pipeline,
        )?;
        compare_optional(
            "warrant input equivalence class",
            &self.input_equivalence_class,
            &exact.input_equivalence_class,
        )
    }

    fn is_exact(&self) -> bool {
        self.operation_oir.is_some()
            && self.target_descriptor.is_some()
            && self.backend_implementation.is_some()
            && self.realization_pipeline.is_some()
            && self.input_equivalence_class.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "requirement")]
pub enum WarrantAssertionV1 {
    OperationRequires(RequirementAtomV1),
    TargetSupports(RequirementAtomV1),
    TargetRejects(RequirementAtomV1),
}

impl WarrantAssertionV1 {
    pub fn requirement(&self) -> &RequirementAtomV1 {
        match self {
            Self::OperationRequires(requirement)
            | Self::TargetSupports(requirement)
            | Self::TargetRejects(requirement) => requirement,
        }
    }
}

/// One authenticated fact in a placement proof. Signature bytes and key
/// resolution remain outside this transport-neutral payload.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementWarrantV1 {
    issuer_key: SemanticDigestV1,
    tier: WarrantTierV1,
    scope: WarrantScopeV1,
    assertion: WarrantAssertionV1,
    observation_count: Option<u32>,
    issued_at: UnixMillisV1,
    expires_at: Option<UnixMillisV1>,
}

impl PlacementWarrantV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issuer_key: SemanticDigestV1,
        tier: WarrantTierV1,
        scope: WarrantScopeV1,
        assertion: WarrantAssertionV1,
        observation_count: Option<u32>,
        issued_at: UnixMillisV1,
        expires_at: Option<UnixMillisV1>,
    ) -> Result<Self, PlacementValidationError> {
        match tier {
            WarrantTierV1::StaticFootprint => {
                if !matches!(assertion, WarrantAssertionV1::OperationRequires(_))
                    || scope.operation_oir.is_none()
                    || expires_at.is_some()
                    || observation_count.is_some()
                {
                    return Err(PlacementValidationError::InvalidToken {
                        field: "static warrant shape",
                        value: format!("{assertion:?}"),
                    });
                }
            }
            WarrantTierV1::RuntimeDiscovered => {
                validate_target_warrant_shape(
                    "runtime-discovered warrant",
                    &scope,
                    &assertion,
                    observation_count,
                    issued_at,
                    expires_at,
                    MAX_DISCOVERED_WARRANT_LIFETIME_MS,
                )?;
            }
            WarrantTierV1::ProviderDeclared => {
                validate_target_warrant_shape(
                    "provider-declared warrant",
                    &scope,
                    &assertion,
                    observation_count,
                    issued_at,
                    expires_at,
                    MAX_DECLARED_WARRANT_LIFETIME_MS,
                )?;
            }
            WarrantTierV1::HistoricalObservation => {
                validate_target_warrant_shape(
                    "historical warrant",
                    &scope,
                    &assertion,
                    observation_count,
                    issued_at,
                    expires_at,
                    MAX_HISTORICAL_WARRANT_LIFETIME_MS,
                )?;
                if !scope.is_exact() {
                    return Err(PlacementValidationError::InvalidToken {
                        field: "historical warrant scope",
                        value: "scope is not exact".to_owned(),
                    });
                }
                let observed = observation_count.unwrap_or_default();
                if observed < MIN_HISTORICAL_OBSERVATIONS {
                    return Err(
                        PlacementValidationError::InsufficientHistoricalObservations {
                            observed,
                            minimum: MIN_HISTORICAL_OBSERVATIONS,
                        },
                    );
                }
            }
        }
        Ok(Self {
            issuer_key,
            tier,
            scope,
            assertion,
            observation_count,
            issued_at,
            expires_at,
        })
    }

    pub fn tier(&self) -> WarrantTierV1 {
        self.tier
    }

    pub fn scope(&self) -> &WarrantScopeV1 {
        &self.scope
    }

    pub fn assertion(&self) -> &WarrantAssertionV1 {
        &self.assertion
    }

    pub fn issuer_key(&self) -> &SemanticDigestV1 {
        &self.issuer_key
    }

    pub fn expires_at(&self) -> Option<UnixMillisV1> {
        self.expires_at
    }

    pub fn id(&self) -> Result<SemanticDigestV1, PlacementValidationError> {
        self.semantic_digest()
    }

    pub fn validate_at(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        if now < self.issued_at {
            return Err(PlacementValidationError::NotYetValid {
                record: "placement warrant",
            });
        }
        if let Some(expires_at) = self.expires_at {
            validate_fresh("placement warrant", self.issued_at, expires_at, now)?;
        }
        let authentication =
            RecordAuthenticationV1::new("placement warrant", self.issuer_key.clone(), self.id()?);
        if authenticator.authenticate(&authentication) {
            Ok(())
        } else {
            Err(PlacementValidationError::Unauthenticated {
                record: "placement warrant",
            })
        }
    }

    fn is_current_and_authenticated(
        &self,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> bool {
        self.validate_at(now, authenticator).is_ok()
    }
}

impl CanonicalPlacementRecordV1 for PlacementWarrantV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/warrant/v1";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementWarrantWireV1 {
    issuer_key: SemanticDigestV1,
    tier: WarrantTierV1,
    scope: WarrantScopeV1,
    assertion: WarrantAssertionV1,
    observation_count: Option<u32>,
    issued_at: UnixMillisV1,
    expires_at: Option<UnixMillisV1>,
}

impl<'de> Deserialize<'de> for PlacementWarrantV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PlacementWarrantWireV1::deserialize(deserializer)?;
        Self::new(
            wire.issuer_key,
            wire.tier,
            wire.scope,
            wire.assertion,
            wire.observation_count,
            wire.issued_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// The exact compiler-side and target-side warrants chosen for one atom.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DischargedRequirementV1 {
    static_warrant: SemanticDigestV1,
    target_warrant: SemanticDigestV1,
}

impl DischargedRequirementV1 {
    pub fn new(static_warrant: SemanticDigestV1, target_warrant: SemanticDigestV1) -> Self {
        Self {
            static_warrant,
            target_warrant,
        }
    }

    pub fn static_warrant(&self) -> &SemanticDigestV1 {
        &self.static_warrant
    }

    pub fn target_warrant(&self) -> &SemanticDigestV1 {
        &self.target_warrant
    }
}

/// Complete, auditable mapping from every required atom to the two facts that
/// justify placement.  There are no wildcard or catch-all entries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WarrantDischargeV1 {
    exact_scope: WarrantScopeV1,
    entries: BTreeMap<RequirementAtomV1, DischargedRequirementV1>,
}

impl WarrantDischargeV1 {
    pub fn new(
        exact_scope: WarrantScopeV1,
        entries: BTreeMap<RequirementAtomV1, DischargedRequirementV1>,
    ) -> Result<Self, PlacementValidationError> {
        if !exact_scope.is_exact() {
            return Err(PlacementValidationError::InvalidToken {
                field: "warrant discharge scope",
                value: "scope is not exact".to_owned(),
            });
        }
        Ok(Self {
            exact_scope,
            entries,
        })
    }

    pub fn exact_scope(&self) -> &WarrantScopeV1 {
        &self.exact_scope
    }

    pub fn entries(&self) -> &BTreeMap<RequirementAtomV1, DischargedRequirementV1> {
        &self.entries
    }

    pub fn validate(
        &self,
        footprint: &RequirementFootprintV1,
        warrants: &[PlacementWarrantV1],
        policy: &PlacementTrustPolicyV1,
        now: UnixMillisV1,
        authenticator: &impl RecordAuthenticatorV1,
    ) -> Result<(), PlacementValidationError> {
        let requirements = footprint.require_complete()?;
        for requirement in requirements {
            if !self.entries.contains_key(requirement) {
                return Err(PlacementValidationError::MissingDischarge(
                    requirement.label(),
                ));
            }
        }
        for requirement in self.entries.keys() {
            if !requirements.contains(requirement) {
                return Err(PlacementValidationError::ExtraneousDischarge(
                    requirement.label(),
                ));
            }
        }

        let mut by_id = BTreeMap::new();
        for warrant in warrants {
            let id = warrant.id()?;
            if by_id.insert(id.clone(), warrant).is_some() {
                return Err(PlacementValidationError::Duplicate {
                    kind: "placement warrant",
                    value: id.to_string(),
                });
            }
        }

        for requirement in requirements {
            // Any applicable, current, authenticated negative is a veto.  This
            // includes declarations because declarations may always strengthen
            // constraints; most importantly, a discovered negative cannot be
            // overridden by a declared or historical positive.
            if warrants.iter().any(|warrant| {
                matches!(
                    warrant.assertion(),
                    WarrantAssertionV1::TargetRejects(candidate) if candidate == requirement
                ) && warrant.scope.covers(&self.exact_scope).is_ok()
                    && warrant.is_current_and_authenticated(now, authenticator)
            }) {
                return Err(PlacementValidationError::NegativeVeto(requirement.label()));
            }

            let discharge = self.entries.get(requirement).expect("exact key checked");
            let static_warrant = by_id.get(discharge.static_warrant()).ok_or_else(|| {
                PlacementValidationError::MissingWarrant(discharge.static_warrant().to_string())
            })?;
            static_warrant.validate_at(now, authenticator)?;
            static_warrant.scope.covers(&self.exact_scope)?;
            if static_warrant.tier != WarrantTierV1::StaticFootprint
                || !matches!(
                    static_warrant.assertion(),
                    WarrantAssertionV1::OperationRequires(candidate) if candidate == requirement
                )
            {
                return Err(PlacementValidationError::WarrantAssertionMismatch(
                    requirement.label(),
                ));
            }

            let target_warrant = by_id.get(discharge.target_warrant()).ok_or_else(|| {
                PlacementValidationError::MissingWarrant(discharge.target_warrant().to_string())
            })?;
            target_warrant.validate_at(now, authenticator)?;
            target_warrant.scope.covers(&self.exact_scope)?;
            if !policy.allows_positive(target_warrant.tier) {
                return Err(PlacementValidationError::WarrantTierNotAllowed(
                    target_warrant.tier.to_string(),
                ));
            }
            if !matches!(
                target_warrant.assertion(),
                WarrantAssertionV1::TargetSupports(candidate) if candidate == requirement
            ) {
                return Err(PlacementValidationError::WarrantAssertionMismatch(
                    requirement.label(),
                ));
            }
        }
        Ok(())
    }
}

impl CanonicalPlacementRecordV1 for WarrantDischargeV1 {
    const DIGEST_DOMAIN: &'static str = "ostadix/placement/warrant-discharge/v1";
}

impl Serialize for WarrantDischargeV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let entries: Vec<_> = self
            .entries
            .iter()
            .map(|(requirement, discharge)| WarrantDischargeEntryWireV1 {
                requirement: requirement.clone(),
                discharge: discharge.clone(),
            })
            .collect();
        let mut state = serializer.serialize_struct("WarrantDischargeV1", 2)?;
        state.serialize_field("exact_scope", &self.exact_scope)?;
        state.serialize_field("entries", &entries)?;
        state.end()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WarrantDischargeEntryWireV1 {
    requirement: RequirementAtomV1,
    discharge: DischargedRequirementV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WarrantDischargeWireV1 {
    exact_scope: WarrantScopeV1,
    entries: Vec<WarrantDischargeEntryWireV1>,
}

impl<'de> Deserialize<'de> for WarrantDischargeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WarrantDischargeWireV1::deserialize(deserializer)?;
        let mut entries = BTreeMap::new();
        for entry in wire.entries {
            if entries
                .insert(entry.requirement.clone(), entry.discharge)
                .is_some()
            {
                return Err(serde::de::Error::custom(format!(
                    "duplicate warrant discharge for {}",
                    entry.requirement.label()
                )));
            }
        }
        Self::new(wire.exact_scope, entries).map_err(serde::de::Error::custom)
    }
}

fn validate_target_warrant_shape(
    record: &'static str,
    scope: &WarrantScopeV1,
    assertion: &WarrantAssertionV1,
    observation_count: Option<u32>,
    issued_at: UnixMillisV1,
    expires_at: Option<UnixMillisV1>,
    maximum_ms: u64,
) -> Result<(), PlacementValidationError> {
    if scope.target_descriptor.is_none()
        || matches!(assertion, WarrantAssertionV1::OperationRequires(_))
    {
        return Err(PlacementValidationError::InvalidToken {
            field: "target warrant shape",
            value: format!("{assertion:?}"),
        });
    }
    if record != "historical warrant" && observation_count.is_some() {
        return Err(PlacementValidationError::InvalidToken {
            field: "warrant observation count",
            value: "unexpected observation count".to_owned(),
        });
    }
    let expires_at = expires_at.ok_or(PlacementValidationError::InvalidValidity { record })?;
    validate_window(record, issued_at, expires_at, maximum_ms)
}

fn compare_optional(
    field: &'static str,
    asserted: &Option<SemanticDigestV1>,
    exact: &Option<SemanticDigestV1>,
) -> Result<(), PlacementValidationError> {
    if let Some(asserted) = asserted {
        match exact {
            Some(exact) if asserted == exact => Ok(()),
            Some(exact) => Err(scope_mismatch(
                field,
                exact.to_string(),
                asserted.to_string(),
            )),
            None => Err(scope_mismatch(field, "present", "missing")),
        }
    } else {
        Ok(())
    }
}

fn compare_optional_artifact(
    field: &'static str,
    asserted: &Option<ArtifactId>,
    exact: &Option<ArtifactId>,
) -> Result<(), PlacementValidationError> {
    if let Some(asserted) = asserted {
        match exact {
            Some(exact) if asserted == exact => Ok(()),
            Some(exact) => Err(scope_mismatch(
                field,
                exact.as_sha256(),
                asserted.as_sha256(),
            )),
            None => Err(scope_mismatch(field, "present", "missing")),
        }
    } else {
        Ok(())
    }
}
