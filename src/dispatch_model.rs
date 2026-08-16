//! Pure dispatch classification shared by evidence analysis and execution.
//!
//! This module decides which adapter an OIR operation describes. It performs
//! no dispatch and owns no workers. Evidence binds the decision; the executor
//! later verifies and realizes that exact adapter.

use std::collections::HashSet;

use crate::effects::{EffectConfidence, EffectSummary, Fallibility, ResourceKey};
use crate::environment::EnvironmentRefV2;
use crate::ir::{
    ExecutionMode, ExecutionPlan, OIr, PlanEdgeKind, PlanNodeId, PlanNodeKind, PlanScheduleKind,
    SpliceRenderer,
};

/// Stable preparation adapter selected by evidence analysis. The runtime may
/// validate the bound adapter against admitted OIR, but may not choose a
/// different adapter as a second scheduling authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchAdapterV1 {
    CoordinatorV1,
    OScopeLoadV1,
    TrustedInlineRendererV1,
    AutonomousEphemeralShimV1,
}

impl DispatchAdapterV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::CoordinatorV1 => "coordinator/v1",
            Self::OScopeLoadV1 => "o-scope-load/v1",
            Self::TrustedInlineRendererV1 => "trusted-inline-renderer/v1",
            Self::AutonomousEphemeralShimV1 => "autonomous-ephemeral-shim/v1",
        }
    }

    pub const fn is_local_worker(self) -> bool {
        !matches!(self, Self::CoordinatorV1)
    }
}

/// A statically determined Send-only task classification for one plan node.
#[derive(Clone, Debug)]
pub enum TaskKind {
    Renderer {
        renderer: SpliceRenderer,
        canonical: String,
    },
    Load {
        name: String,
    },
    EphemeralShim {
        language: String,
        renderer: SpliceRenderer,
    },
}

impl TaskKind {
    pub(crate) const fn adapter(&self) -> DispatchAdapterV1 {
        match self {
            Self::Renderer { .. } => DispatchAdapterV1::TrustedInlineRendererV1,
            Self::Load { .. } => DispatchAdapterV1::OScopeLoadV1,
            Self::EphemeralShim { .. } => DispatchAdapterV1::AutonomousEphemeralShimV1,
        }
    }
}

/// Classify a plan node when a local Send-only task adapter is available.
pub(crate) fn classify(plan: &ExecutionPlan, oir: &OIr, id: PlanNodeId) -> Option<TaskKind> {
    match oir {
        OIr::Load(name) => Some(TaskKind::Load { name: name.clone() }),
        OIr::Exec { attr, backend, .. }
            if attr.is_none()
                && backend.pure
                && backend.execution == ExecutionMode::InlineValue
                && renderer_inputs_statically_preparable(oir) =>
        {
            match backend.canonical.as_str() {
                "html" | "markdown" | "text" | "latex" => Some(TaskKind::Renderer {
                    renderer: backend.renderer,
                    canonical: backend.canonical.clone(),
                }),
                _ => None,
            }
        }
        OIr::Exec { backend, .. } if autonomous_ephemeral_group(plan, id, oir).is_some() => {
            Some(TaskKind::EphemeralShim {
                language: backend.canonical.clone(),
                renderer: backend.renderer,
            })
        }
        _ => None,
    }
}

/// Confirm that admission's exact adapter still matches the admitted OIR.
pub(crate) fn adapter_matches(
    adapter: DispatchAdapterV1,
    plan: &ExecutionPlan,
    id: PlanNodeId,
    oir: &OIr,
) -> bool {
    match adapter {
        DispatchAdapterV1::OScopeLoadV1 => matches!(oir, OIr::Load(_)),
        DispatchAdapterV1::TrustedInlineRendererV1 => renderer_inputs_statically_preparable(oir),
        DispatchAdapterV1::AutonomousEphemeralShimV1 => {
            autonomous_ephemeral_group(plan, id, oir).is_some()
        }
        DispatchAdapterV1::CoordinatorV1 => false,
    }
}

