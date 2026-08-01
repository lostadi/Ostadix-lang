//! Stable planner projection separating governed state from ambient host state.
//!
//! This report is descriptive. It never treats serialized capability metadata
//! as authority, never promotes an unresolved native value to portable data,
//! and never rewrites the logical HGraph. A later placement layer can bind the
//! same logical plan to a different [`WorldIdentity`] without changing it.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use thiserror::Error;

use crate::effects::{ActorResourceId, ResourceKey};
use crate::eval::BlockOptions;
use crate::hgraph::HGraph;
use crate::ir::{ExecutionMode, ExecutionPlan, PlanEdgeKind, PlanNodeId, PlanNodeKind};
use crate::value::{BackendAuthority, OValue, RehydratePolicy};

use super::identity::{WorldId, WorldIdentity, WorldIdentityError};

/// One ordinary OValue dependency in the logical execution plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OValueFlowGrounding {
    pub from: PlanNodeId,
    pub to: PlanNodeId,
    pub relation: OValueFlowRelation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OValueFlowRelation {
    Structural,
    Data,
}

impl OValueFlowRelation {
    fn name(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::Data => "data",
        }
    }
}

/// Authority required or preferred by one backend operation.
///
/// `requested_rights` describes runtime policy input, not authority already
/// granted. Current hosted evaluation may fall back from a named bearer to the
/// evaluator's ambient default backend authority; the report exposes that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityGrounding {
    pub plan_node: PlanNodeId,
    pub backend: String,
    pub preferred_binding: Option<String>,
    pub requested_rights: BTreeSet<BackendAuthority>,
    pub ambient_fallback: bool,
}

/// A concrete native value observed in a materialized graph node.
///
/// Existing `ONative` values do not yet carry World/node/domain/process
/// generation provenance, so `origin_generation_grounded` remains false even
/// when their declared rehydration policy is more specific.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleGrounding {
    pub graph_node: crate::hgraph::NodeId,
    pub language: String,
    pub rehydrate: RehydratePolicy,
    pub origin_generation_grounded: bool,
}

/// Resource and affinity projection for one logical operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationGrounding {
    pub plan_node: PlanNodeId,
    pub governed_reads: BTreeSet<ResourceKey>,
    pub governed_writes: BTreeSet<ResourceKey>,
    pub ambient_reads: BTreeSet<ResourceKey>,
    pub ambient_writes: BTreeSet<ResourceKey>,
    pub actor_affinity: Option<ActorResourceId>,
}

impl OperationGrounding {
    pub fn has_residual_host_world(&self) -> bool {
        self.ambient_reads.contains(&ResourceKey::HostWorld)
            || self.ambient_writes.contains(&ResourceKey::HostWorld)
    }
}

/// A deterministic, non-authorizing view of logical flow and physical
/// grounding information currently known to the planner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroundingReport {
    bound_world: Option<WorldIdentity>,
    pub ovalue_flows: Vec<OValueFlowGrounding>,
    pub capabilities: Vec<CapabilityGrounding>,
    pub capsules: Vec<CapsuleGrounding>,
    pub operations: Vec<OperationGrounding>,
}

#[derive(Debug, Error)]
pub enum GroundingError {
    #[error("the plan is not bound to a World epoch")]
    UnboundWorld,
    #[error("invalid or inconsistent grounding plan/HGraph: {0}")]
    InvalidExecutionGraph(String),
    #[error("plan node P{plan_node} has no semantic effect summary")]
    MissingEffectSummary { plan_node: usize },
    #[error("invalid block attributes at plan node P{plan_node}: {reason}")]
    InvalidBlockAttributes { plan_node: usize, reason: String },
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
}

