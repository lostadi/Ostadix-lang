//! Hypergraph substrate for O execution and value-fidelity analysis.
//!
//! Semantic values, resource versions, successful-completion tokens, and
//! branch controls are nodes. Executable operations and typed dependency
//! relations are hyperedges. Persistent backend state is modeled explicitly as
//! an actor-state resource, not by a hidden actor scheduler.

pub mod from_oir;
pub mod graph;
pub mod kinds;
pub mod schedule;
pub mod solve;

pub use graph::{
    ActorId, EdgeId, ExecInfo, HEdge, HGraph, HNode, HNodeKind, NodeId, Port, PortRole,
    SequenceDependency,
};
pub use kinds::{
    AdmissionFactKind, ConstraintOp, DomainFlags, ExecutableOp, HEdgeKind, MemOrder, OcoreOpKind,
    OpKind, ReadyInputPolicy, RepFlags, ValueState,
};
pub use schedule::{schedule, try_schedule, ExecutionCluster, ReadyOp, ReadySchedule, Schedule};

#[cfg(test)]
mod tests {
    use num_bigint::BigInt;
    use proptest::prelude::*;

    use crate::{
        effects::ResourceKey,
        ir::{BackendRegistry, InvokeMode, OIr, OIrProgram},
        value::{AnnotationKind, Fidelity, GroupMode, ONumber, OValue},
    };

    use super::*;

    type SolverProjection = (DomainFlags, RepFlags, Option<Fidelity>, Option<OValue>);

    fn fidelity_strategy() -> impl Strategy<Value = Fidelity> {
        prop_oneof![
            Just(Fidelity::Lossless),
            Just(Fidelity::NativeCapsule),
            Just(Fidelity::Unsupported),
            proptest::collection::btree_set(0_u8..8, 1..=8).prop_map(|kinds| {
                Fidelity::structural(kinds.into_iter().map(|kind| match kind {
                    0 => AnnotationKind::TypeTag,
                    1 => AnnotationKind::NumericPrecision,
                    2 => AnnotationKind::NumericExactness,
                    3 => AnnotationKind::Encoding,
                    4 => AnnotationKind::Ordering,
                    5 => AnnotationKind::Identity,
                    6 => AnnotationKind::Constraint,
                    _ => AnnotationKind::Capability,
                }))
            }),
        ]
    }

    fn solve_dataflow_order(order: &[usize]) -> SolverProjection {
        let mut graph = HGraph::default();
        let dataflow_input = graph.add_node(HNode {
            fidelity: Some(Fidelity::Lossless),
            ..HNode::fresh()
        });
        let crossing_input = graph.add_node(HNode {
            value: Some(OValue::Number {
                v: ONumber::Rational {
                    num: BigInt::from(1),
                    den: BigInt::from(3),
                },
            }),
            domain: DomainFlags::NUMERIC,
            rep: RepFlags::BIG,
            ..HNode::fresh()
        });
        let output = graph.add_node(HNode::fresh());

        for relation in order {
            let edge = match relation {
                0 => HEdge::constraint(
                    OpKind::AbiFixed {
                        dom: DomainFlags::BOOL,
                        rep: RepFlags::BOOL,
                    },
                    vec![Port {
                        node: output,
                        role: PortRole::Output,
                    }],
                ),
                1 => HEdge::constraint(
                    OpKind::BackendCrossing {
                        from_lang: "O".into(),
                        to_lang: "javascript".into(),
                    },
                    vec![
                        Port {
                            node: crossing_input,
                            role: PortRole::Input,
                        },
                        Port {
                            node: output,
                            role: PortRole::Output,
                        },
                    ],
                ),
                2 => HEdge::constraint(
                    OpKind::DataFlow,
                    vec![
                        Port {
                            node: dataflow_input,
                            role: PortRole::Input,
                        },
                        Port {
                            node: output,
                            role: PortRole::Output,
                        },
                    ],
                ),
                other => panic!("unknown solver test relation {other}"),
            };
            graph.add_edge(edge);
        }

        solve::solve_types(&mut graph).unwrap();
        let output = graph.node(output).unwrap();
        (
            output.domain,
            output.rep,
            output.fidelity.clone(),
            output.value.clone(),
        )
    }

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
        solve::solve_types(&mut graph).unwrap();

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
        let mut actor_versions = graph
            .nodes
            .values()
            .filter_map(|node| match &node.kind {
                HNodeKind::ResourceState {
                    resource: ResourceKey::ActorState(_),
                    version,
                } => Some(*version),
                _ => None,
            })
            .collect::<Vec<_>>();
        actor_versions.sort_unstable();
        assert_eq!(actor_versions, vec![0, 1]);
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

