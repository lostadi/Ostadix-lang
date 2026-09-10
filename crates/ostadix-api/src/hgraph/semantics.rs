//! Exhaustive confluence checks for an explicitly supplied finite transition model.
//!
//! The model is a specification, not extracted backend semantics. Successful
//! checking cannot mint execution admission or prove an adapter implements the
//! supplied transitions. Divergence, interleaving within an operation, and
//! contextual equivalence require separate refinements.
use std::collections::BTreeMap;

use super::{HGraph, ReadyInputPolicy, ReadySchedule};
use crate::ir::PlanNodeId;

/// A deterministic total transition table for every executable operation.
/// State indices are 0..observations.len(); class labels define equivalence R.
/// Failure/blocking, when modeled, must be explicit states with appropriate
/// transitions for subsequent operations.
#[derive(Clone, Debug)]
pub struct FiniteTransitionModel {
    pub observations: Vec<u64>,
    pub equivalence_classes: Vec<u64>,
    pub transitions: BTreeMap<PlanNodeId, Vec<usize>>,
}

/// Descriptive results of exhaustive checks over a supplied finite model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiniteConfluenceCheck {
    pub states: usize,
    pub operations: usize,
    pub incomparable_pairs_checked: usize,
}

/// Verify the hypotheses of topological-order confluence modulo R against the
/// actual executable HGraph blockers. All unordered pairs must commute modulo
/// R, every operation must preserve R, and observation must factor through R.
///
/// Any two complete DAG orders are connected by adjacent incomparable swaps.
/// Commutation relates the states after each swap; congruence transports that
/// relation through the remaining suffix; observation soundness closes it.
/// This checks all model states, a stronger condition than reachable states.
pub fn check_finite_confluence(
    graph: &HGraph,
    model: &FiniteTransitionModel,
) -> Result<FiniteConfluenceCheck, String> {
    const MAX_STATES: usize = 256;
    const MAX_OPERATIONS: usize = 256;
    let schedule = ReadySchedule::derive(graph)?;
    schedule.waves()?;
    let n = model.observations.len();
    let m = schedule.ops.len();
    if n == 0 || n > MAX_STATES || m > MAX_OPERATIONS {
        return Err(
            "finite confluence check requires 1..=256 states and at most 256 operations".into(),
        );
    }
    if model.equivalence_classes.len() != n || model.transitions.len() != m {
        return Err(
            "model must describe every state and exactly every executable operation".into(),
        );
    }
    let mut tables = Vec::with_capacity(m);
    for op in &schedule.ops {
        if op.input_policy(graph)? != ReadyInputPolicy::All {
            return Err(
                "finite DAG theorem requires conjunctive inputs and complete execution".into(),
            );
        }
        let table = model
            .transitions
            .get(&op.plan_node)
            .ok_or_else(|| format!("missing transition for P{}", op.plan_node.0))?;
        if table.len() != n || table.iter().any(|state| *state >= n) {
            return Err(format!(
                "P{} is not a total transition on the model carrier",
                op.plan_node.0
            ));
        }
        tables.push(table);
    }
    let class = &model.equivalence_classes;
    for x in 0..n {
        for y in 0..x {
            if class[x] != class[y] {
                continue;
            }
            if model.observations[x] != model.observations[y] {
                return Err(format!(
                    "equivalent states {x} and {y} have different observations"
                ));
            }
            for (i, table) in tables.iter().enumerate() {
                if class[table[x]] != class[table[y]] {
                    return Err(format!(
                        "P{} does not preserve equivalence at states {x}, {y}",
                        schedule.ops[i].plan_node.0
                    ));
                }
            }
        }
    }
    let mut precedes = vec![vec![false; m]; m];
    for (i, op) in schedule.ops.iter().enumerate() {
        for &p in &op.blocked_by {
            precedes[p][i] = true;
        }
    }
    for k in 0..m {
        for i in 0..m {
            for j in 0..m {
                precedes[i][j] |= precedes[i][k] && precedes[k][j];
            }
        }
    }
    let mut pairs = 0;
    for u in 0..m {
        for v in 0..u {
            if precedes[u][v] || precedes[v][u] {
                continue;
            }
            pairs += 1;
            for w in 0..n {
                if class[tables[u][tables[v][w]]] != class[tables[v][tables[u][w]]] {
                    return Err(format!(
                        "incomparable P{} and P{} do not commute modulo equivalence at state {w}",
                        schedule.ops[u].plan_node.0, schedule.ops[v].plan_node.0
                    ));
                }
            }
        }
    }
    Ok(FiniteConfluenceCheck {
        states: n,
        operations: m,
        incomparable_pairs_checked: pairs,
    })
}