impl GroundingReport {
    pub fn analyze(
        plan: &ExecutionPlan,
        graph: &HGraph,
        bound_world: Option<WorldIdentity>,
    ) -> Result<Self, GroundingError> {
        graph
            .validate_execution_plan(plan)
            .map_err(GroundingError::InvalidExecutionGraph)?;

        let mut ovalue_flows = plan
            .edges
            .iter()
            .filter_map(|edge| {
                let relation = match edge.kind {
                    PlanEdgeKind::Structural => OValueFlowRelation::Structural,
                    PlanEdgeKind::Data => OValueFlowRelation::Data,
                    PlanEdgeKind::Sequence => return None,
                };
                Some(OValueFlowGrounding {
                    from: edge.from,
                    to: edge.to,
                    relation,
                })
            })
            .collect::<Vec<_>>();
        ovalue_flows.sort_by_key(|flow| (flow.from.0, flow.to.0, flow.relation.name()));

        let mut capabilities = plan
            .nodes
            .iter()
            .map(|node| capability_grounding(node.id, &node.kind))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        capabilities.sort_by_key(|capability| capability.plan_node.0);

        let mut capsules = graph
            .node_ids()
            .into_iter()
            .filter_map(|id| {
                let node = graph.node(id)?;
                let OValue::Native { v } = node.value.as_ref()? else {
                    return None;
                };
                Some(CapsuleGrounding {
                    graph_node: id,
                    language: v.lang.clone(),
                    rehydrate: v.rehydrate,
                    origin_generation_grounded: false,
                })
            })
            .collect::<Vec<_>>();
        capsules.sort_by_key(|capsule| capsule.graph_node.0);

        let mut operations = Vec::new();
        for node in &plan.nodes {
            if graph.op_for(node.id).is_none() {
                continue;
            }
            let summary =
                graph
                    .effect_summary(node.id)
                    .ok_or(GroundingError::MissingEffectSummary {
                        plan_node: node.id.0,
                    })?;
            operations.push(OperationGrounding {
                plan_node: node.id,
                governed_reads: summary
                    .reads
                    .iter()
                    .filter(|resource| resource.is_governed_resource())
                    .cloned()
                    .collect(),
                governed_writes: summary
                    .writes
                    .iter()
                    .filter(|resource| resource.is_governed_resource())
                    .cloned()
                    .collect(),
                ambient_reads: summary
                    .reads
                    .iter()
                    .filter(|resource| resource.is_host_resource())
                    .cloned()
                    .collect(),
                ambient_writes: summary
                    .writes
                    .iter()
                    .filter(|resource| resource.is_host_resource())
                    .cloned()
                    .collect(),
                actor_affinity: summary.actor_state.clone(),
            });
        }
        operations.sort_by_key(|operation| operation.plan_node.0);

        Ok(Self {
            bound_world,
            ovalue_flows,
            capabilities,
            capsules,
            operations,
        })
    }

    pub fn bound_world(&self) -> Option<&WorldIdentity> {
        self.bound_world.as_ref()
    }

    /// Reject a report made for an old World epoch before placement or
    /// execution can consume it.
    pub fn require_current_world(&self, current: &WorldIdentity) -> Result<(), GroundingError> {
        let reference = self
            .bound_world
            .as_ref()
            .ok_or(GroundingError::UnboundWorld)?;
        current.require_current(reference)?;
        for resource in self.operations.iter().flat_map(|operation| {
            operation
                .governed_reads
                .iter()
                .chain(&operation.governed_writes)
        }) {
            require_governed_resource_world(current, resource)?;
        }
        Ok(())
    }

