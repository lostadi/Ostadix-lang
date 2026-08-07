//! Ontology and ready-operation scheduler tests for the HGraph executor.
//!
//! These assert the directed value/state/control projection: every non-literal
//! OIR operation lowers to one multi-output Execute hyperedge, source sequence
//! becomes a completion dependency when required, effect and actor resources
//! form versioned state chains, and only established independence reaches the
//! same ready wave.

use o_lang::effects::{ActorResourceId, ResourceKey};
use o_lang::hgraph::from_oir::build_program;
use o_lang::hgraph::{
    schedule::ReadySchedule, ExecInfo, ExecutableOp, HEdgeKind, HGraph, HNodeKind, NodeId,
};
use o_lang::ir::{
    BackendRegistry, ExecutionPlan, InvokeMode, OIr, OIrProgram, PlanNodeId, PlanNodeKind,
};
use o_lang::value::GroupMode;

fn html_backend() -> o_lang::ir::BackendInterface {
    BackendRegistry::global().interface_for("html")
}

fn inline_exec(lang: &str, body: &str) -> OIr {
    OIr::Exec {
        lang: lang.into(),
        env_id: u32::MAX,
        attr: None,
        backend: BackendRegistry::global().interface_for(lang),
        body: vec![OIr::Text(body.into())],
    }
}

fn shim_exec(lang: &str, env_id: u32, body: Vec<OIr>) -> OIr {
    OIr::Exec {
        lang: lang.into(),
        env_id,
        attr: None,
        backend: BackendRegistry::global().interface_for(lang),
        body,
    }
}

fn plan_exec_id(plan: &ExecutionPlan, lang: &str) -> PlanNodeId {
    plan.nodes
        .iter()
        .find_map(|node| match &node.kind {
            PlanNodeKind::Exec {
                lang: candidate, ..
            } if candidate == lang => Some(node.id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing {lang} execution node"))
}

fn wave_of(waves: &[Vec<PlanNodeId>], plan_node: PlanNodeId) -> usize {
    waves
        .iter()
        .position(|wave| wave.contains(&plan_node))
        .unwrap_or_else(|| panic!("plan node {} was not scheduled", plan_node.0))
}

fn resource_transition(
    graph: &HGraph,
    info: &ExecInfo,
    resource: &ResourceKey,
) -> (NodeId, u64, NodeId, u64) {
    let state = |node: NodeId| match &graph.node(node).expect("state node exists").kind {
        HNodeKind::ResourceState {
            resource: candidate,
            version,
        } if candidate == resource => Some(*version),
        _ => None,
    };
    let inputs = info
        .inputs
        .iter()
        .filter_map(|node| state(*node).map(|version| (*node, version)))
        .collect::<Vec<_>>();
    let outputs = info
        .outputs
        .iter()
        .filter_map(|node| state(*node).map(|version| (*node, version)))
        .collect::<Vec<_>>();
    assert_eq!(inputs.len(), 1, "missing unique {resource:?} input");
    assert_eq!(outputs.len(), 1, "missing unique {resource:?} output");
    (inputs[0].0, inputs[0].1, outputs[0].0, outputs[0].1)
}

fn resource_read_state(graph: &HGraph, info: &ExecInfo, resource: &ResourceKey) -> (NodeId, u64) {
    let state = |node: NodeId| match &graph.node(node).expect("state node exists").kind {
        HNodeKind::ResourceState {
            resource: candidate,
            version,
        } if candidate == resource => Some(*version),
        _ => None,
    };
    let inputs = info
        .inputs
        .iter()
        .filter_map(|node| state(*node).map(|version| (*node, version)))
        .collect::<Vec<_>>();
    let outputs = info
        .outputs
        .iter()
        .filter_map(|node| state(*node).map(|version| (*node, version)))
        .collect::<Vec<_>>();
    assert_eq!(inputs.len(), 1, "missing unique {resource:?} read input");
    assert!(
        outputs.is_empty(),
        "read unexpectedly advanced {resource:?}: {outputs:?}"
    );
    inputs[0]
}

#[test]
fn every_operation_kind_lowers_to_an_execute_edge() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "x".into(),
                expr: Box::new(OIr::Text("hi".into())),
            },
            OIr::Load("x".into()),
            OIr::Invoke {
                fn_name: "batch".into(),
                mode: InvokeMode::Group(GroupMode::Batch),
                args: vec![OIr::Text("a".into()), OIr::Text("b".into())],
            },
            OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: None,
                backend: html_backend(),
                body: vec![OIr::Text("body".into())],
            },
        ],
    };

    let plan = program.plan();
    let graph = build_program(&program);

    for node in &plan.nodes {
        let op = graph.op_for(node.id);
        match &node.kind {
            // Literal text is a materialized value node with no Execute edge.
            PlanNodeKind::Text => {
                assert!(op.is_none(), "text node {} must not have an op", node.id.0);
            }
            kind => {
                let info = op.unwrap_or_else(|| {
                    panic!("plan node {} ({kind:?}) has no Execute edge", node.id.0)
                });
                let edge = graph.exec_edge(info.edge).expect("exec edge exists");
                let executable = match &edge.op {
                    HEdgeKind::Execute(op) => op.clone(),
                    HEdgeKind::Constraint(_) => panic!("op edge must be Execute"),
                };
                match (kind, executable) {
                    (PlanNodeKind::Store { .. }, ExecutableOp::Store) => {}
                    (PlanNodeKind::Load { .. }, ExecutableOp::LoadBinding) => {}
                    (PlanNodeKind::Group { .. }, ExecutableOp::Group { .. }) => {}
                    (PlanNodeKind::Exec { .. }, ExecutableOp::InlineBackend { .. }) => {}
                    (kind, other) => {
                        panic!("plan node {kind:?} lowered to unexpected op {other:?}")
                    }
                }
            }
        }
    }
}

