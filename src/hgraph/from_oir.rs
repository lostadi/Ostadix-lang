use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::{
    effects::{effect_summary_for_plan_node, EffectSummary, ResourceKey},
    ir::{
        BackendInterface, ExecutionMode, ExecutionPlan, OIr, OIrProgram, PlanEdgeKind, PlanNodeId,
        PlanNodeKind, PlanScheduleKind,
    },
    value::{GroupMode, OValue},
};

use super::{
    graph::{HEdge, HGraph, HNode, NodeId, Port, PortRole},
    kinds::{DomainFlags, ExecutableOp, OpKind, RepFlags},
};

pub fn build_program(program: &OIrProgram) -> HGraph {
    let plan = program.plan();
    build_program_with_plan(program, &plan)
        .expect("freshly-built OIR execution plan should project to HGraph")
}

pub fn build_program_with_plan(
    program: &OIrProgram,
    plan: &ExecutionPlan,
) -> Result<HGraph, String> {
    plan.validate(program.nodes.len())?;

    // A caller may supply alternate dependency edges for analysis/testing, but
    // node identity and root identity must still describe this exact OIR tree.
    // Otherwise the coordinator would schedule one semantic operation while
    // executing a different instruction from `ir_map`.
    let canonical_plan = program.plan();
    if plan.nodes != canonical_plan.nodes {
        return Err("execution plan node semantics do not match the OIR program".to_string());
    }
    if plan.roots != canonical_plan.roots {
        return Err("execution plan roots do not match the OIR program".to_string());
    }

    let oir_nodes = program.flatten_for_plan();
    if oir_nodes.len() != plan.nodes.len() {
        return Err(format!(
            "OIR flatten produced {} nodes for execution plan with {} nodes",
            oir_nodes.len(),
            plan.nodes.len()
        ));
    }

    let mut graph = HGraph::default();
    graph.set_source_plan(plan.clone());
    let mut node_map: HashMap<PlanNodeId, NodeId> = HashMap::new();

    for plan_node in &plan.nodes {
        let oir = oir_nodes[plan_node.id.0];
        let graph_node = graph.add_node(hnode_for_oir(oir));
        if let Some(node) = graph.node_mut(graph_node) {
            node.plan_node = Some(plan_node.id);
        }
        graph.record_ir(graph_node, oir);
        node_map.insert(plan_node.id, graph_node);
    }

    for root in &plan.roots {
        let node = node_map[root];
        graph.push_root(node);
    }

    // Constraint/type edges — kept structurally identical to the historical
    // projection so the type/fidelity solver and the DOT exporter operate over
    // the same `OpKind` relations they always did.
    for edge in &plan.edges {
        graph.add_edge(HEdge::constraint(
            match edge.kind {
                PlanEdgeKind::Structural => OpKind::StructuralBarrier,
                PlanEdgeKind::Sequence => OpKind::Sequence,
                PlanEdgeKind::Data
                    if matches!(plan.nodes[edge.to.0].kind, PlanNodeKind::Load { .. }) =>
                {
                    OpKind::DataFlow
                }
                PlanEdgeKind::Data => OpKind::Sequence,
            },
            vec![
                Port {
                    node: node_map[&edge.from],
                    role: PortRole::Input,
                },
                Port {
                    node: node_map[&edge.to],
                    role: PortRole::Output,
                },
            ],
        ));
    }

    add_plan_semantics(&mut graph, plan, &node_map);
    add_execute_edges(&mut graph, plan, &node_map)?;
    graph.validate_execution_graph()?;
    Ok(graph)
}