    pub fn to_text(&self) -> String {
        let mut output = String::from("; GroundingReport oworld.grounding/v1\n");
        match &self.bound_world {
            Some(world) => writeln!(output, "world {world}").unwrap(),
            None => output.push_str("world none\n"),
        }

        if self.ovalue_flows.is_empty() {
            output.push_str("ovalue-flow none\n");
        } else {
            for flow in &self.ovalue_flows {
                writeln!(
                    output,
                    "ovalue-flow P{} -> P{} {}",
                    flow.from.0,
                    flow.to.0,
                    flow.relation.name()
                )
                .unwrap();
            }
        }

        if self.capabilities.is_empty() {
            output.push_str("capability-flow none\n");
        } else {
            for capability in &self.capabilities {
                let rights = join_authorities(&capability.requested_rights);
                let binding = capability.preferred_binding.as_deref().unwrap_or("none");
                let resolution = match (
                    capability.preferred_binding.is_some(),
                    capability.ambient_fallback,
                ) {
                    (true, true) => "preferred-bearer-then-ambient",
                    (false, true) => "ambient-default",
                    (true, false) => "explicit-bearer",
                    (false, false) => "none",
                };
                writeln!(
                    output,
                    "capability-flow P{} backend={} preferred={} requested-rights=[{}] resolution={}",
                    capability.plan_node.0,
                    capability.backend,
                    binding,
                    rights,
                    resolution
                )
                .unwrap();
            }
        }

        if self.capsules.is_empty() {
            output.push_str(
                "capsule-affinity none (runtime outputs unresolved; unproved portability is nonportable)\n",
            );
        } else {
            for capsule in &self.capsules {
                writeln!(
                    output,
                    "capsule-affinity N{} lang={} rehydrate={} origin-generation={}",
                    capsule.graph_node.0,
                    capsule.language,
                    rehydrate_name(capsule.rehydrate),
                    if capsule.origin_generation_grounded {
                        "grounded"
                    } else {
                        "unresolved"
                    }
                )
                .unwrap();
            }
        }

        let mut emitted_governed = false;
        for operation in &self.operations {
            if operation.governed_reads.is_empty() && operation.governed_writes.is_empty() {
                continue;
            }
            emitted_governed = true;
            writeln!(
                output,
                "governed-effects P{} reads=[{}] writes=[{}]",
                operation.plan_node.0,
                join_resources(&operation.governed_reads),
                join_resources(&operation.governed_writes)
            )
            .unwrap();
        }
        if !emitted_governed {
            output.push_str("governed-effects none\n");
        }

        let mut emitted_actor = false;
        for operation in &self.operations {
            if let Some(actor) = &operation.actor_affinity {
                emitted_actor = true;
                writeln!(
                    output,
                    "actor-affinity P{} actor:{}",
                    operation.plan_node.0, actor
                )
                .unwrap();
            }
        }
        if !emitted_actor {
            output.push_str("actor-affinity none\n");
        }

        let mut emitted_ambient = false;
        for operation in &self.operations {
            if operation.ambient_reads.is_empty() && operation.ambient_writes.is_empty() {
                continue;
            }
            emitted_ambient = true;
            writeln!(
                output,
                "ambient-effects P{} reads=[{}] writes=[{}] hostworld={}",
                operation.plan_node.0,
                join_resources(&operation.ambient_reads),
                join_resources(&operation.ambient_writes),
                if operation.has_residual_host_world() {
                    "residual"
                } else {
                    "no"
                }
            )
            .unwrap();
        }
        if !emitted_ambient {
            output.push_str("ambient-effects none\n");
        }

        output.push_str(
            "authority-note serialized capability metadata is descriptive; granted rights remain private to the live broker\n",
        );
        output
    }
}

fn capability_grounding(
    plan_node: PlanNodeId,
    kind: &PlanNodeKind,
) -> Result<Option<CapabilityGrounding>, GroundingError> {
    let PlanNodeKind::Exec {
        lang,
        attr,
        backend,
        ..
    } = kind
    else {
        return Ok(None);
    };

    // Use the evaluator's parser so inspection rejects the same malformed,
    // duplicate, or contradictory block attributes as live execution.
    let options = BlockOptions::parse(attr.as_deref(), lang).map_err(|error| {
        GroundingError::InvalidBlockAttributes {
            plan_node: plan_node.0,
            reason: error.to_string(),
        }
    })?;

    let mut requested_rights = BTreeSet::new();
    if backend.execution == ExecutionMode::Shim {
        requested_rights.extend(BackendAuthority::ALL);
    }
    requested_rights.extend(backend.required_authorities.iter().copied());
    requested_rights.extend(options.permissions().iter().copied());

    // resolve_backend_authority returns before consulting either a preferred
    // binding or ambient authority when no permission is requested.
    if requested_rights.is_empty() {
        return Ok(None);
    }

    Ok(Some(CapabilityGrounding {
        plan_node,
        backend: backend.canonical.clone(),
        preferred_binding: options.capability_binding().map(str::to_owned),
        requested_rights,
        // Current hosted evaluation deliberately falls back to the default
        // backend authority when a preferred bearer is absent or insufficient.
        ambient_fallback: true,
    }))
}

fn join_authorities(authorities: &BTreeSet<BackendAuthority>) -> String {
    authorities
        .iter()
        .map(|authority| authority.name())
        .collect::<Vec<_>>()
        .join(",")
}

