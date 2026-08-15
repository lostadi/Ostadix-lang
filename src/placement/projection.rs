use crate::environment::EnvironmentRefV2;
use crate::ir::{ExecutionMode, ExecutionPlan, OIrProgram, PlanNodeId, PlanNodeKind};

use super::{
    CapabilityAtomV1, CapabilityKeyV1, EffectRequirementV1, EnvironmentRequirementV1,
    PlacementValidationError, RequirementAtomV1, RequirementFootprintV1, ResourceKindV1,
    SemanticDigestV1,
};

pub const SESSION_SERIALIZED_OPAQUE_EFFECTS_NAMESPACE_V1: &str = "execution";
pub const SESSION_SERIALIZED_OPAQUE_EFFECTS_NAME_V1: &str = "session-serialized-opaque-effects";
pub const SESSION_SERIALIZED_OPAQUE_EFFECTS_CAPABILITY_V1: &str =
    "execution/session-serialized-opaque-effects@1";

/// Semantic authority available while projecting one plan island.
///
/// `AutonomousUnknownEffects` is never inferred from an ephemeral environment;
/// callers may select it only for syntax already nested under an explicit
/// `autonomous(...)` policy region or for a sealed single-fragment authority
/// whose consuming runtime forbids recursive evaluator callbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlacementIntentV1 {
    #[default]
    Strict,
    AutonomousUnknownEffects,
    /// The exact persistent actor/session serializes this one opaque shim
    /// command.  This does not claim compiler-known effects, purity,
    /// replayability, or isolation from other sessions; the selected target
    /// must explicitly advertise the matching execution capability.
    SessionSerializedOpaqueEffects,
}

/// Derive a placement footprint for one executable-plan node.
///
/// Structural children such as text are identity elements because their
/// enclosing `Exec` owns realization. Evaluator-local control/scope operations
/// remain conservative unknowns until portable input/state packaging exists.
pub fn requirement_footprint_for_plan_node(
    kind: &PlanNodeKind,
    intent: PlacementIntentV1,
) -> Result<RequirementFootprintV1, PlacementValidationError> {
    let PlanNodeKind::Exec {
        env_id, backend, ..
    } = kind
    else {
        return match kind {
            PlanNodeKind::Text => Ok(RequirementFootprintV1::empty()),
            PlanNodeKind::Load { .. } | PlanNodeKind::Store { .. } => {
                RequirementFootprintV1::conservative_unknown(
                    [],
                    ["scope state is not packaged as a portable placement input".to_string()],
                )
            }
            _ => RequirementFootprintV1::conservative_unknown(
                [],
                ["evaluator control operation remains coordinator-local".to_string()],
            ),
        };
    };

    let environment = EnvironmentRefV2::from_encoded(*env_id);
    let mut atoms = Vec::new();
    atoms.push(RequirementAtomV1::resource_minimum(
        ResourceKindV1::CpuSlots,
        1,
    )?);

    let mut reasons = Vec::new();
    match backend.specification_sha256.as_deref() {
        Some(digest) => atoms.push(RequirementAtomV1::BackendSpecification(
            SemanticDigestV1::from_sha256(digest.to_string())?,
        )),
        None => reasons.push("backend has no catalog specification digest".to_string()),
    }

    let environment_requirement = match backend.execution {
        ExecutionMode::InlineValue => EnvironmentRequirementV1::Stateless,
        ExecutionMode::Shim if environment.is_fresh() => EnvironmentRequirementV1::Ephemeral,
        ExecutionMode::Shim => EnvironmentRequirementV1::SameLogicalEnvironment {
            identity: logical_environment_digest(
                backend.canonical.as_str(),
                backend.specification_sha256.as_deref(),
                environment,
            ),
        },
        ExecutionMode::InlineAst => {
            reasons.push("inline AST execution remains coordinator-local".to_string());
            EnvironmentRequirementV1::Stateless
        }
    };
    atoms.push(RequirementAtomV1::Environment(environment_requirement));

    if backend.pure && backend.execution == ExecutionMode::InlineValue {
        atoms.push(RequirementAtomV1::Effect(
            EffectRequirementV1::CompilerVerifiedPure,
        ));
    } else if backend.execution == ExecutionMode::Shim
        && environment.is_fresh()
        && intent == PlacementIntentV1::AutonomousUnknownEffects
    {
        atoms.push(RequirementAtomV1::Effect(
            EffectRequirementV1::AutonomousUnknownEffects,
        ));
    } else if backend.execution == ExecutionMode::Shim
        && environment.is_persistent()
        && intent == PlacementIntentV1::SessionSerializedOpaqueEffects
    {
        atoms.push(RequirementAtomV1::Capability(CapabilityAtomV1::new(
            CapabilityKeyV1::new(
                SESSION_SERIALIZED_OPAQUE_EFFECTS_NAMESPACE_V1,
                SESSION_SERIALIZED_OPAQUE_EFFECTS_NAME_V1,
            )?,
            1,
        )?));
    } else if backend.execution == ExecutionMode::Shim {
        reasons.push("hosted shim effects are not compiler-closed".to_string());
    }

    if reasons.is_empty() {
        Ok(RequirementFootprintV1::complete(atoms))
    } else {
        RequirementFootprintV1::conservative_unknown(atoms, reasons)
    }
}