/// Lower every non-literal plan operation to an executable hyperedge consuming
/// ordinary values, prior resource versions, and preserved completion tokens,
/// then producing one ordinary value plus completion and successor states.
/// Literal `Text` nodes are materialized values with no Execute edge.
fn add_execute_edges(
    graph: &mut HGraph,
    plan: &ExecutionPlan,
    node_map: &HashMap<PlanNodeId, NodeId>,
) -> Result<(), String> {
    let mut summaries: HashMap<PlanNodeId, EffectSummary> = HashMap::new();
    for plan_node in &plan.nodes {
        if executable_op(&plan_node.kind).is_none() {
            continue;
        }
        let summary = effect_summary_for_plan_node(plan_node.id, &plan_node.kind)?;
        graph.set_effect_summary(plan_node.id, summary.clone());
        summaries.insert(plan_node.id, summary);
        graph.add_completion_node(plan_node.id)?;
    }

    // Plan ids are allocated preorder (parents before nested children), so
    // resource versions must advance in dependency/topological order. Using
    // source ids here would create A -> C state edges against C -> A structural
    // edges for a nested effect and make the executable graph cyclic.
    let mut state = StateLowering::default();
    for (ordinal, id) in plan.topological_order()?.into_iter().enumerate() {
        let plan_node = &plan.nodes[id.0];
        let Some(op) = executable_op(&plan_node.kind) else {
            continue;
        };
        let summary = summaries
            .get(&id)
            .ok_or_else(|| format!("missing effect summary for plan node {}", id.0))?;
        let value_output = node_map[&id];
        let completion = graph
            .completion_node(id)
            .ok_or_else(|| format!("missing completion node for plan node {}", id.0))?;
        let mut inputs = operation_value_inputs(plan, node_map, id);
        let mut outputs = vec![value_output, completion];

        let preserved_sequences = executable_sequence_predecessors(plan, id)
            .into_iter()
            .filter(|predecessor| !sequence_can_relax(plan, *predecessor, id, &summaries))
            .collect::<Vec<_>>();
        for predecessor in &preserved_sequences {
            let predecessor_completion = graph.completion_node(*predecessor).ok_or_else(|| {
                format!(
                    "sequence predecessor {} has no completion node",
                    predecessor.0
                )
            })?;
            inputs.push(predecessor_completion);
        }

        add_resource_state_transitions(
            graph,
            &mut state,
            summary,
            id,
            completion,
            &mut inputs,
            &mut outputs,
        );

        deduplicate_nodes(&mut inputs);
        deduplicate_nodes(&mut outputs);
        // The stable ordinal is the reference executor's topological
        // execution rank, not preorder PlanNodeId. Nested parents are allocated
        // before their children but execute after them; failure selection must
        // follow the latter.
        graph.add_exec_edge(id, op, inputs, outputs, value_output, ordinal as u64)?;
        for predecessor in preserved_sequences {
            let completion = graph
                .completion_node(predecessor)
                .expect("preserved predecessor completion was checked above");
            graph.record_sequence_dependency(predecessor, id, completion)?;
        }
    }
    Ok(())
}

fn add_resource_state_transitions(
    graph: &mut HGraph,
    state: &mut StateLowering,
    summary: &EffectSummary,
    producer: PlanNodeId,
    completion: NodeId,
    inputs: &mut Vec<NodeId>,
    outputs: &mut Vec<NodeId>,
) {
    let (reads, writes) = summary.scheduling_accesses();
    let resources = reads.union(&writes).cloned().collect::<BTreeSet<_>>();
    for resource in resources {
        if writes.contains(&resource) {
            let (prior, open_reads, successor) = state.write(graph, resource, producer);
            inputs.push(prior);
            inputs.extend(open_reads);
            outputs.push(successor);
        } else {
            inputs.push(state.read(graph, resource, completion));
        }
    }
}

#[derive(Clone, Copy)]
struct ResourceHead {
    node: NodeId,
    version: u64,
}

struct ResourceFrontier {
    last_write: ResourceHead,
    open_reads: BTreeSet<NodeId>,
}

#[derive(Default)]
struct StateLowering {
    frontiers: BTreeMap<ResourceKey, ResourceFrontier>,
}