#[test]
fn every_executable_edge_has_one_value_and_one_completion_output() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "x".into(),
                expr: Box::new(OIr::Text("hi".into())),
            },
            OIr::Load("x".into()),
            OIr::Invoke {
                fn_name: "all".into(),
                mode: InvokeMode::Group(GroupMode::All),
                args: vec![OIr::Text("a".into()), OIr::Text("b".into())],
            },
            inline_exec("html", "body"),
        ],
    };
    let graph = build_program(&program);

    graph
        .validate_execution_graph()
        .expect("lowered graph validates");
    for info in graph.exec_ops_ordered() {
        let values = info
            .outputs
            .iter()
            .filter(|node| {
                matches!(
                    graph.node(**node).map(|node| &node.kind),
                    Some(HNodeKind::Value)
                )
            })
            .copied()
            .collect::<Vec<_>>();
        let completions = info
            .outputs
            .iter()
            .filter(|node| {
                matches!(
                    graph.node(**node).map(|node| &node.kind),
                    Some(HNodeKind::Completion { plan_node }) if *plan_node == info.plan_node
                )
            })
            .copied()
            .collect::<Vec<_>>();

        assert_eq!(values, vec![info.value_output]);
        assert_eq!(completions.len(), 1);
        assert_eq!(graph.completion_node(info.plan_node), Some(completions[0]));
        for output in &info.outputs {
            assert_eq!(graph.node(*output).unwrap().producer, Some(info.edge));
        }
    }
}

