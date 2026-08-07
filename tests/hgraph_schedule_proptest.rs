//! Property coverage for state-complete executable HGraph scheduling.
//!
//! Generated programs mix ordinary value dependencies, lexical sequence (with
//! optional literal separators), precise resource declarations, persistent
//! actor identities, conservative hosted work, and verified-pure inline work.
//! The assertions are graph-generic: readiness must be exactly the producer set
//! of an operation's inputs, conflicts must have a directed order, and the
//! deliberately independent pure pair must retain concurrency.

use std::collections::{BTreeSet, HashMap, HashSet};

use o_lang::effects::ResourceKey;
use o_lang::hgraph::from_oir::build_program;
use o_lang::hgraph::{schedule::ReadySchedule, HGraph, HNodeKind, NodeId};
use o_lang::ir::{BackendRegistry, OIr, OIrProgram, PlanNodeId, PlanNodeKind};
use proptest::prelude::*;

#[derive(Clone, Debug)]
struct GeneratedOp {
    class: u8,
    resource_class: u8,
    identity: u8,
    separator_before: bool,
}

fn inline_exec(lang: &str, body: impl Into<String>) -> OIr {
    OIr::Exec {
        lang: lang.into(),
        env_id: u32::MAX,
        attr: None,
        backend: BackendRegistry::global().interface_for(lang),
        body: vec![OIr::Text(body.into())],
    }
}

fn resource_attribute(resource_class: u8, identity: u8) -> Option<String> {
    match resource_class % 5 {
        0 => None,
        1 => Some(format!("reads=host:/tmp/olang-hgraph-{identity}")),
        2 => Some(format!("writes=env:OLANG_PROP_{identity}")),
        3 => Some(format!("writes=scope:resource_{identity}")),
        _ => Some(format!("reads=service:service-{identity}")),
    }
}

fn hosted_exec(env_id: u32, attr: Option<String>, marker: impl Into<String>) -> OIr {
    OIr::Exec {
        lang: "python".into(),
        env_id,
        attr,
        backend: BackendRegistry::global().interface_for("python"),
        body: vec![OIr::Text(marker.into())],
    }
}

fn generated_program(
    separator_between_pure: bool,
    with_value_dependency: bool,
    specs: &[GeneratedOp],
) -> OIrProgram {
    // Keep an independently provable pair first. Later lexical operations may
    // depend on these, but neither member receives an incoming dependency.
    let mut nodes = vec![inline_exec("html", "pure-left")];
    if separator_between_pure {
        nodes.push(OIr::Text("\n".into()));
    }
    nodes.push(inline_exec("markdown", "pure-right"));

    if with_value_dependency {
        nodes.push(OIr::Store {
            name: "value_dep".into(),
            expr: Box::new(OIr::Text("payload".into())),
        });
        nodes.push(OIr::Exec {
            lang: "text".into(),
            env_id: u32::MAX,
            attr: None,
            backend: BackendRegistry::global().interface_for("text"),
            body: vec![OIr::Load("value_dep".into())],
        });
    }

    for (index, spec) in specs.iter().enumerate() {
        if spec.separator_before {
            nodes.push(OIr::Text(format!("\n{index}\n")));
        }
        let node = match spec.class % 4 {
            // Another verified-pure operation.
            0 => inline_exec("text", format!("pure-{index}")),
            // Unknown ephemeral hosted work with a varying declared footprint.
            1 => hosted_exec(
                u32::MAX,
                resource_attribute(spec.resource_class, spec.identity),
                format!("__oval_result__ = {index}"),
            ),
            // Persistent hosted work. Environment id is the actor identity.
            2 => hosted_exec(
                u32::from(spec.identity % 3),
                resource_attribute(spec.resource_class, spec.identity),
                format!("__oval_result__ = {index}"),
            ),
            // A precise O-level binding-state transition.
            _ => OIr::Store {
                name: format!("slot_{}", spec.identity % 3),
                expr: Box::new(OIr::Text(format!("stored-{index}"))),
            },
        };
        nodes.push(node);
    }

    OIrProgram { nodes }
}