impl StateLowering {
    fn frontier<'a>(
        &'a mut self,
        graph: &mut HGraph,
        resource: &ResourceKey,
    ) -> &'a mut ResourceFrontier {
        self.frontiers.entry(resource.clone()).or_insert_with(|| {
            let node = graph.add_node(HNode::resource_state(resource.clone(), 0));
            ResourceFrontier {
                last_write: ResourceHead { node, version: 0 },
                open_reads: BTreeSet::new(),
            }
        })
    }

    fn read(&mut self, graph: &mut HGraph, resource: ResourceKey, completion: NodeId) -> NodeId {
        let frontier = self.frontier(graph, &resource);
        frontier.open_reads.insert(completion);
        frontier.last_write.node
    }

    fn write(
        &mut self,
        graph: &mut HGraph,
        resource: ResourceKey,
        producer: PlanNodeId,
    ) -> (NodeId, Vec<NodeId>, NodeId) {
        let frontier = self.frontier(graph, &resource);
        let prior = frontier.last_write.node;
        let open_reads = std::mem::take(&mut frontier.open_reads)
            .into_iter()
            .collect::<Vec<_>>();
        let next_version = frontier.last_write.version + 1;
        let successor = graph.add_node(HNode::resource_state(resource, next_version));
        if let Some(node) = graph.node_mut(successor) {
            node.plan_node = Some(producer);
        }
        frontier.last_write = ResourceHead {
            node: successor,
            version: next_version,
        };
        (prior, open_reads, successor)
    }
}

/// Ordinary value inputs an operation consumes: structural children followed
/// by additional lexical/data predecessors. State and completion inputs are
/// appended separately by `add_execute_edges`.
fn operation_value_inputs(
    plan: &ExecutionPlan,
    node_map: &HashMap<PlanNodeId, NodeId>,
    parent: PlanNodeId,
) -> Vec<NodeId> {
    let mut seen = HashSet::new();
    let mut ordered: Vec<(usize, NodeId)> = Vec::new();

    for edge in &plan.edges {
        let is_input =
            matches!(edge.kind, PlanEdgeKind::Structural | PlanEdgeKind::Data) && edge.to == parent;
        if is_input && seen.insert(edge.from) {
            ordered.push((edge.from.0, node_map[&edge.from]));
        }
    }

    ordered.sort_by_key(|(source, _)| *source);
    ordered.into_iter().map(|(_, node)| node).collect()
}

/// Follow every incoming source-sequence path backward across materialized
/// literal nodes until its nearest executable predecessor. This preserves
/// `A -> whitespace -> B` as `Completion(A) -> B`, and it deliberately handles
/// validated custom plans with more than one incoming sequence edge.
pub(super) fn executable_sequence_predecessors(
    plan: &ExecutionPlan,
    target: PlanNodeId,
) -> BTreeSet<PlanNodeId> {
    let mut pending = plan
        .edges
        .iter()
        .filter_map(|edge| {
            (edge.kind == PlanEdgeKind::Sequence && edge.to == target).then_some(edge.from)
        })
        .collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut executable = BTreeSet::new();

    while let Some(source) = pending.pop() {
        if !visited.insert(source) {
            continue;
        }
        if executable_op(&plan.nodes[source.0].kind).is_some() {
            executable.insert(source);
            continue;
        }
        pending.extend(plan.edges.iter().filter_map(|edge| {
            (edge.kind == PlanEdgeKind::Sequence && edge.to == source).then_some(edge.from)
        }));
    }
    executable
}

pub(super) fn sequence_can_relax(
    plan: &ExecutionPlan,
    predecessor: PlanNodeId,
    successor: PlanNodeId,
    summaries: &HashMap<PlanNodeId, EffectSummary>,
) -> bool {
    if direct_members_of_concurrent_group(plan, predecessor, successor) {
        return true;
    }
    let Some(left) = summaries.get(&predecessor) else {
        return false;
    };
    let Some(right) = summaries.get(&successor) else {
        return false;
    };
    if inside_left_to_right_region(plan, predecessor)
        || inside_left_to_right_region(plan, successor)
    {
        return false;
    }
    // O-level loads are compiler-verified, read-only, and leave no external
    // effect when they fail. Their outcomes can therefore be selected in
    // stable ordinal order by a future concurrent dispatcher without changing
    // strict fail-stop effects. Keep this narrow: hosted/user-declared reads do
    // not receive the same authority.
    if matches!(plan.nodes[predecessor.0].kind, PlanNodeKind::Load { .. })
        && matches!(plan.nodes[successor.0].kind, PlanNodeKind::Load { .. })
        && verified_read_only(left)
        && verified_read_only(right)
    {
        return true;
    }
    left.is_verified_pure_infallible()
        && right.is_verified_pure_infallible()
        && verified_reorderable_inline(plan, predecessor, summaries, &mut HashSet::new())
        && verified_reorderable_inline(plan, successor, summaries, &mut HashSet::new())
}