#[test]
fn store_execute_edge_consumes_its_expression_value() {
    let program = OIrProgram {
        nodes: vec![OIr::Store {
            name: "x".into(),
            expr: Box::new(OIr::Text("payload".into())),
        }],
    };
    let plan = program.plan();
    let graph = build_program(&program);

    // The store is plan node 1 (the text expr is a structural child, node 0).
    let store_id = plan
        .nodes
        .iter()
        .find(|n| matches!(n.kind, PlanNodeKind::Store { .. }))
        .unwrap()
        .id;
    let text_id = plan
        .nodes
        .iter()
        .find(|n| matches!(n.kind, PlanNodeKind::Text))
        .unwrap()
        .id;

    let store_op = graph.op_for(store_id).expect("store has an op");
    assert!(
        graph.op_for(text_id).is_none(),
        "literal text produces no Execute op"
    );

    // The store operation consumes the text value node as an input and produces
    // its own (distinct) output value node.
    assert!(
        !store_op.inputs.is_empty(),
        "store operation must consume its expression value node"
    );
    assert!(
        !store_op.inputs.contains(&store_op.value_output),
        "store output must differ from its inputs"
    );
}

#[test]
fn independent_siblings_are_concurrently_ready() {
    // Two independent inline blocks: no data dependency between them.
    let program = OIrProgram {
        nodes: vec![
            OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: None,
                backend: html_backend(),
                body: vec![OIr::Text("a".into())],
            },
            OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: None,
                backend: html_backend(),
                body: vec![OIr::Text("b".into())],
            },
        ],
    };
    let graph = build_program(&program);
    let schedule = ReadySchedule::derive(&graph).expect("schedule derives");
    let waves = schedule.waves().expect("acyclic");

    // Both inline operations should share the very first wave (no blocking deps
    // between them): independent siblings run concurrently.
    let first = &waves[0];
    assert_eq!(
        first.len(),
        2,
        "independent inline siblings must be concurrently ready, waves = {waves:?}"
    );
    assert!(
        graph.sequence_dependencies.is_empty(),
        "verified pure, infallible inline siblings may relax lexical sequence"
    );
    for info in graph.exec_ops_ordered() {
        assert!(
            info.inputs.iter().all(|node| !matches!(
                &graph.node(*node).unwrap().kind,
                HNodeKind::ResourceState {
                    resource: ResourceKey::HostWorld,
                    ..
                }
            )),
            "verified pure inline execution must not consume HostWorld"
        );
    }
}

#[test]
fn pure_outer_renderer_with_fallible_subtree_keeps_source_completion() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: None,
                backend: html_backend(),
                body: vec![OIr::Load("possibly_missing".into())],
            },
            inline_exec("html", "later"),
        ],
    };
    let plan = program.plan();
    let graph = build_program(&program);
    let [earlier, later] = plan.roots.as_slice() else {
        panic!("expected two roots")
    };

    // The outer renderer's own summary is pure, but forcing its structural
    // subtree can fail. The subtree proof must therefore retain fail-fast
    // source order instead of considering only the two root summaries.
    assert!(graph
        .effect_summary(*earlier)
        .unwrap()
        .is_verified_pure_infallible());
    let completion = graph.completion_node(*earlier).unwrap();
    assert!(graph.op_for(*later).unwrap().inputs.contains(&completion));
    assert!(graph.sequence_dependencies.iter().any(|dependency| {
        dependency.predecessor == *earlier
            && dependency.successor == *later
            && dependency.completion == completion
    }));
}

#[test]
fn pure_children_of_structural_o_keep_left_to_right_completion() {
    let program = OIrProgram {
        nodes: vec![OIr::Exec {
            lang: "O".into(),
            env_id: u32::MAX,
            attr: None,
            backend: BackendRegistry::global().interface_for("O"),
            body: vec![inline_exec("html", "left"), inline_exec("text", "right")],
        }],
    };
    let plan = program.plan();
    let graph = build_program(&program);
    let children = plan
        .nodes
        .iter()
        .filter_map(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Exec { lang, .. } if lang == "html" || lang == "text"
            )
            .then_some(node.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);

    let completion = graph.completion_node(children[0]).unwrap();
    assert!(graph
        .op_for(children[1])
        .unwrap()
        .inputs
        .contains(&completion));
    assert!(graph.sequence_dependencies.iter().any(|dependency| {
        dependency.predecessor == children[0]
            && dependency.successor == children[1]
            && dependency.completion == completion
    }));

    let waves = ReadySchedule::derive(&graph).unwrap().waves().unwrap();
    assert!(wave_of(&waves, children[0]) < wave_of(&waves, children[1]));
}