/// Join per-node requirements for an execution island. ACI of the carrier
/// makes this independent of fusion order and safe for parallel coarseners.
pub fn requirement_footprint_for_island(
    plan: &ExecutionPlan,
    nodes: impl IntoIterator<Item = PlanNodeId>,
    intent: PlacementIntentV1,
) -> Result<RequirementFootprintV1, PlacementValidationError> {
    let mut footprint = RequirementFootprintV1::empty();
    for node in nodes {
        let Some(plan_node) = plan.nodes.get(node.0) else {
            return Err(PlacementValidationError::PlanNodeOutOfBounds {
                node: node.0,
                len: plan.nodes.len(),
            });
        };
        footprint = footprint.join(&requirement_footprint_for_plan_node(
            &plan_node.kind,
            intent,
        )?);
    }
    Ok(footprint)
}

/// Project one canonical program/plan node, recognizing an explicit enclosing
/// `autonomous(...)` region as the only authority for unknown hosted effects.
pub fn requirement_footprint_for_program_node(
    program: &OIrProgram,
    plan: &ExecutionPlan,
    node: PlanNodeId,
) -> Result<RequirementFootprintV1, PlacementValidationError> {
    let Some(plan_node) = plan.nodes.get(node.0) else {
        return Err(PlacementValidationError::PlanNodeOutOfBounds {
            node: node.0,
            len: plan.nodes.len(),
        });
    };
    let flattened = program.flatten_for_plan();
    let Some(oir) = flattened.get(node.0) else {
        return Err(PlacementValidationError::PlanNodeOutOfBounds {
            node: node.0,
            len: flattened.len(),
        });
    };
    let intent = if crate::hgraph::from_oir::autonomous_ephemeral_group(plan, node, oir).is_some() {
        PlacementIntentV1::AutonomousUnknownEffects
    } else {
        PlacementIntentV1::Strict
    };
    requirement_footprint_for_plan_node(&plan_node.kind, intent)
}

fn logical_environment_digest(
    canonical_backend: &str,
    specification_sha256: Option<&str>,
    environment: EnvironmentRefV2,
) -> SemanticDigestV1 {
    let mut bytes = Vec::with_capacity(canonical_backend.len() + 1 + 64 + 4);
    bytes.extend_from_slice(canonical_backend.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(specification_sha256.unwrap_or("unknown").as_bytes());
    bytes.extend_from_slice(&environment.encoded().to_be_bytes());
    SemanticDigestV1::hash_bytes("ostadix/placement/logical-environment/v1", &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::BackendRegistry;

    fn exec(lang: &str, environment: EnvironmentRefV2) -> PlanNodeKind {
        PlanNodeKind::Exec {
            lang: lang.to_string(),
            env_id: environment.encoded(),
            attr: None,
            backend: BackendRegistry::global().interface_for(lang),
        }
    }

    #[test]
    fn freshness_does_not_silently_authorize_unknown_effects() {
        let strict = requirement_footprint_for_plan_node(
            &exec("python", EnvironmentRefV2::LinkerIsolated),
            PlacementIntentV1::Strict,
        )
        .unwrap();
        assert!(strict.is_conservative_unknown());

        let autonomous = requirement_footprint_for_plan_node(
            &exec("python", EnvironmentRefV2::LinkerIsolated),
            PlacementIntentV1::AutonomousUnknownEffects,
        )
        .unwrap();
        assert!(autonomous.is_complete());
        assert!(autonomous
            .known_atoms()
            .contains(&RequirementAtomV1::Effect(
                EffectRequirementV1::AutonomousUnknownEffects
            )));
    }

    #[test]
    fn pure_inline_index_is_stateless_not_actor_affine() {
        let footprint = requirement_footprint_for_plan_node(
            &exec("html", EnvironmentRefV2::Persistent(9)),
            PlacementIntentV1::Strict,
        )
        .unwrap();
        assert!(footprint.is_complete());
        assert!(footprint
            .known_atoms()
            .contains(&RequirementAtomV1::Environment(
                EnvironmentRequirementV1::Stateless
            )));
    }

    #[test]
    fn persistent_opaque_effects_require_explicit_session_serialization() {
        let persistent = exec("python", EnvironmentRefV2::Persistent(7));
        let strict =
            requirement_footprint_for_plan_node(&persistent, PlacementIntentV1::Strict).unwrap();
        assert!(strict.is_conservative_unknown());
        let autonomous = requirement_footprint_for_plan_node(
            &persistent,
            PlacementIntentV1::AutonomousUnknownEffects,
        )
        .unwrap();
        assert!(
            autonomous.is_conservative_unknown(),
            "fresh/autonomous authority must not authorize a persistent session"
        );

        let session = requirement_footprint_for_plan_node(
            &persistent,
            PlacementIntentV1::SessionSerializedOpaqueEffects,
        )
        .unwrap();
        assert!(session.is_complete());
        assert!(session
            .known_atoms()
            .contains(&RequirementAtomV1::Capability(
                CapabilityAtomV1::new(
                    CapabilityKeyV1::new(
                        SESSION_SERIALIZED_OPAQUE_EFFECTS_NAMESPACE_V1,
                        SESSION_SERIALIZED_OPAQUE_EFFECTS_NAME_V1,
                    )
                    .unwrap(),
                    1,
                )
                .unwrap()
            )));
        assert!(session.known_atoms().iter().any(|atom| matches!(
            atom,
            RequirementAtomV1::Environment(EnvironmentRequirementV1::SameLogicalEnvironment { .. })
        )));
    }
}
