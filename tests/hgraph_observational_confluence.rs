//! Finite model checks of the congruence/commutation scheduling theorem.
use o_lang::hgraph::semantics::{check_finite_confluence, FiniteTransitionModel};
use o_lang::hgraph::{HGraph, ReadySchedule};
use o_lang::ir::{BackendRegistry, OIr, OIrProgram};
use std::collections::BTreeMap;

fn graph(language: &str, count: usize) -> HGraph {
    OIrProgram {
        nodes: (0..count)
            .map(|i| OIr::Exec {
                lang: language.into(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for(language),
                body: vec![OIr::Text(format!("{i}"))],
            })
            .collect(),
    }
    .hgraph()
}

fn model(
    graph: &HGraph,
    tables: Vec<Vec<usize>>,
    classes: Vec<u64>,
    observations: Vec<u64>,
) -> FiniteTransitionModel {
    let ops = ReadySchedule::derive(graph).unwrap().ops;
    assert_eq!(ops.len(), tables.len());
    FiniteTransitionModel {
        transitions: ops
            .into_iter()
            .zip(tables)
            .map(|(op, table)| (op.plan_node, table))
            .collect::<BTreeMap<_, _>>(),
        equivalence_classes: classes,
        observations,
    }
}

#[test]
fn every_legal_topological_order_has_the_same_model_observation() {
    let graph = graph("text", 3);
    let model = model(
        &graph,
        vec![vec![1, 2, 3, 0], vec![2, 3, 0, 1], vec![3, 0, 1, 2]],
        vec![0, 1, 2, 3],
        vec![0, 1, 2, 3],
    );
    let checked = check_finite_confluence(&graph, &model).unwrap();
    assert_eq!(checked.incomparable_pairs_checked, 3);
    let schedule = ReadySchedule::derive(&graph).unwrap();
    fn orders(schedule: &ReadySchedule, prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if prefix.len() == schedule.ops.len() {
            out.push(prefix.clone());
            return;
        }
        for (i, op) in schedule.ops.iter().enumerate() {
            if !prefix.contains(&i) && op.blocked_by.iter().all(|p| prefix.contains(p)) {
                prefix.push(i);
                orders(schedule, prefix, out);
                prefix.pop();
            }
        }
    }
    let mut all = Vec::new();
    orders(&schedule, &mut Vec::new(), &mut all);
    assert_eq!(all.len(), 6);
    for initial in 0..4 {
        let observations = all
            .iter()
            .map(|order| {
                let final_state = order.iter().fold(initial, |s, &i| {
                    model.transitions[&schedule.ops[i].plan_node][s]
                });
                model.observations[final_state]
            })
            .collect::<Vec<_>>();
        assert!(observations.iter().all(|o| *o == observations[0]));
    }
}

#[test]
fn rejects_a_relation_that_a_later_operation_can_distinguish() {
    let graph = graph("text", 2);
    let model = model(
        &graph,
        vec![vec![0, 1, 2], vec![0, 2, 2]],
        vec![0, 0, 1],
        vec![0, 0, 1],
    );
    assert!(check_finite_confluence(&graph, &model)
        .unwrap_err()
        .contains("does not preserve equivalence"));
}

#[test]
fn noncommuting_write_and_failure_require_an_order() {
    // States: untouched, written, failed untouched, failed after write.
    // Failure is absorbing for a later write. Swapping write/fail is visible.
    let tables = vec![vec![1, 1, 2, 3], vec![2, 3, 2, 3]];
    let unordered = graph("text", 2);
    let spec = model(
        &unordered,
        tables.clone(),
        vec![0, 1, 2, 3],
        vec![0, 1, 2, 3],
    );
    assert!(check_finite_confluence(&unordered, &spec)
        .unwrap_err()
        .contains("do not commute"));
    let ordered = graph("python", 2);
    let spec = model(&ordered, tables, vec![0, 1, 2, 3], vec![0, 1, 2, 3]);
    assert_eq!(
        check_finite_confluence(&ordered, &spec)
            .unwrap()
            .incomparable_pairs_checked,
        0
    );
}

#[test]
fn identical_indistinguishability_does_not_imply_identical_behavior() {
    let graph = graph("text", 1);
    let identity = model(&graph, vec![vec![0, 1, 2]], vec![0, 1, 2], vec![0, 1, 2]);
    let constant = model(&graph, vec![vec![0, 0, 0]], vec![0, 1, 2], vec![0, 1, 2]);
    check_finite_confluence(&graph, &identity).unwrap();
    check_finite_confluence(&graph, &constant).unwrap();
    let op = ReadySchedule::derive(&graph).unwrap().ops[0].plan_node;
    assert_ne!(
        identity.observations[identity.transitions[&op][1]],
        constant.observations[constant.transitions[&op][1]]
    );
}