#[test]
fn unknown_siblings_share_a_directed_hostworld_chain() {
    let program = OIrProgram {
        nodes: vec![
            shim_exec(
                "python",
                u32::MAX,
                vec![OIr::Text("__oval_result__ = 1".into())],
            ),
            shim_exec(
                "python",
                u32::MAX,
                vec![OIr::Text("__oval_result__ = 2".into())],
            ),
        ],
    };
    let graph = build_program(&program);
    let infos = graph.exec_ops_ordered();
    assert_eq!(infos.len(), 2);

    let (_, first_version, first_successor, first_next) =
        resource_transition(&graph, &infos[0], &ResourceKey::HostWorld);
    let (second_prior, second_version, _, second_next) =
        resource_transition(&graph, &infos[1], &ResourceKey::HostWorld);
    assert_eq!((first_version, first_next), (0, 1));
    assert_eq!((second_version, second_next), (1, 2));
    assert_eq!(first_successor, second_prior);

    let schedule = ReadySchedule::derive(&graph).expect("schedule derives");
    assert!(schedule.ops[1].blocked_by.contains(&0));
    let waves = schedule.waves().expect("acyclic");
    assert_ne!(
        wave_of(&waves, infos[0].plan_node),
        wave_of(&waves, infos[1].plan_node),
        "unknown siblings must never be concurrently ready"
    );
}

#[test]
fn data_dependencies_force_later_waves() {
    // store x = "hi"; text^( $x )  → the exec depends on the store's value.
    let text = BackendRegistry::global().interface_for("text");
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "x".into(),
                expr: Box::new(OIr::Text("hi".into())),
            },
            OIr::Exec {
                lang: "text".into(),
                env_id: u32::MAX,
                attr: None,
                backend: text,
                body: vec![OIr::Load("x".into())],
            },
        ],
    };
    let graph = build_program(&program);
    let schedule = ReadySchedule::derive(&graph).expect("schedule derives");
    let waves = schedule.waves().expect("acyclic");

    // The Load (and the Exec consuming it) must appear in a later wave than the
    // Store that produces the binding value.
    let plan = program.plan();
    let store_id = plan
        .nodes
        .iter()
        .find(|n| matches!(n.kind, PlanNodeKind::Store { .. }))
        .expect("store node")
        .id;
    let load_id = plan
        .nodes
        .iter()
        .find(|n| matches!(n.kind, PlanNodeKind::Load { .. }))
        .expect("load node")
        .id;
    let store_wave = waves
        .iter()
        .position(|wave| wave.contains(&store_id))
        .expect("store scheduled");
    let load_wave = waves
        .iter()
        .position(|wave| wave.contains(&load_id))
        .expect("load scheduled");
    assert!(
        load_wave > store_wave,
        "load wave {load_wave} must follow store wave {store_wave}; waves = {waves:?}"
    );
}

