//! Hypergraph substrate for O execution and value-fidelity analysis.
//!
//! Values are nodes. Operations, dependencies, actor constraints, and group
//! barriers are hyperedges. This mirrors the design note in the pasted brief:
//! type/fidelity facts live on values, while operations are relations over
//! those values.

pub mod from_oir;
pub mod graph;
pub mod kinds;
pub mod schedule;
pub mod solve;

pub use graph::{ActorId, EdgeId, HEdge, HGraph, HNode, NodeId, Port, PortRole};
pub use kinds::{DomainFlags, MemOrder, OcoreOpKind, OpKind, RepFlags};
pub use schedule::{schedule, try_schedule, ExecutionCluster, Schedule};

#[cfg(test)]
mod tests {
    use num_bigint::BigInt;

    use crate::{
        ir::{BackendRegistry, InvokeMode, OIr, OIrProgram},
        value::{AnnotationKind, Fidelity, GroupMode, ONumber, OValue},
    };

    use super::*;

    #[test]
    fn oir_hgraph_records_core_execution_relations() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "x".into(),
                    expr: Box::new(OIr::Text("9223372036854775808".into())),
                },
                OIr::Exec {
                    lang: "python".into(),
                    env_id: 0,
                    attr: None,
                    backend: BackendRegistry::global().interface_for("python"),
                    body: vec![OIr::Load("x".into())],
                },
                OIr::Invoke {
                    fn_name: "batch".into(),
                    mode: InvokeMode::Group(GroupMode::Batch),
                    args: vec![OIr::Text("1".into()), OIr::Text("2".into())],
                },
            ],
        };

        let mut graph = program.hgraph();
        solve::solve_types(&mut graph);

        assert_eq!(graph.root_nodes.len(), 3);
        for root in &graph.root_nodes {
            assert!(
                graph.ir_map.contains_key(root),
                "root nodes must retain OIR provenance"
            );
        }
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::DataFlow)));
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::Sequence)));
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::StructuralBarrier)));
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::ActorSerial { .. })));
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::Batch)));

        let big_literal = graph
            .nodes
            .values()
            .find(|node| node.value == Some(OValue::str_("9223372036854775808")))
            .expect("OIR text should become a graph node");
        assert!(big_literal.domain.contains(DomainFlags::STRING));
        assert_eq!(big_literal.rep, RepFlags::STR);

        let schedule = schedule::try_schedule(&graph).unwrap();
        assert_eq!(schedule.root_order(&graph).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn bounded_integer_nodes_materialize_number_int() {
        let mut graph = HGraph::default();
        let out = graph.add_node(HNode::fresh());
        let bigint = BigInt::from(i64::MAX) + BigInt::from(1_u8);
        graph.add_edge(HEdge {
            id: EdgeId(0),
            kind: OpKind::Bounded {
                value: bigint.clone(),
            },
            ports: vec![Port {
                node: out,
                role: PortRole::Output,
            }],
        });

        solve::solve_types(&mut graph);
        let node = graph.node(out).unwrap();
        assert!(node.domain.contains(DomainFlags::INTEGER));
        assert_eq!(node.rep, RepFlags::BIG);
        assert_eq!(
            node.value,
            Some(OValue::Number {
                v: ONumber::Int { v: bigint }
            })
        );
    }

    #[test]
    fn backend_crossing_marks_bigint_precision_loss_for_fixed_width_backend() {
        let mut graph = HGraph::default();
        let input = graph.add_node(HNode {
            value: Some(OValue::big_int(BigInt::from(i64::MAX) + BigInt::from(1_u8))),
            domain: DomainFlags::INTEGER,
            rep: RepFlags::BIG,
            ..HNode::fresh()
        });
        let output = graph.add_node(HNode::fresh());
        graph.add_edge(HEdge {
            id: EdgeId(0),
            kind: OpKind::BackendCrossing {
                from_lang: "O".into(),
                to_lang: "javascript".into(),
            },
            ports: vec![
                Port {
                    node: input,
                    role: PortRole::Input,
                },
                Port {
                    node: output,
                    role: PortRole::Output,
                },
            ],
        });

        solve::solve_types(&mut graph);
        assert_eq!(
            graph.node(output).and_then(|node| node.fidelity.clone()),
            Some(Fidelity::Structural {
                lost: vec![AnnotationKind::NumericPrecision],
            })
        );
    }

    #[test]
    fn actor_serial_edges_prevent_same_actor_parallel_cluster() {
        let mut graph = HGraph::default();
        let actor = ActorId { lang: 1, env: 0 };
        let first = graph.add_node(HNode {
            actor: Some(actor),
            ..HNode::fresh()
        });
        let second = graph.add_node(HNode {
            actor: Some(actor),
            ..HNode::fresh()
        });
        for node in [first, second] {
            graph.add_edge(HEdge {
                id: EdgeId(0),
                kind: OpKind::ActorSerial { actor },
                ports: vec![Port {
                    node,
                    role: PortRole::InOut,
                }],
            });
        }

        let schedule = schedule::schedule(&graph);
        let first_cluster = schedule
            .clusters
            .iter()
            .position(|cluster| cluster.nodes.contains(&first))
            .unwrap();
        let second_cluster = schedule
            .clusters
            .iter()
            .position(|cluster| cluster.nodes.contains(&second))
            .unwrap();

        assert!(first_cluster < second_cluster);
        assert!(!schedule.clusters[first_cluster].nodes.contains(&second));
    }

    #[test]
    fn scheduler_rejects_cycles() {
        let mut graph = HGraph::default();
        let left = graph.add_node(HNode::fresh());
        let right = graph.add_node(HNode::fresh());
        graph.add_edge(HEdge {
            id: EdgeId(0),
            kind: OpKind::Sequence,
            ports: vec![
                Port {
                    node: left,
                    role: PortRole::Input,
                },
                Port {
                    node: right,
                    role: PortRole::Output,
                },
            ],
        });
        graph.add_edge(HEdge {
            id: EdgeId(0),
            kind: OpKind::Sequence,
            ports: vec![
                Port {
                    node: right,
                    role: PortRole::Input,
                },
                Port {
                    node: left,
                    role: PortRole::Output,
                },
            ],
        });

        assert!(schedule::try_schedule(&graph)
            .unwrap_err()
            .contains("cycle"));
    }
}