fn verified_read_only(summary: &EffectSummary) -> bool {
    summary.confidence == crate::effects::EffectConfidence::Verified
        && !summary.unknown
        && summary.actor_state.is_none()
        && summary.writes.is_empty()
        && !summary.network
        && !summary.spawn
        && !summary.clock
}

/// Structural `O` regions promise left-to-right child evaluation. Even a pair
/// of otherwise reorderable inline renders keeps its completion dependency
/// there. Explicit concurrent groups are handled before this check and retain
/// their own topology.
fn inside_left_to_right_region(plan: &ExecutionPlan, node: PlanNodeId) -> bool {
    plan.edges
        .iter()
        .filter(|edge| edge.kind == PlanEdgeKind::Structural && edge.from == node)
        .map(|edge| &plan.nodes[edge.to.0].kind)
        .any(|kind| {
            matches!(
                kind,
                PlanNodeKind::Exec { backend, .. }
                    if backend.execution == ExecutionMode::InlineAst
                        && backend.canonical == "O"
            )
        })
}

fn direct_members_of_concurrent_group(
    plan: &ExecutionPlan,
    left: PlanNodeId,
    right: PlanNodeId,
) -> bool {
    plan.edges
        .iter()
        .filter(|edge| edge.kind == PlanEdgeKind::Structural && edge.from == left)
        .map(|edge| edge.to)
        .any(|parent| {
            matches!(plan.nodes[parent.0].kind, PlanNodeKind::Group { .. })
                && plan.edges.iter().any(|edge| {
                    edge.kind == PlanEdgeKind::Structural && edge.from == right && edge.to == parent
                })
        })
}

fn verified_reorderable_inline(
    plan: &ExecutionPlan,
    node: PlanNodeId,
    summaries: &HashMap<PlanNodeId, EffectSummary>,
    visited: &mut HashSet<PlanNodeId>,
) -> bool {
    if !visited.insert(node) {
        return false;
    }
    let is_trusted_renderer = matches!(
        &plan.nodes[node.0].kind,
        PlanNodeKind::Exec { backend, .. }
            if backend.pure
                && backend.execution == ExecutionMode::InlineValue
                && matches!(backend.canonical.as_str(), "html" | "markdown" | "text" | "latex")
    );
    if !is_trusted_renderer {
        return false;
    }

    // A pure outer renderer can still contain a load, request, schedule, or
    // hosted evaluation that fails or mutates state while its body is forced.
    // Relaxation is therefore justified only when the complete structural
    // subtree consists of literals and recursively verified renderers.
    plan.edges
        .iter()
        .filter_map(|edge| {
            (edge.kind == PlanEdgeKind::Structural && edge.to == node).then_some(edge.from)
        })
        .all(|child| match &plan.nodes[child.0].kind {
            PlanNodeKind::Text => true,
            PlanNodeKind::Exec { .. } => {
                summaries
                    .get(&child)
                    .is_some_and(EffectSummary::is_verified_pure_infallible)
                    && verified_reorderable_inline(plan, child, summaries, visited)
            }
            _ => false,
        })
}

fn deduplicate_nodes(nodes: &mut Vec<NodeId>) {
    let mut seen = HashSet::new();
    nodes.retain(|node| seen.insert(*node));
}

pub(super) fn executable_op(kind: &PlanNodeKind) -> Option<ExecutableOp> {
    match kind {
        PlanNodeKind::Text => None,
        PlanNodeKind::Load { .. } => Some(ExecutableOp::LoadBinding),
        PlanNodeKind::Store { .. } => Some(ExecutableOp::Store),
        PlanNodeKind::Call { fn_name, mode, .. } => Some(ExecutableOp::Invoke {
            fn_name: fn_name.clone(),
            mode: *mode,
        }),
        PlanNodeKind::Request { kind, .. } => Some(ExecutableOp::Request {
            kind: kind.label().to_string(),
        }),
        PlanNodeKind::Group { mode, .. } => Some(ExecutableOp::Group { mode: *mode }),
        PlanNodeKind::Schedule { kind, .. } => match kind {
            PlanScheduleKind::Force => Some(ExecutableOp::ForceRequest {
                kind: "force".to_string(),
            }),
            other => Some(ExecutableOp::Schedule {
                kind: other.label().to_string(),
            }),
        },
        PlanNodeKind::Exec {
            env_id, backend, ..
        } => match backend.execution {
            ExecutionMode::InlineAst | ExecutionMode::InlineValue => {
                Some(ExecutableOp::InlineBackend {
                    lang: backend.canonical.clone(),
                })
            }
            ExecutionMode::Shim => Some(ExecutableOp::EvalBackend {
                lang: backend.canonical.clone(),
                env: *env_id,
            }),
        },
    }
}