#[test]
fn reads_of_the_same_resource_share_the_current_writer_frontier() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "shared".into(),
                expr: Box::new(OIr::Text("value".into())),
            },
            OIr::Load("shared".into()),
            OIr::Load("shared".into()),
        ],
    };
    let plan = program.plan();
    let graph = build_program(&program);
    let loads = plan
        .nodes
        .iter()
        .filter_map(|node| matches!(&node.kind, PlanNodeKind::Load { .. }).then_some(node.id))
        .collect::<Vec<_>>();
    assert_eq!(loads.len(), 2);

    let resource = ResourceKey::ScopeBinding("shared".into());
    let first = graph.op_for(loads[0]).expect("first load has an op");
    let second = graph.op_for(loads[1]).expect("second load has an op");
    let (first_state, first_version) = resource_read_state(&graph, first, &resource);
    let (second_state, second_version) = resource_read_state(&graph, second, &resource);

    // The preceding store has already advanced scope:shared from v0 to v1.
    // Both reads consume that immutable writer epoch and emit only their own
    // completion tokens; neither manufactures a resource successor.
    assert_eq!((first_version, second_version), (1, 1));
    assert_eq!(first_state, second_state);

    let schedule = ReadySchedule::derive(&graph).expect("schedule derives");
    let waves = schedule.waves().expect("acyclic");
    assert_eq!(
        wave_of(&waves, loads[0]),
        wave_of(&waves, loads[1]),
        "verified same-resource reads must share a legal ready frontier: {waves:?}"
    );
}

#[test]
fn writer_after_reads_drains_every_reader_and_advances_once() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "shared".into(),
                expr: Box::new(OIr::Text("initial".into())),
            },
            OIr::Load("shared".into()),
            OIr::Load("shared".into()),
            OIr::Store {
                name: "shared".into(),
                expr: Box::new(OIr::Text("next".into())),
            },
            OIr::Load("shared".into()),
        ],
    };
    let plan = program.plan();
    let graph = build_program(&program);
    let loads = plan
        .nodes
        .iter()
        .filter_map(|node| matches!(node.kind, PlanNodeKind::Load { .. }).then_some(node.id))
        .collect::<Vec<_>>();
    let stores = plan
        .nodes
        .iter()
        .filter_map(|node| matches!(node.kind, PlanNodeKind::Store { .. }).then_some(node.id))
        .collect::<Vec<_>>();
    assert_eq!(loads.len(), 3);
    assert_eq!(stores.len(), 2);

    let resource = ResourceKey::ScopeBinding("shared".into());
    let writer = graph.op_for(stores[1]).expect("second store has an op");
    let (prior, version, successor, next) = resource_transition(&graph, writer, &resource);
    assert_eq!((version, next), (1, 2));
    assert_ne!(prior, successor);
    for reader in &loads[..2] {
        let completion = graph.completion_node(*reader).expect("reader completion");
        assert!(
            writer.inputs.contains(&completion),
            "writer omitted reader P{} completion N{}",
            reader.0,
            completion.0
        );
    }
    let later = graph.op_for(loads[2]).expect("later load has an op");
    assert_eq!(
        resource_read_state(&graph, later, &resource),
        (successor, 2)
    );
}

#[test]
fn validation_rejects_a_writer_that_omits_one_open_reader() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "shared".into(),
                expr: Box::new(OIr::Text("initial".into())),
            },
            OIr::Load("shared".into()),
            OIr::Load("shared".into()),
            OIr::Store {
                name: "shared".into(),
                expr: Box::new(OIr::Text("next".into())),
            },
        ],
    };
    let plan = program.plan();
    let mut graph = build_program(&program);
    let loads = plan
        .nodes
        .iter()
        .filter_map(|node| matches!(node.kind, PlanNodeKind::Load { .. }).then_some(node.id))
        .collect::<Vec<_>>();
    let writer = plan
        .nodes
        .iter()
        .filter_map(|node| matches!(node.kind, PlanNodeKind::Store { .. }).then_some(node.id))
        .nth(1)
        .expect("second store");
    let omitted = graph
        .completion_node(loads[0])
        .expect("first reader completion");
    let edge = graph.op_for(writer).expect("writer op").edge;

    graph
        .op_map
        .get_mut(&writer)
        .expect("writer op remains registered")
        .inputs
        .retain(|node| *node != omitted);
    graph
        .exec_edges
        .get_mut(&edge)
        .expect("writer edge exists")
        .ports
        .retain(|port| port.node != omitted);
    let reader_completion = graph
        .nodes
        .get_mut(&omitted)
        .expect("reader completion exists");
    reader_completion
        .consumers
        .retain(|candidate| *candidate != edge);
    reader_completion
        .incident
        .retain(|candidate| *candidate != edge);

    let error = graph
        .validate_execution_source(&program, &plan)
        .expect_err("writer admission must fail when one prior reader is omitted");
    assert!(
        error.contains("omits open reader completion"),
        "unexpected validation error: {error}"
    );
}

