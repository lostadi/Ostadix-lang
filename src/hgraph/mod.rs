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
pub use schedule::{schedule, ExecutionCluster, Schedule};

#[cfg(test)]
mod tests {
    use num_bigint::BigInt;

    use crate::{
        ir::{BackendRegistry, InvokeMode, OIr, OIrProgram},
        value::{AnnotationKind, Fidelity, GroupMode, OValue},
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
            .expect("integer-looking OIR text should become a graph node");
        assert!(big_literal.domain.contains(DomainFlags::INTEGER));
        assert_eq!(big_literal.rep, RepFlags::BIG);
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
}
