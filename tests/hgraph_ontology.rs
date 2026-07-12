//! Ontology and ready-operation scheduler tests for the HGraph executor.
//!
//! These assert the value-node / operation-hyperedge projection: every
//! non-literal OIR operation lowers to exactly one Execute hyperedge with the
//! right inputs/output, literal text stays a materialized value node, and the
//! ready-operation scheduler exposes concurrency for independent siblings while
//! serializing same-actor work and respecting data dependencies.

use o_lang::hgraph::from_oir::build_program;
use o_lang::hgraph::{schedule::ReadySchedule, ExecutableOp, HEdgeKind};
use o_lang::ir::{BackendRegistry, InvokeMode, OIr, OIrProgram, PlanNodeKind};
use o_lang::value::GroupMode;

fn html_backend() -> o_lang::ir::BackendInterface {
    BackendRegistry::global().interface_for("html")
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
        !store_op.inputs.contains(&store_op.output),
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
}