#[test]
fn same_actor_operations_serialize() {
    // Two persistent python blocks against the same environment share an actor
    // and must be serialized (the second is blocked by the first).
    let py = BackendRegistry::global().interface_for("python");
    let program = OIrProgram {
        nodes: vec![
            OIr::Exec {
                lang: "python".into(),
                env_id: 0,
                attr: None,
                backend: py.clone(),
                body: vec![OIr::Text("__oval_result__ = 1".into())],
            },
            OIr::Exec {
                lang: "python".into(),
                env_id: 0,
                attr: None,
                backend: py,
                body: vec![OIr::Text("__oval_result__ = 2".into())],
            },
        ],
    };
    let graph = build_program(&program);
    let schedule = ReadySchedule::derive(&graph).expect("schedule derives");

    // The second same-actor operation must list the first among its blockers.
    let ops = &schedule.ops;
    assert_eq!(ops.len(), 2);
    let (first, second) = if ops[0].ordinal < ops[1].ordinal {
        (0, 1)
    } else {
        (1, 0)
    };
    assert!(
        ops[second].blocked_by.contains(&first),
        "same-actor op must be blocked by its predecessor: {:?}",
        ops
    );

    let actor = ResourceKey::ActorState(ActorResourceId::new("python", 0));
    let first_info = graph.op_for(ops[first].plan_node).unwrap();
    let second_info = graph.op_for(ops[second].plan_node).unwrap();
    let (_, first_version, first_successor, first_next) =
        resource_transition(&graph, first_info, &actor);
    let (second_prior, second_version, _, second_next) =
        resource_transition(&graph, second_info, &actor);
    assert_eq!((first_version, first_next), (0, 1));
    assert_eq!((second_version, second_next), (1, 2));
    assert_eq!(first_successor, second_prior);
}

#[test]
fn persistent_backend_alias_uses_canonical_actor_identity() {
    let backend = BackendRegistry::global().interface_for("py");
    assert_eq!(backend.canonical, "python");
    let program = OIrProgram {
        nodes: vec![OIr::Exec {
            lang: "py".into(),
            env_id: 0,
            attr: None,
            backend,
            body: vec![OIr::Text("__oval_result__ = 1".into())],
        }],
    };
    let plan = program.plan();
    let graph = build_program(&program);
    let info = graph.op_for(plan.roots[0]).expect("alias block has an op");
    let edge = graph.exec_edge(info.edge).unwrap();
    assert!(matches!(
        &edge.op,
        HEdgeKind::Execute(ExecutableOp::EvalBackend { lang, env })
            if lang == "python" && *env == 0
    ));

    let actor = ResourceKey::ActorState(ActorResourceId::new("python", 0));
    assert_eq!(resource_transition(&graph, info, &actor).1, 0);
    graph.validate_execution_graph().unwrap();
}

#[test]
fn lexical_sequence_crosses_literal_separators_via_completion() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "left".into(),
                expr: Box::new(OIr::Text("A".into())),
            },
            OIr::Text("\n".into()),
            OIr::Store {
                name: "right".into(),
                expr: Box::new(OIr::Text("B".into())),
            },
        ],
    };
    let plan = program.plan();
    let graph = build_program(&program);
    let stores = plan
        .nodes
        .iter()
        .filter_map(|node| matches!(&node.kind, PlanNodeKind::Store { .. }).then_some(node.id))
        .collect::<Vec<_>>();
    assert_eq!(stores.len(), 2);

    let completion = graph.completion_node(stores[0]).unwrap();
    let successor = graph.op_for(stores[1]).unwrap();
    assert!(successor.inputs.contains(&completion));
    assert!(graph.sequence_dependencies.iter().any(|dependency| {
        dependency.predecessor == stores[0]
            && dependency.successor == stores[1]
            && dependency.completion == completion
    }));

    let schedule = ReadySchedule::derive(&graph).expect("schedule derives");
    let waves = schedule.waves().expect("acyclic");
    assert!(wave_of(&waves, stores[0]) < wave_of(&waves, stores[1]));
}