        solve::solve_types(&mut graph).unwrap();
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
    fn dataflow_meets_abi_fixed_domain_and_representation() {
        let mut graph = HGraph::default();
        let input = graph.add_node(HNode::fresh());
        let output = graph.add_node(HNode::fresh());
        graph.add_edge(HEdge::constraint(
            OpKind::AbiFixed {
                dom: DomainFlags::BOOL,
                rep: RepFlags::BOOL,
            },
            vec![Port {
                node: output,
                role: PortRole::Output,
            }],
        ));
        graph.add_edge(HEdge::constraint(
            OpKind::DataFlow,
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

        solve::solve_types(&mut graph).unwrap();

        let output = graph.node(output).unwrap();
        assert_eq!(output.domain, DomainFlags::BOOL);
        assert_eq!(output.rep, RepFlags::BOOL);
    }

    #[test]
    fn dataflow_preserves_backend_crossing_fidelity_join() {
        let mut graph = HGraph::default();
        let crossing_input = graph.add_node(HNode {
            value: Some(OValue::Number {
                v: ONumber::Rational {
                    num: BigInt::from(1),
                    den: BigInt::from(3),
                },
            }),
            domain: DomainFlags::NUMERIC,
            rep: RepFlags::BIG,
            ..HNode::fresh()
        });
        let dataflow_input = graph.add_node(HNode {
            fidelity: Some(Fidelity::Lossless),
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
                    node: crossing_input,
                    role: PortRole::Input,
                },
                Port {
                    node: output,
                    role: PortRole::Output,
                },
            ],
        ));
        graph.add_edge(HEdge::constraint(
            OpKind::DataFlow,
            vec![
                Port {
                    node: dataflow_input,
                    role: PortRole::Input,
                },
                Port {
                    node: output,
                    role: PortRole::Output,
                },
            ],
        ));

        solve::solve_types(&mut graph).unwrap();

        assert_eq!(
            graph.node(output).and_then(|node| node.fidelity.clone()),
            Some(Fidelity::structural([
                AnnotationKind::NumericExactness,
                AnnotationKind::TypeTag,
            ]))
        );
    }

    #[test]
    fn dataflow_solution_is_independent_of_reversed_edge_insertion_order() {
        let forward = solve_dataflow_order(&[0, 1, 2]);
        let reversed = solve_dataflow_order(&[2, 1, 0]);

        assert_eq!(forward, reversed);
        assert_eq!(
            forward,
            (
                DomainFlags::BOOL,
                RepFlags::BOOL,
                Some(Fidelity::structural([
                    AnnotationKind::NumericExactness,
                    AnnotationKind::TypeTag,
                ])),
                None,
            )
        );
    }