fn hnode_for_oir(node: &OIr) -> HNode {
    match node {
        OIr::Text(text) => {
            let mut node = HNode::with_value(OValue::str_(text.clone()));
            if !text.is_empty() {
                node.domain = DomainFlags::STRING;
                node.rep = RepFlags::STR;
            }
            node
        }
        _ => HNode::fresh(),
    }
}

fn add_plan_semantics(
    graph: &mut HGraph,
    plan: &ExecutionPlan,
    node_map: &HashMap<PlanNodeId, NodeId>,
) {
    for plan_node in &plan.nodes {
        let output = node_map[&plan_node.id];
        let inputs = structural_inputs(plan, node_map, plan_node.id);

        match &plan_node.kind {
            PlanNodeKind::Exec { lang, backend, .. } => {
                if let Some((dom, rep)) = backend_output_constraints(backend) {
                    graph.add_edge(HEdge::constraint(
                        OpKind::AbiFixed { dom, rep },
                        vec![Port {
                            node: output,
                            role: PortRole::Output,
                        }],
                    ));
                }

                for input in &inputs {
                    graph.add_edge(HEdge::constraint(
                        OpKind::BackendCrossing {
                            from_lang: "O".to_string(),
                            to_lang: lang.clone(),
                        },
                        vec![
                            Port {
                                node: *input,
                                role: PortRole::Input,
                            },
                            Port {
                                node: output,
                                role: PortRole::Output,
                            },
                        ],
                    ));
                }

                if let Some(policy) = plan_node.kind.eval_cache_policy() {
                    add_control_relation(
                        graph,
                        OpKind::Request {
                            kind: "eval".to_string(),
                        },
                        &inputs,
                        output,
                    );
                    add_control_relation(
                        graph,
                        OpKind::CacheMemo {
                            cacheable: policy.cacheable(),
                        },
                        &inputs,
                        output,
                    );
                }
            }
            PlanNodeKind::Request { kind, .. } => {
                add_control_relation(
                    graph,
                    OpKind::Request {
                        kind: kind.label().to_string(),
                    },
                    &inputs,
                    output,
                );
                if let Some(policy) = plan_node.kind.eval_cache_policy() {
                    add_control_relation(
                        graph,
                        OpKind::CacheMemo {
                            cacheable: policy.cacheable(),
                        },
                        &inputs,
                        output,
                    );
                }
            }
            PlanNodeKind::Group { mode, .. } => {
                add_control_relation(graph, group_op(*mode), &inputs, output);
            }
            PlanNodeKind::Schedule { kind, .. } => {
                add_control_relation(
                    graph,
                    OpKind::Schedule {
                        kind: kind.label().to_string(),
                    },
                    &inputs,
                    output,
                );
            }
            PlanNodeKind::Text
            | PlanNodeKind::Load { .. }
            | PlanNodeKind::Store { .. }
            | PlanNodeKind::Call { .. } => {}
        }
    }
}

fn structural_inputs(
    plan: &ExecutionPlan,
    node_map: &HashMap<PlanNodeId, NodeId>,
    parent: PlanNodeId,
) -> Vec<NodeId> {
    let mut children = plan
        .edges
        .iter()
        .filter(|edge| edge.kind == PlanEdgeKind::Structural && edge.to == parent)
        .map(|edge| (edge.from.0, node_map[&edge.from]))
        .collect::<Vec<_>>();
    children.sort_by_key(|(id, _)| *id);
    children.into_iter().map(|(_, node)| node).collect()
}