fn join_resources(resources: &BTreeSet<ResourceKey>) -> String {
    resources
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn rehydrate_name(policy: RehydratePolicy) -> &'static str {
    match policy {
        RehydratePolicy::Portable => "portable-declared",
        RehydratePolicy::SameBackend => "same-backend",
        RehydratePolicy::SameProcess => "same-process",
        RehydratePolicy::Never => "never",
    }
}

fn require_governed_resource_world(
    current: &WorldIdentity,
    resource: &ResourceKey,
) -> Result<(), WorldIdentityError> {
    match resource {
        ResourceKey::WorldState(world) => current.require_current(world),
        ResourceKey::ArtifactState(artifact) => current.require_current(artifact.world()),
        ResourceKey::NodeState(node) => require_same_world(current, node.world()),
        ResourceKey::DomainState(domain) => require_same_world(current, domain.node().world()),
        ResourceKey::GovernedResource(resource) => {
            require_same_world(current, resource.owner().world())
        }
        ResourceKey::TaskState(task) => require_same_world(current, task.world()),
        _ => Ok(()),
    }
}

fn require_same_world(
    current: &WorldIdentity,
    reference: &WorldId,
) -> Result<(), WorldIdentityError> {
    if current.world() == reference {
        Ok(())
    } else {
        Err(WorldIdentityError::IdentityMismatch {
            kind: "world",
            expected: current.world().to_string(),
            got: reference.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{
        ArtifactId, ArtifactPublicationIdentity, AttemptGeneration, DomainGeneration, DomainId,
        DomainIdentity, NodeGeneration, NodeId, NodeIdentity, ResourceId, ResourceIdentity,
        ResourceOwner, TaskAttemptIdentity, TaskId, WorldEpoch,
    };

    fn world(name: &str, epoch: u64) -> WorldIdentity {
        WorldIdentity::new(WorldId::new(name).unwrap(), WorldEpoch::new(epoch).unwrap())
    }

    #[test]
    fn world_validation_fences_snapshots_without_over_fencing_owned_identities() {
        let current = world("desk", 9);
        let node = NodeIdentity::new(
            WorldId::new("desk").unwrap(),
            NodeId::new("node-a").unwrap(),
            NodeGeneration::new(2).unwrap(),
        );
        let domain = DomainIdentity::new(
            node.clone(),
            DomainId::new("provider").unwrap(),
            DomainGeneration::new(3).unwrap(),
        );
        let resource = ResourceIdentity::new(
            ResourceOwner::Domain {
                domain: domain.clone(),
            },
            ResourceId::new("cpu/slot-0").unwrap(),
        );
        let task = TaskAttemptIdentity::new(
            WorldId::new("desk").unwrap(),
            TaskId::new("build").unwrap(),
            AttemptGeneration::new(4).unwrap(),
        );

        for owned in [
            ResourceKey::NodeState(node),
            ResourceKey::DomainState(domain),
            ResourceKey::GovernedResource(resource),
            ResourceKey::TaskState(task),
        ] {
            require_governed_resource_world(&current, &owned).unwrap();
        }

        let stale_world = world("desk", 8);
        assert!(matches!(
            require_governed_resource_world(
                &current,
                &ResourceKey::WorldState(stale_world.clone())
            ),
            Err(WorldIdentityError::StaleGeneration {
                kind: "world epoch",
                expected: 9,
                got: 8
            })
        ));
        let stale_publication = ArtifactPublicationIdentity::new(
            stale_world,
            ArtifactId::from_sha256("a".repeat(64)).unwrap(),
        );
        assert!(matches!(
            require_governed_resource_world(
                &current,
                &ResourceKey::ArtifactState(stale_publication)
            ),
            Err(WorldIdentityError::StaleGeneration {
                kind: "world epoch",
                expected: 9,
                got: 8
            })
        ));

        let other_node = NodeIdentity::new(
            WorldId::new("other").unwrap(),
            NodeId::new("node-a").unwrap(),
            NodeGeneration::new(2).unwrap(),
        );
        assert!(matches!(
            require_governed_resource_world(&current, &ResourceKey::NodeState(other_node)),
            Err(WorldIdentityError::IdentityMismatch { kind: "world", .. })
        ));
    }
}