    #[test]
    fn dataflow_solution_is_independent_of_every_edge_permutation() {
        let expected = solve_dataflow_order(&[0, 1, 2]);
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            assert_eq!(
                solve_dataflow_order(&order),
                expected,
                "solver result changed for edge order {order:?}"
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn dataflow_solution_is_independent_of_random_edge_permutations(
            keys in proptest::array::uniform3(any::<u64>()),
        ) {
            let mut order = [0_usize, 1, 2];
            order.sort_by_key(|relation| (keys[*relation], *relation));

            prop_assert_eq!(
                solve_dataflow_order(&order),
                solve_dataflow_order(&[0, 1, 2]),
            );
        }

        #[test]
        fn fidelity_join_is_associative_commutative_idempotent_with_identity(
            a in fidelity_strategy(),
            b in fidelity_strategy(),
            c in fidelity_strategy(),
        ) {
            prop_assert_eq!(
                a.clone().compose(b.clone()),
                b.clone().compose(a.clone()),
            );
            prop_assert_eq!(
                a.clone().compose(b.clone()).compose(c.clone()),
                a.clone().compose(b.clone().compose(c.clone())),
            );
            prop_assert_eq!(a.clone().compose(a.clone()), a.clone());
            prop_assert_eq!(a.clone().compose(Fidelity::Lossless), a.clone());
            prop_assert_eq!(Fidelity::Lossless.compose(a.clone()), a);
        }
    }

    #[test]
    fn dataflow_rejects_multiple_value_inputs() {
        let mut graph = HGraph::default();
        let first = graph.add_node(HNode::fresh());
        let second = graph.add_node(HNode::fresh());
        let output = graph.add_node(HNode::fresh());
        let edge = graph.add_edge(HEdge::constraint(
            OpKind::DataFlow,
            vec![
                Port {
                    node: first,
                    role: PortRole::Input,
                },
                Port {
                    node: second,
                    role: PortRole::Input,
                },
                Port {
                    node: output,
                    role: PortRole::Output,
                },
            ],
        ));

        assert_eq!(
            solve::solve_types(&mut graph).unwrap_err(),
            solve::SolveError::InvalidDataFlowShape {
                edge,
                value_inputs: 2,
                value_outputs: 1,
                non_value_ports: 0,
            }
        );
    }

    #[test]
    fn dataflow_rejects_missing_value_output() {
        let mut graph = HGraph::default();
        let input = graph.add_node(HNode::fresh());
        let edge = graph.add_edge(HEdge::constraint(
            OpKind::DataFlow,
            vec![Port {
                node: input,
                role: PortRole::Input,
            }],
        ));

        assert_eq!(
            solve::solve_types(&mut graph).unwrap_err(),
            solve::SolveError::InvalidDataFlowShape {
                edge,
                value_inputs: 1,
                value_outputs: 0,
                non_value_ports: 0,
            }
        );
    }

    #[test]
    fn dataflow_rejects_multiple_producers_for_one_destination() {
        let mut graph = HGraph::default();
        let first_input = graph.add_node(HNode::fresh());
        let second_input = graph.add_node(HNode::fresh());
        let output = graph.add_node(HNode::fresh());
        let mut producers = Vec::new();
        for input in [first_input, second_input] {
            producers.push(graph.add_edge(HEdge::constraint(
                OpKind::DataFlow,
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
            )));
        }

        assert_eq!(
            solve::solve_types(&mut graph).unwrap_err(),
            solve::SolveError::MultipleDataFlowProducers {
                node: output,
                first: producers[0],
                second: producers[1],
            }
        );
    }

    #[test]
    fn dataflow_rejects_duplicate_destination_ports() {
        let mut graph = HGraph::default();
        let input = graph.add_node(HNode::fresh());
        let output = graph.add_node(HNode::fresh());
        let edge = graph.add_edge(HEdge::constraint(
            OpKind::DataFlow,
            vec![
                Port {
                    node: input,
                    role: PortRole::Input,
                },
                Port {
                    node: output,
                    role: PortRole::Output,
                },
                Port {
                    node: output,
                    role: PortRole::Output,
                },
            ],
        ));

        assert_eq!(
            solve::solve_types(&mut graph).unwrap_err(),
            solve::SolveError::DuplicateDataFlowDestination { edge, node: output }
        );
    }

    #[test]
    fn graph_seeded_backend_specific_fidelity_vocabulary_converges() {
        let mut graph = HGraph::default();
        let first_kind = AnnotationKind::BackendSpecific {
            lang: "python".into(),
            label: "dtype".into(),
        };
        let second_kind = AnnotationKind::BackendSpecific {
            lang: "python".into(),
            label: "shape".into(),
        };
        let input = graph.add_node(HNode {
            fidelity: Some(Fidelity::structural([second_kind.clone()])),
            ..HNode::fresh()
        });
        let output = graph.add_node(HNode {
            fidelity: Some(Fidelity::structural([first_kind.clone()])),
            ..HNode::fresh()
        });
        graph.add_edge(HEdge::constraint(
            OpKind::DataFlow,
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

        solve::solve_types(&mut graph).unwrap();

        assert_eq!(
            graph.node(output).and_then(|node| node.fidelity.clone()),
            Some(Fidelity::structural([first_kind, second_kind]))
        );
    }

    #[test]
    fn conflicting_bounded_literals_return_typed_error() {
        let mut graph = HGraph::default();
        let output = graph.add_node(HNode::fresh());
        for value in [BigInt::from(3), BigInt::from(5)] {
            graph.add_edge(HEdge::constraint(
                OpKind::Bounded { value },
                vec![Port {
                    node: output,
                    role: PortRole::Output,
                }],
            ));
        }

        let error = solve::solve_types(&mut graph).unwrap_err();

        assert_eq!(
            error,
            solve::SolveError::ConflictingMaterializedValue {
                edge: EdgeId(1),
                node: output,
                existing: Box::new(OValue::big_int(BigInt::from(3))),
                incoming: Box::new(OValue::big_int(BigInt::from(5))),
            }
        );
    }

    #[test]
    fn bounded_and_dataflow_conflicts_fail_in_both_edge_orders() {
        let three = OValue::big_int(BigInt::from(3));
        let five = OValue::big_int(BigInt::from(5));

        for dataflow_first in [false, true] {
            let mut graph = HGraph::default();
            let input = graph.add_node(HNode::with_value(five.clone()));
            let output = graph.add_node(HNode::fresh());
            let dataflow = HEdge::constraint(
                OpKind::DataFlow,
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
            );
            let bounded = HEdge::constraint(
                OpKind::Bounded {
                    value: BigInt::from(3),
                },
                vec![Port {
                    node: output,
                    role: PortRole::Output,
                }],
            );
            if dataflow_first {
                graph.add_edge(dataflow);
                graph.add_edge(bounded);
            } else {
                graph.add_edge(bounded);
                graph.add_edge(dataflow);
            }

            let solve::SolveError::ConflictingMaterializedValue {
                edge,
                node,
                existing,
                incoming,
            } = solve::solve_types(&mut graph).unwrap_err()
            else {
                panic!("expected a materialized-value conflict");
            };
            assert_eq!(edge, EdgeId(1));
            assert_eq!(node, output);
            assert!(
                (*existing == three && *incoming == five)
                    || (*existing == five && *incoming == three)
            );
        }
    }

    #[test]
    fn matching_bounded_and_dataflow_values_converge_in_both_edge_orders() {
        for dataflow_first in [false, true] {
            let mut graph = HGraph::default();
            let value = OValue::big_int(BigInt::from(3));
            let input = graph.add_node(HNode::with_value(value.clone()));
            let output = graph.add_node(HNode::fresh());
            let dataflow = HEdge::constraint(
                OpKind::DataFlow,
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
            );
            let bounded = HEdge::constraint(
                OpKind::Bounded {
                    value: BigInt::from(3),
                },
                vec![Port {
                    node: output,
                    role: PortRole::Output,
                }],
            );
            if dataflow_first {
                graph.add_edge(dataflow);
                graph.add_edge(bounded);
            } else {
                graph.add_edge(bounded);
                graph.add_edge(dataflow);
            }

            solve::solve_types(&mut graph).unwrap();
            assert_eq!(
                graph.node(output).and_then(|node| node.value.clone()),
                Some(value)
            );
        }
    }

    #[test]
    fn solver_budget_exhaustion_returns_typed_error() {
        let mut graph = HGraph::default();
        let output = graph.add_node(HNode::fresh());
        let edge = graph.add_edge(HEdge::constraint(
            OpKind::AbiFixed {
                dom: DomainFlags::BOOL,
                rep: RepFlags::BOOL,
            },
            vec![Port {
                node: output,
                role: PortRole::Output,
            }],
        ));

        let solve::SolveError::BudgetExhausted(diagnostics) =
            solve::solve_types_with_budget(&mut graph, 1).unwrap_err()
        else {
            panic!("expected solver budget exhaustion");
        };
        assert_eq!(diagnostics.completed_passes, 1);
        assert_eq!(diagnostics.slot_updates, 2);
        assert!(diagnostics.derived_pass_bound > diagnostics.applied_pass_limit);
        assert_eq!(diagnostics.applied_pass_limit, 1);
        assert!(diagnostics.limit_is_below_derived_bound);
        assert_eq!(diagnostics.last_changed_edge, Some(edge));
        assert_eq!(diagnostics.last_changed_node, Some(output));
        assert_eq!(diagnostics.last_changed_slot, Some("representation"));
        assert_eq!(
            diagnostics.last_before.as_deref(),
            Some("representation bits 0x07ff")
        );
        assert_eq!(
            diagnostics.last_after.as_deref(),
            Some("representation bits 0x0200")
        );
        assert_eq!(diagnostics.recent_changed_edges, vec![edge]);
    }

    #[test]
    fn solver_budget_diagnostic_bounds_recent_changed_edge_trace() {
        let mut graph = HGraph::default();
        let mut edges = Vec::new();
        for _ in 0..20 {
            let output = graph.add_node(HNode::fresh());
            edges.push(graph.add_edge(HEdge::constraint(
                OpKind::AbiFixed {
                    dom: DomainFlags::BOOL,
                    rep: RepFlags::BOOL,
                },
                vec![Port {
                    node: output,
                    role: PortRole::Output,
                }],
            )));
        }

        let solve::SolveError::BudgetExhausted(diagnostics) =
            solve::solve_types_with_budget(&mut graph, 1).unwrap_err()
        else {
            panic!("expected solver budget exhaustion");
        };
        assert_eq!(diagnostics.slot_updates, 40);
        assert_eq!(diagnostics.recent_changed_edges, edges[4..]);
        assert_eq!(diagnostics.last_changed_edge, edges.last().copied());
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

        solve::solve_types(&mut graph).unwrap();
        assert_eq!(
            graph.node(output).and_then(|node| node.fidelity.clone()),
            Some(Fidelity::structural([
                AnnotationKind::NumericPrecision,
                AnnotationKind::NumericExactness,
                AnnotationKind::TypeTag,
            ]))
        );
    }

    #[test]
    fn javascript_integer_fidelity_respects_the_exact_2_pow_53_boundary() {
        let boundary = BigInt::from(1_u8) << 53_usize;
        let at_boundary =
            solve::fidelity_for_value(&OValue::big_int(boundary.clone()), "javascript");
        let above_boundary =
            solve::fidelity_for_value(&OValue::big_int(boundary + 1_u8), "javascript");

        let at_losses = at_boundary.losses().expect("numeric kind collapse");
        assert!(!at_losses.contains(&AnnotationKind::NumericPrecision));
        assert!(at_losses.contains(&AnnotationKind::NumericExactness));
        let above_losses = above_boundary.losses().expect("numeric fidelity loss");
        assert!(above_losses.contains(&AnnotationKind::NumericPrecision));
        assert!(above_losses.contains(&AnnotationKind::NumericExactness));
    }

    #[test]
    fn abstract_i64_crossing_uses_backend_capabilities_not_language_names() {
        let node = HNode {
            domain: DomainFlags::INTEGER,
            rep: RepFlags::I64,
            ..HNode::fresh()
        };

        let javascript = solve::fidelity_for(&node, "O", "javascript");
        assert!(
            javascript
                .losses()
                .is_some_and(|lost| lost.contains(&AnnotationKind::NumericPrecision)),
            "abstract I64 includes values outside JavaScript's exact integer range"
        );
        assert_eq!(
            solve::fidelity_for(&node, "O", "python"),
            Fidelity::Lossless
        );
        assert_eq!(
            solve::fidelity_for(&node, "O", "py"),
            Fidelity::Lossless,
            "aliases must resolve through the same canonical capability descriptor"
        );
        assert_eq!(
            solve::fidelity_for(&node, "O", "unregistered-backend"),
            Fidelity::Unsupported
        );
    }

    #[test]
    fn fidelity_phase_waits_for_type_and_value_fixpoint() {
        let mut graph = HGraph::default();
        let input = graph.add_node(HNode::fresh());
        let output = graph.add_node(HNode::fresh());

        // Deliberately insert the crossing before the fact that materializes
        // its input. A single mixed phase would seed Unsupported here and the
        // ascending join could never recover the precise structural result.
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
        graph.add_edge(HEdge::constraint(
            OpKind::Bounded {
                value: (BigInt::from(1_u8) << 53_usize) + 1_u8,
            },
            vec![Port {
                node: input,
                role: PortRole::Output,
            }],
        ));

        solve::solve_types(&mut graph).unwrap();
        let fidelity = graph
            .node(output)
            .and_then(|node| node.fidelity.as_ref())
            .expect("crossing fidelity");
        assert_ne!(fidelity, &Fidelity::Unsupported);
        assert!(fidelity
            .losses()
            .is_some_and(|lost| lost.contains(&AnnotationKind::NumericPrecision)));
    }

    #[test]
    fn same_backend_aliases_are_lossless_but_unknown_backends_are_not() {
        let node = HNode::fresh();
        assert_eq!(
            solve::fidelity_for(&node, "py", "python"),
            Fidelity::Lossless
        );
        assert_eq!(
            solve::fidelity_for(&node, "mystery", "mystery"),
            Fidelity::Unsupported,
            "syntactic equality is not capability evidence"
        );
    }

    #[test]
    fn backend_crossing_fixpoint_terminates_for_multi_kind_structural_loss() {
        // Regression for the non-terminating solve_types case: a crossing
        // whose loss set has 2+ distinct AnnotationKinds (here a rational
        // number into a backend without rich-number support) is recomposed
        // against itself on every pass of solve_types's `while changed` loop.
        // Under the old Vec<AnnotationKind> + dedup() accumulator this grew
        // without bound and never terminated. If this test completes at all,
        // the fixpoint converged; the assertion checks it converged to the
        // right two-element set rather than some other fixed point.
        let mut graph = HGraph::default();
        let input = graph.add_node(HNode {
            value: Some(OValue::Number {
                v: ONumber::Rational {
                    num: BigInt::from(1),
                    den: BigInt::from(3),
                },
            }),
            domain: DomainFlags::NUMERIC,
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

        solve::solve_types(&mut graph).unwrap();
        assert_eq!(
            graph.node(output).and_then(|node| node.fidelity.clone()),
            Some(Fidelity::structural([
                AnnotationKind::NumericExactness,
                AnnotationKind::TypeTag,
            ]))
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
                    env_id: 0,
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
                    env_id: 0,
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
            env_id: 0,
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
    fn ready_schedule_parallelizes_explicit_autonomous_ephemeral_group() {
        let registry = BackendRegistry::global();
        let python = |value| OIr::Exec {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            backend: registry.interface_for("python"),
            body: vec![OIr::Text(format!("__oval_result__ = {value}"))],
        };
        let program = OIrProgram {
            nodes: vec![OIr::Invoke {
                fn_name: "autonomous".into(),
                mode: InvokeMode::Autonomous,
                args: vec![OIr::Invoke {
                    fn_name: "batch".into(),
                    mode: InvokeMode::Group(GroupMode::Batch),
                    args: vec![python(1), python(2), python(3), python(4)],
                }],
            }],
        };

        let graph = program.hgraph();
        let waves = ReadySchedule::derive(&graph).unwrap().waves().unwrap();
        assert_eq!(
            waves.first(),
            Some(&vec![
                crate::ir::PlanNodeId(2),
                crate::ir::PlanNodeId(4),
                crate::ir::PlanNodeId(6),
                crate::ir::PlanNodeId(8),
            ])
        );
    }

    #[test]
    fn inner_lazy_policy_blocks_outer_autonomous_hosted_dispatch() {
        let registry = BackendRegistry::global();
        let python = |value| OIr::Exec {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            backend: registry.interface_for("python"),
            body: vec![OIr::Text(format!("__oval_result__ = {value}"))],
        };
        let program = OIrProgram {
            nodes: vec![OIr::Invoke {
                fn_name: "autonomous".into(),
                mode: InvokeMode::Autonomous,
                args: vec![OIr::Invoke {
                    fn_name: "lazy".into(),
                    mode: InvokeMode::Lazy,
                    args: vec![OIr::Invoke {
                        fn_name: "batch".into(),
                        mode: InvokeMode::Group(GroupMode::Batch),
                        args: vec![python(1), python(2)],
                    }],
                }],
            }],
        };
        let plan = program.plan();
        let flat = program.flatten_for_plan();
        for node in &plan.nodes {
            if matches!(node.kind, crate::ir::PlanNodeKind::Exec { .. }) {
                assert_eq!(
                    crate::hgraph::from_oir::autonomous_ephemeral_group(
                        &plan,
                        node.id,
                        flat[node.id.0]
                    ),
                    None,
                    "an inner lazy(...) policy must override an outer autonomous(...)"
                );
            }
        }
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