/// Return the explicit coordination group authorizing non-strict execution of
/// one bare hosted shim.
///
/// This is a policy classification, not an effect-independence proof: the shim
/// remains effect-unknown, and only the nearest explicit `autonomous(...)`
/// schedule can opt it into unordered semantics.
pub(crate) fn autonomous_ephemeral_group(
    plan: &ExecutionPlan,
    node: PlanNodeId,
    oir: &OIr,
) -> Option<PlanNodeId> {
    let OIr::Exec {
        env_id,
        attr,
        backend,
        body,
        ..
    } = oir
    else {
        return None;
    };
    if !EnvironmentRefV2::from_encoded(*env_id).is_fresh()
        || attr.is_some()
        || backend.execution != ExecutionMode::Shim
        || !body
            .iter()
            .all(|child| matches!(child, OIr::Text(_) | OIr::Store { .. }))
    {
        return None;
    }

    let group = plan.edges.iter().find_map(|edge| {
        (edge.kind == PlanEdgeKind::Structural
            && edge.from == node
            && matches!(plan.nodes[edge.to.0].kind, PlanNodeKind::Group { .. }))
        .then_some(edge.to)
    })?;

    nearest_policy_schedule_is_autonomous(plan, group).then_some(group)
}

fn nearest_policy_schedule_is_autonomous(plan: &ExecutionPlan, node: PlanNodeId) -> bool {
    let mut current = node;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current) {
            return false;
        }
        let parents = plan
            .edges
            .iter()
            .filter_map(|edge| {
                (edge.kind == PlanEdgeKind::Structural && edge.from == current).then_some(edge.to)
            })
            .collect::<Vec<_>>();
        let [parent] = parents.as_slice() else {
            return false;
        };
        match plan.nodes[parent.0].kind {
            PlanNodeKind::Schedule {
                kind: PlanScheduleKind::Autonomous,
                ..
            } => return true,
            PlanNodeKind::Schedule {
                kind: PlanScheduleKind::Lazy,
                ..
            } => return false,
            _ => current = *parent,
        }
    }
}

/// Admission may claim a local-worker renderer only when preparation is
/// entirely source-proven and cannot force a lazy evaluator request.
pub(crate) fn renderer_inputs_statically_preparable(oir: &OIr) -> bool {
    let OIr::Exec {
        attr,
        backend,
        body,
        ..
    } = oir
    else {
        return false;
    };
    attr.is_none()
        && backend.pure
        && backend.execution == ExecutionMode::InlineValue
        && matches!(
            backend.canonical.as_str(),
            "html" | "markdown" | "text" | "latex"
        )
        && body.iter().all(|child| match child {
            OIr::Text(_) | OIr::Store { .. } => true,
            OIr::Exec { .. } => renderer_inputs_statically_preparable(child),
            OIr::Load(_) | OIr::Invoke { .. } => false,
        })
}

/// Hard effect/failure predicate shared by evidence and runtime verification.
pub(crate) fn effect_contract_worker_safe(summary: &EffectSummary, oir: &OIr) -> bool {
    match oir {
        OIr::Exec {
            env_id, backend, ..
        } if EnvironmentRefV2::from_encoded(*env_id).is_fresh()
            && backend.execution == ExecutionMode::Shim =>
        {
            summary.unknown
                && summary.fallibility == Fallibility::MayFail
                && summary.actor_state.is_none()
        }
        OIr::Load(_) => {
            summary.confidence == EffectConfidence::Verified
                && summary.deterministic
                && summary.fallibility == Fallibility::MayFail
                && !summary.unknown
                && summary.actor_state.is_none()
                && summary.writes.is_empty()
                && summary
                    .reads
                    .iter()
                    .all(|resource| matches!(resource, ResourceKey::ScopeBinding(_)))
                && !summary.network
                && !summary.spawn
                && !summary.clock
        }
        _ => summary.is_verified_pure_infallible(),
    }
}
