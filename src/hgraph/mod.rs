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

pub use graph::{ActorId, EdgeId, ExecInfo, HEdge, HGraph, HNode, NodeId, Port, PortRole};
pub use kinds::{
    ConstraintOp, DomainFlags, ExecutableOp, HEdgeKind, MemOrder, OcoreOpKind, OpKind, RepFlags,
    ValueState,
};
pub use schedule::{schedule, try_schedule, ExecutionCluster, ReadyOp, ReadySchedule, Schedule};

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
                OIr::Invoke {
                    fn_name: "instantiate".into(),
                    mode: InvokeMode::Eager,
                    args: vec![OIr::Text("{ name = \"demo\"; }".into())],
                },
                OIr::Invoke {
                    fn_name: "autonomous".into(),
                    mode: InvokeMode::Autonomous,
                    args: vec![OIr::Text("body".into())],
                },
            ],
        };

        let mut graph = program.hgraph();
        solve::solve_types(&mut graph);

        assert_eq!(graph.root_nodes.len(), 5);
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
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::Request { .. })));
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::Schedule { .. })));
        assert!(graph
            .edges
            .values()
            .any(|edge| matches!(edge.kind, OpKind::CacheMemo { cacheable: true })));

        let big_literal = graph
            .nodes
            .values()
            .find(|node| node.value == Some(OValue::str_("9223372036854775808")))
            .expect("OIR text should become a graph node");
        assert!(big_literal.domain.contains(DomainFlags::STRING));
        assert_eq!(big_literal.rep, RepFlags::STR);

        let schedule = schedule::try_schedule(&graph).unwrap();
        assert_eq!(schedule.root_order(&graph).unwrap(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn oir_hgraph_preserves_activation_request_kind() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Invoke {
                    fn_name: "dry_activate".into(),
                    mode: InvokeMode::Eager,
                    args: vec![OIr::Text("/nix/store/demo-system".into())],
                },
                OIr::Invoke {
                    fn_name: "activate".into(),
                    mode: InvokeMode::Eager,
                    args: vec![OIr::Text("/nix/store/demo-system".into())],
                },
            ],
        };

        let graph = program.hgraph();
        let request_kinds = graph
            .edges
            .values()
            .filter_map(|edge| match &edge.kind {
                OpKind::Request { kind } => Some(kind.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(request_kinds.contains(&"dry_activate"));
        assert!(request_kinds.contains(&"activate"));
        assert!(
            graph
                .edges
                .values()
                .filter(|edge| matches!(edge.kind, OpKind::CacheMemo { cacheable: false }))
                .count()
                >= 2
        );
    }

    #[test]
    fn bounded_integer_nodes_materialize_number_int() {
        let mut graph = HGraph::default();
        let out = graph.add_node(HNode::fresh());
        let bigint = BigInt::from(i64::MAX) + BigInt::from(1_u8);
        graph.add_edge(HEdge::constraint(
            OpKind::Bounded {
                value: bigint.clone(),
            },
            vec![Port {
                node: out,
                role: PortRole::Output,
            }],
        ));

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
        graph.add_edge(HEdge::constraint(
            OpKind::BackendCrossing {
                from_lang: "O".into(),
                to_lang: "javascript".into(),
            },
            vec![
                Port {
                    node: input,
                    role: PortRole::Input,
                },
                Port {
                    node: output,
                    role: PortRole::Output,
                },
            ],
        ));

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
            graph.add_edge(HEdge::constraint(
                OpKind::ActorSerial { actor },
                vec![Port {
                    node,
                    role: PortRole::InOut,
                }],
            ));
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
        graph.add_edge(HEdge::constraint(
            OpKind::Sequence,
            vec![
                Port {
                    node: left,
                    role: PortRole::Input,
                },
                Port {
                    node: right,
                    role: PortRole::Output,
                },
            ],
        ));
        graph.add_edge(HEdge::constraint(
            OpKind::Sequence,
            vec![
                Port {
                    node: right,
                    role: PortRole::Input,
                },
                Port {
                    node: left,
                    role: PortRole::Output,
                },
            ],
        ));

        assert!(schedule::try_schedule(&graph)
            .unwrap_err()
            .contains("cycle"));
    }

    #[test]
    fn ready_schedule_preserves_effectful_sequence_after_nested_dependency() {
        let registry = BackendRegistry::global();
        let program = OIrProgram {
            nodes: vec![
                OIr::Exec {
                    lang: "python".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: registry.interface_for("python"),
                    body: vec![OIr::Exec {
                        lang: "text".into(),
                        env_id: u32::MAX,
                        attr: None,
                        backend: registry.interface_for("text"),
                        body: vec![OIr::Text("A".into())],
                    }],
                },
                OIr::Exec {
                    lang: "python".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: registry.interface_for("python"),
                    body: vec![OIr::Text("B".into())],
                },
            ],
        };
        let plan = program.plan();

        assert!(plan.edges.iter().any(|edge| {
            edge.from == crate::ir::PlanNodeId(1)
                && edge.to == crate::ir::PlanNodeId(0)
                && edge.kind == crate::ir::PlanEdgeKind::Structural
        }));
        assert!(plan.edges.iter().any(|edge| {
            edge.from == crate::ir::PlanNodeId(0)
                && edge.to == crate::ir::PlanNodeId(3)
                && edge.kind == crate::ir::PlanEdgeKind::Sequence
        }));

        let graph = program.hgraph_for_plan(&plan).unwrap();
        let waves = ReadySchedule::derive(&graph).unwrap().waves().unwrap();
        assert_eq!(
            waves,
            vec![
                vec![crate::ir::PlanNodeId(1)],
                vec![crate::ir::PlanNodeId(0)],
                vec![crate::ir::PlanNodeId(3)],
            ]
        );
    }

    #[test]
    fn ready_schedule_preserves_sequence_across_literal_separator() {
        let registry = BackendRegistry::global();
        let python = || OIr::Exec {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            backend: registry.interface_for("python"),
            body: vec![OIr::Text("__oval_result__ = 1".into())],
        };
        let program = OIrProgram {
            nodes: vec![python(), OIr::Text("\n".into()), python()],
        };

        let graph = program.hgraph();
        assert_eq!(
            ReadySchedule::derive(&graph).unwrap().waves().unwrap(),
            vec![
                vec![crate::ir::PlanNodeId(0)],
                vec![crate::ir::PlanNodeId(3)],
            ]
        );
    }

    #[test]
    fn same_language_native_value_is_not_lossless() {
        use crate::value::{
            NativeBoundary, NativeCodecSafety, NativeIdentity, ONative, RehydratePolicy,
        };

        let mut node = HNode::fresh();
        node.value = Some(OValue::Native {
            v: ONative {
                lang: "python".into(),
                implementation: None,
                version: None,
                type_name: "socket".into(),
                identity: NativeIdentity {
                    stable: None,
                    live: Some("handle-1".into()),
                },
                codec: "opaque".into(),
                payload: None,
                boundary: NativeBoundary::Effectful,
                safety: NativeCodecSafety::LiveHandle,
                capabilities: Vec::new(),
                metadata: Default::default(),
                rehydrate: RehydratePolicy::SameProcess,
            },
        });

        // Two evaluators sharing the canonical language name are not the same
        // process: a process-bound native value must never be classified as a
        // lossless same-language crossing.
        assert_eq!(
            solve::fidelity_for(&node, "python", "python"),
            Fidelity::NativeCapsule
        );
        assert_eq!(
            solve::fidelity_for(&node, "python", "rust"),
            Fidelity::NativeCapsule
        );
    }
}