#[test]
fn explicit_group_members_use_concurrent_control_topology() {
    for mode in [
        GroupMode::Batch,
        GroupMode::All,
        GroupMode::Any,
        GroupMode::Race,
    ] {
        let program = OIrProgram {
            nodes: vec![OIr::Invoke {
                fn_name: mode.name().into(),
                mode: InvokeMode::Group(mode),
                args: vec![
                    OIr::Store {
                        name: format!("{}_left", mode.name()),
                        expr: Box::new(OIr::Text("A".into())),
                    },
                    OIr::Store {
                        name: format!("{}_right", mode.name()),
                        expr: Box::new(OIr::Text("B".into())),
                    },
                ],
            }],
        };
        let plan = program.plan();
        let graph = build_program(&program);
        let stores = plan
            .nodes
            .iter()
            .filter_map(|node| matches!(&node.kind, PlanNodeKind::Store { .. }).then_some(node.id))
            .collect::<Vec<_>>();
        let group = plan
            .nodes
            .iter()
            .find_map(|node| {
                matches!(&node.kind, PlanNodeKind::Group { mode: candidate, .. } if *candidate == mode)
                    .then_some(node.id)
            })
            .expect("group node");
        assert_eq!(stores.len(), 2);
        assert!(
            !graph.sequence_dependencies.iter().any(|dependency| {
                dependency.predecessor == stores[0] && dependency.successor == stores[1]
            }),
            "{} members must not inherit ordinary sibling completion order",
            mode.name()
        );

        let schedule = ReadySchedule::derive(&graph).expect("schedule derives");
        let waves = schedule.waves().expect("acyclic");
        let left_wave = wave_of(&waves, stores[0]);
        let right_wave = wave_of(&waves, stores[1]);
        assert_eq!(
            left_wave,
            right_wave,
            "{} members should become ready together: {waves:?}",
            mode.name()
        );
        assert!(
            wave_of(&waves, group) > left_wave,
            "{} barrier must follow its members",
            mode.name()
        );
    }
}

#[test]
fn nested_unknown_child_orders_outer_then_later_sibling_without_cycle() {
    let nested = shim_exec(
        "python",
        u32::MAX,
        vec![OIr::Text("__oval_result__ = 'C'".into())],
    );
    let program = OIrProgram {
        nodes: vec![
            shim_exec("bash", u32::MAX, vec![nested]),
            shim_exec(
                "ruby",
                u32::MAX,
                vec![OIr::Text("__oval_result__ = 'B'".into())],
            ),
        ],
    };
    let plan = program.plan();
    let nested_id = plan_exec_id(&plan, "python");
    let outer_id = plan_exec_id(&plan, "bash");
    let later_id = plan_exec_id(&plan, "ruby");
    let graph = build_program(&program);

    graph
        .validate_execution_graph()
        .expect("nested state lowering remains acyclic");
    let schedule = ReadySchedule::derive(&graph).expect("schedule derives");
    let waves = schedule.waves().expect("acyclic");
    assert!(
        wave_of(&waves, nested_id) < wave_of(&waves, outer_id),
        "nested child must materialize before its outer operation: {waves:?}"
    );
    assert!(
        wave_of(&waves, outer_id) < wave_of(&waves, later_id),
        "outer completion must precede the later sibling: {waves:?}"
    );
}