fn plan_exec_id(program: &OIrProgram, lang: &str) -> PlanNodeId {
    program
        .plan()
        .nodes
        .into_iter()
        .find_map(|node| match node.kind {
            PlanNodeKind::Exec {
                lang: candidate, ..
            } if candidate == lang => Some(node.id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing generated {lang} operation"))
}

fn wave_map(waves: &[Vec<PlanNodeId>]) -> HashMap<PlanNodeId, usize> {
    waves
        .iter()
        .enumerate()
        .flat_map(|(wave, nodes)| nodes.iter().copied().map(move |node| (node, wave)))
        .collect()
}

fn transitively_depends_on(schedule: &ReadySchedule, node: usize, ancestor: usize) -> bool {
    let mut stack = schedule.ops[node].blocked_by.clone();
    let mut visited = HashSet::new();
    while let Some(candidate) = stack.pop() {
        if candidate == ancestor {
            return true;
        }
        if visited.insert(candidate) {
            stack.extend(schedule.ops[candidate].blocked_by.iter().copied());
        }
    }
    false
}

fn resource_versions(graph: &HGraph, nodes: &[NodeId], resource: &ResourceKey) -> Vec<u64> {
    nodes
        .iter()
        .filter_map(|node| match &graph.node(*node)?.kind {
            HNodeKind::ResourceState {
                resource: candidate,
                version,
            } if candidate == resource => Some(*version),
            _ => None,
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_programs_are_dependency_complete_and_conservative(
        separator_between_pure in any::<bool>(),
        with_value_dependency in any::<bool>(),
        raw_specs in prop::collection::vec((0u8..4, 0u8..5, 0u8..8, any::<bool>()), 0..7),
    ) {
        let specs = raw_specs
            .iter()
            .map(|(class, resource_class, identity, separator_before)| GeneratedOp {
                class: *class,
                resource_class: *resource_class,
                identity: *identity,
                separator_before: *separator_before,
            })
            .collect::<Vec<_>>();
        let program = generated_program(separator_between_pure, with_value_dependency, &specs);
        let plan = program.plan();
        let graph = build_program(&program);

        prop_assert!(
            graph.validate_execution_graph().is_ok(),
            "generated execution graph failed validation: specs={specs:?}"
        );
        let schedule = ReadySchedule::derive(&graph).expect("validated graph schedules");
        let waves = schedule.waves().expect("validated graph is acyclic");
        let plan_to_index = schedule
            .ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.plan_node, index))
            .collect::<HashMap<_, _>>();
        let edge_to_index = schedule
            .ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.edge, index))
            .collect::<HashMap<_, _>>();
        let plan_to_wave = wave_map(&waves);

        // ReadySchedule must contain exactly the producers of graph inputs, no
        // actor/effect side table and no omitted value/control dependency.
        for (index, op) in schedule.ops.iter().enumerate() {
            let expected = op
                .inputs
                .iter()
                .filter_map(|input| graph.node(*input).and_then(|node| node.producer))
                .filter_map(|edge| edge_to_index.get(&edge).copied())
                .filter(|producer| *producer != index)
                .collect::<BTreeSet<_>>();
            let actual = op.blocked_by.iter().copied().collect::<BTreeSet<_>>();
            prop_assert_eq!(
                &actual,
                &expected,
                "blockers are not exactly input producers for plan node {}",
                op.plan_node.0
            );
            for producer in expected {
                prop_assert!(
                    plan_to_wave[&schedule.ops[producer].plan_node] < plan_to_wave[&op.plan_node],
                    "plan node {} ran no later than input producer {}",
                    op.plan_node.0,
                    schedule.ops[producer].plan_node.0
                );
            }
        }

        // Every recorded lexical dependency is an actual completion input and
        // therefore an ordinary producer-derived blocker.
        for dependency in &graph.sequence_dependencies {
            let predecessor = plan_to_index[&dependency.predecessor];
            let successor = plan_to_index[&dependency.successor];
            prop_assert_eq!(
                graph.completion_node(dependency.predecessor),
                Some(dependency.completion)
            );
            prop_assert!(graph.op_for(dependency.successor).unwrap().inputs.contains(&dependency.completion));
            prop_assert!(schedule.ops[successor].blocked_by.contains(&predecessor));
        }

        // Reads lease the current writer epoch without advancing it; writes
        // alone publish the next resource version.
        for op in &schedule.ops {
            let summary = graph.effect_summary(op.plan_node).expect("effect summary");
            let (reads, writes) = summary.scheduling_accesses();
            for resource in reads.union(&writes) {
                let inputs = resource_versions(&graph, &op.inputs, &resource);
                let outputs = resource_versions(&graph, &op.outputs, &resource);
                prop_assert_eq!(inputs.len(), 1, "missing {:?} input", resource);
                if writes.contains(resource) {
                    prop_assert_eq!(outputs.len(), 1, "missing {:?} write output", resource);
                    prop_assert_eq!(outputs[0], inputs[0] + 1);
                } else {
                    prop_assert!(outputs.is_empty(), "read advanced {:?}", resource);
                }
            }
        }

        // Semantic conflicts must have a directed dependency path, so they can
        // never be simultaneously ready even if their lexical depths differ.
        for left in 0..schedule.ops.len() {
            for right in (left + 1)..schedule.ops.len() {
                let left_summary = graph.effect_summary(schedule.ops[left].plan_node).unwrap();
                let right_summary = graph.effect_summary(schedule.ops[right].plan_node).unwrap();
                if left_summary.conflicts_with(right_summary) {
                    prop_assert!(
                        transitively_depends_on(&schedule, left, right)
                            || transitively_depends_on(&schedule, right, left),
                        "conflicting operations {} and {} have no directed order",
                        schedule.ops[left].plan_node.0,
                        schedule.ops[right].plan_node.0
                    );
                    prop_assert_ne!(
                        plan_to_wave[&schedule.ops[left].plan_node],
                        plan_to_wave[&schedule.ops[right].plan_node],
                        "conflicting operations became ready in the same wave"
                    );
                }
            }
        }

        // The first two roots are verified-pure, infallible inline operations.
        // Their lexical edge (possibly bridged by a literal) must be relaxed.
        let html = plan
            .nodes
            .iter()
            .find_map(|node| matches!(&node.kind, PlanNodeKind::Exec { lang, .. } if lang == "html").then_some(node.id))
            .expect("html pure op");
        let markdown = plan_exec_id(&program, "markdown");
        let html_index = plan_to_index[&html];
        let markdown_index = plan_to_index[&markdown];
        prop_assert!(!transitively_depends_on(&schedule, html_index, markdown_index));
        prop_assert!(!transitively_depends_on(&schedule, markdown_index, html_index));
        prop_assert_eq!(plan_to_wave[&html], plan_to_wave[&markdown]);

        if with_value_dependency {
            let store = plan
                .nodes
                .iter()
                .find_map(|node| matches!(&node.kind, PlanNodeKind::Store { name } if name == "value_dep").then_some(node.id))
                .expect("generated store");
            let load = plan
                .nodes
                .iter()
                .find_map(|node| matches!(&node.kind, PlanNodeKind::Load { name } if name == "value_dep").then_some(node.id))
                .expect("generated load");
            let store_info = graph.op_for(store).unwrap();
            let load_info = graph.op_for(load).unwrap();
            prop_assert!(load_info.inputs.contains(&store_info.value_output));
            prop_assert!(schedule.ops[plan_to_index[&load]].blocked_by.contains(&plan_to_index[&store]));
        }
    }
}