fn add_control_relation(graph: &mut HGraph, kind: OpKind, inputs: &[NodeId], output: NodeId) {
    let mut ports = inputs
        .iter()
        .copied()
        .map(|node| Port {
            node,
            role: PortRole::Input,
        })
        .collect::<Vec<_>>();
    ports.push(Port {
        node: output,
        role: PortRole::Output,
    });
    graph.add_edge(HEdge::constraint(kind, ports));
}

fn group_op(mode: GroupMode) -> OpKind {
    match mode {
        GroupMode::Batch => OpKind::Batch,
        GroupMode::All => OpKind::All,
        GroupMode::Any => OpKind::Any,
        GroupMode::Race => OpKind::Race,
    }
}

fn backend_output_constraints(backend: &BackendInterface) -> Option<(DomainFlags, RepFlags)> {
    if !backend.pure {
        return None;
    }
    match backend.canonical.as_str() {
        "html" | "markdown" | "latex" | "text" => Some((DomainFlags::STRING, RepFlags::STR)),
        _ => None,
    }
}

#[cfg(test)]
mod world_resource_key_tests {
    use super::*;
    use crate::hgraph::HNodeKind;
    use crate::world::{
        NodeGeneration, NodeId, NodeIdentity, ResourceGeneration, ResourceId, ResourceIdentity,
        ResourceOwner, WorldId,
    };

    #[test]
    fn world_resource_keys_share_the_generic_hgraph_state_chain() {
        let node = NodeIdentity::new(
            WorldId::new("desk").unwrap(),
            NodeId::new("node-a").unwrap(),
            NodeGeneration::new(2).unwrap(),
        );
        let resource = ResourceIdentity::new(
            ResourceOwner::Node { node },
            ResourceId::new("device/gpu-0").unwrap(),
            ResourceGeneration::new(5).unwrap(),
        );
        let generic = ResourceKey::GovernedResource(resource.clone());

        let mut generic_writer = EffectSummary::pure();
        generic_writer.writes.insert(generic.clone());
        let mut device_writer = EffectSummary::pure();
        device_writer
            .writes
            .insert(ResourceKey::DeviceState(resource));

        let mut accelerator_writer = EffectSummary::pure();
        accelerator_writer
            .writes
            .insert(ResourceKey::AcceleratorState(match &generic {
                ResourceKey::GovernedResource(resource) => resource.clone(),
                _ => unreachable!(),
            }));

        let mut graph = HGraph::default();
        let mut lowering = StateLowering::default();
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut generic_heads = Vec::new();
        for (plan_node, summary) in [generic_writer, device_writer, accelerator_writer]
            .into_iter()
            .enumerate()
        {
            let completion = graph.add_node(HNode::completion(PlanNodeId(plan_node)));
            add_resource_state_transitions(
                &mut graph,
                &mut lowering,
                &summary,
                PlanNodeId(plan_node),
                completion,
                &mut inputs,
                &mut outputs,
            );
            generic_heads.push(lowering.frontiers[&generic].last_write);
        }

        assert_eq!(generic_heads[0].version, 1);
        assert_eq!(generic_heads[1].version, 2);
        assert_eq!(generic_heads[2].version, 3);
        let versions = generic_heads
            .iter()
            .map(|head| match &graph.node(head.node).unwrap().kind {
                HNodeKind::ResourceState { resource, version } if resource == &generic => *version,
                other => panic!("unexpected generic resource node {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(versions, [1, 2, 3]);
        assert!(inputs.contains(&generic_heads[0].node));
        assert!(inputs.contains(&generic_heads[1].node));
        assert!(outputs.contains(&generic_heads[2].node));
        assert!(graph.node_ids().into_iter().any(|node| {
            matches!(
                &graph.node(node).unwrap().kind,
                HNodeKind::ResourceState {
                    resource: ResourceKey::DeviceState(_),
                    ..
                }
            )
        }));
        assert!(graph.node_ids().into_iter().any(|node| {
            matches!(
                &graph.node(node).unwrap().kind,
                HNodeKind::ResourceState {
                    resource: ResourceKey::AcceleratorState(_),
                    ..
                }
            )
        }));
    }
}
