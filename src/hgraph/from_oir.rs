use std::collections::HashMap;

use crate::{
    ir::{
        BackendInterface, ExecutionMode, ExecutionPlan, OIr, OIrProgram, PlanEdgeKind, PlanNodeId,
        PlanNodeKind, PlanScheduleKind,
    },
    value::{GroupMode, OValue},
};

use super::{
    graph::{ActorId, HEdge, HGraph, HNode, NodeId, Port, PortRole},
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

    let oir_nodes = program.flatten_for_plan();
    if oir_nodes.len() != plan.nodes.len() {
        return Err(format!(
            "OIR flatten produced {} nodes for execution plan with {} nodes",
            oir_nodes.len(),
            plan.nodes.len()
        ));
    }

    let mut graph = HGraph::default();
    let mut node_map: HashMap<PlanNodeId, NodeId> = HashMap::new();

    for plan_node in &plan.nodes {
        let oir = oir_nodes[plan_node.id.0];
        let graph_node = graph.add_node(hnode_for_oir(oir));
        if let Some(node) = graph.node_mut(graph_node) {
            node.plan_node = Some(plan_node.id);
        }
        if let PlanNodeKind::Exec { lang, env_id, .. } = &plan_node.kind {
            if *env_id != u32::MAX {
                if let Some(node) = graph.node_mut(graph_node) {
                    node.actor = Some(ActorId {
                        lang: intern_lang(lang),
                        env: *env_id,
                    });
                }
            }
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
    add_execute_edges(&mut graph, plan, &node_map);
    Ok(graph)
}

/// Lower every non-literal plan operation to an executable operation hyperedge
/// that consumes its structural children and data-dependency value nodes and
/// produces its own output value node. Literal `Text` nodes are materialized
/// value nodes with no Execute edge.
fn add_execute_edges(
    graph: &mut HGraph,
    plan: &ExecutionPlan,
    node_map: &HashMap<PlanNodeId, NodeId>,
) {
    for plan_node in &plan.nodes {
        let Some(op) = executable_op(&plan_node.kind) else {
            continue;
        };
        let output = node_map[&plan_node.id];
        let inputs = operation_inputs(plan, node_map, plan_node.id);
        let ordinal = plan_node.id.0 as u64;
        graph.add_exec_edge(plan_node.id, op, inputs, output, ordinal);
    }
}

/// Value-node inputs an operation consumes: its structural children (in source
/// order) followed by any additional data predecessors. Both must be
/// materialized before the operation becomes ready.
fn operation_inputs(
    plan: &ExecutionPlan,
    node_map: &HashMap<PlanNodeId, NodeId>,
    parent: PlanNodeId,
) -> Vec<NodeId> {
    let mut seen = std::collections::HashSet::new();
    let mut ordered: Vec<(usize, NodeId)> = Vec::new();

    for edge in &plan.edges {
        let is_input = (edge.kind == PlanEdgeKind::Structural || edge.kind == PlanEdgeKind::Data)
            && edge.to == parent;
        if is_input && seen.insert(edge.from) {
            ordered.push((edge.from.0, node_map[&edge.from]));
        }
    }
    ordered.sort_by_key(|(source, _)| *source);
    ordered.into_iter().map(|(_, node)| node).collect()
}

fn executable_op(kind: &PlanNodeKind) -> Option<ExecutableOp> {
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
            lang,
            env_id,
            backend,
            ..
        } => match backend.execution {
            ExecutionMode::InlineAst | ExecutionMode::InlineValue => {
                Some(ExecutableOp::InlineBackend { lang: lang.clone() })
            }
            ExecutionMode::Shim => Some(ExecutableOp::EvalBackend {
                lang: lang.clone(),
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
            PlanNodeKind::Exec {
                lang,
                env_id,
                backend,
                ..
            } => {
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

                if *env_id != u32::MAX {
                    let actor = ActorId {
                        lang: intern_lang(lang),
                        env: *env_id,
                    };
                    graph.add_edge(HEdge::constraint(
                        OpKind::ActorSerial { actor },
                        vec![Port {
                            node: output,
                            role: PortRole::InOut,
                        }],
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

fn intern_lang(lang: &str) -> u32 {
    lang.bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32))
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
