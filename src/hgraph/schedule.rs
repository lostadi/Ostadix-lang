use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ir::PlanNodeId;

use super::{
    graph::{ActorId, EdgeId, HGraph, NodeId, PortRole},
    kinds::OpKind,
};

// ─────────────────────────────────────────────────────────────────────────────
// Ready-operation scheduler
//
// An Execute hyperedge is ready when all of its input value nodes are
// materialized, its blocking constraints (data/structural producers and its
// same-actor predecessor) are satisfied, and its branch guard (if any) is
// active. Every Execute edge carries a stable source ordinal used both for
// tie-breaking when several operations become ready simultaneously and for the
// deterministic commit order. Plain sibling sequencing is NOT a blocking
// constraint here — that is how independent siblings become concurrently ready.
// ─────────────────────────────────────────────────────────────────────────────

/// One schedulable operation: its plan node, its Execute edge, the value nodes
/// it consumes/produces, its stable ordinal, and (for backend blocks) its actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyOp {
    pub plan_node: PlanNodeId,
    pub edge: EdgeId,
    pub output: NodeId,
    pub inputs: Vec<NodeId>,
    pub ordinal: u64,
    pub actor: Option<ActorId>,
    /// Indices (into `ReadySchedule::ops`) of operations that must complete
    /// before this one can run.
    pub blocked_by: Vec<usize>,
}

/// The derived ready-operation schedule for a graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadySchedule {
    pub ops: Vec<ReadyOp>,
}

impl ReadySchedule {
    /// Derive the ready-operation schedule from the graph's Execute hyperedges.
    pub fn derive(graph: &HGraph) -> Result<ReadySchedule, String> {
        let infos = graph.exec_ops_ordered();

        // producer value node → op index (an op's output node)
        let mut producer_op: HashMap<NodeId, usize> = HashMap::new();
        for (index, info) in infos.iter().enumerate() {
            producer_op.insert(info.output, index);
        }

        let mut ops: Vec<ReadyOp> = infos
            .iter()
            .map(|info| ReadyOp {
                plan_node: info.plan_node,
                edge: info.edge,
                output: info.output,
                inputs: info.inputs.clone(),
                ordinal: info.ordinal,
                actor: graph.node(info.output).and_then(|node| node.actor),
                blocked_by: Vec::new(),
            })
            .collect();

        // Data/structural blocking: an op waits on the producers of its inputs.
        for index in 0..ops.len() {
            let mut deps: BTreeSet<usize> = BTreeSet::new();
            for input in ops[index].inputs.clone() {
                if let Some(&producer) = producer_op.get(&input) {
                    if producer != index {
                        deps.insert(producer);
                    }
                }
            }
            ops[index].blocked_by = deps.into_iter().collect();
        }

        // Actor serialization: same-actor ops run in stable ordinal order.
        let mut by_actor: HashMap<ActorId, Vec<usize>> = HashMap::new();
        for (index, op) in ops.iter().enumerate() {
            if let Some(actor) = op.actor {
                by_actor.entry(actor).or_default().push(index);
            }
        }
        for members in by_actor.values() {
            let mut ordered = members.clone();
            ordered.sort_by_key(|&i| (ops[i].ordinal, i));
            for window in ordered.windows(2) {
                let (prev, next) = (window[0], window[1]);
                if !ops[next].blocked_by.contains(&prev) {
                    ops[next].blocked_by.push(prev);
                }
            }
        }
        for op in &mut ops {
            op.blocked_by.sort_unstable();
            op.blocked_by.dedup();
        }

        Ok(ReadySchedule { ops })
    }

    /// Topological "waves": each wave is the set of operations that become
    /// ready at the same step (in stable ordinal order). Independent siblings
    /// share a wave; same-actor and data-dependent operations land in later
    /// waves. Returns an error if the operation dependency graph has a cycle.
    pub fn waves(&self) -> Result<Vec<Vec<PlanNodeId>>, String> {
        let mut indegree = vec![0usize; self.ops.len()];
        let mut successors: Vec<Vec<usize>> = vec![Vec::new(); self.ops.len()];
        for (index, op) in self.ops.iter().enumerate() {
            for &dep in &op.blocked_by {
                indegree[index] += 1;
                successors[dep].push(index);
            }
        }

        let order_key = |i: usize| (self.ops[i].ordinal, i);
        let mut ready: Vec<usize> = (0..self.ops.len()).filter(|&i| indegree[i] == 0).collect();
        ready.sort_by_key(|&i| order_key(i));

        let mut waves: Vec<Vec<PlanNodeId>> = Vec::new();
        let mut scheduled = 0usize;
        while !ready.is_empty() {
            let wave = std::mem::take(&mut ready);
            waves.push(wave.iter().map(|&i| self.ops[i].plan_node).collect());
            scheduled += wave.len();
            let mut next: Vec<usize> = Vec::new();
            for node in wave {
                for &successor in &successors[node] {
                    indegree[successor] -= 1;
                    if indegree[successor] == 0 {
                        next.push(successor);
                    }
                }
            }
            next.sort_by_key(|&i| order_key(i));
            ready = next;
        }

        if scheduled != self.ops.len() {
            return Err(format!(
                "ready-operation graph contains a cycle: scheduled {scheduled} of {} operations",
                self.ops.len()
            ));
        }
        Ok(waves)
    }

    /// The plan nodes in a deterministic, dependency-respecting order (stable
    /// ordinal breaks ties). This is the order the coordinator uses to launch
    /// operations onto the ready frontier.
    pub fn launch_order(&self) -> Result<Vec<PlanNodeId>, String> {
        Ok(self.waves()?.into_iter().flatten().collect())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Legacy node-clustering schedule (compatibility surface)
//
// The reference (serial) executor and a number of analysis tests still consume
// the node-level clustering schedule. It is retained here as a compatibility
// API over the constraint/type edge set.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionCluster {
    pub nodes: Vec<NodeId>,
    pub can_parallelize: bool,
    pub actor: Option<ActorId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub clusters: Vec<ExecutionCluster>,
}

impl Schedule {
    pub fn root_order(&self, graph: &HGraph) -> Result<Vec<usize>, String> {
        let root_positions = graph
            .root_nodes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, node)| (node, index))
            .collect::<HashMap<_, _>>();
        let mut order = Vec::with_capacity(graph.root_nodes.len());
        for cluster in &self.clusters {
            for node in &cluster.nodes {
                if let Some(index) = root_positions.get(node) {
                    order.push(*index);
                }
            }
        }
        if order.len() != graph.root_nodes.len() {
            return Err(format!(
                "hypergraph schedule covered {} of {} root nodes",
                order.len(),
                graph.root_nodes.len()
            ));
        }
        Ok(order)
    }
}

pub fn schedule(graph: &HGraph) -> Schedule {
    try_schedule(graph).expect("invalid hypergraph schedule")
}

pub fn try_schedule(graph: &HGraph) -> Result<Schedule, String> {
    let mut precedes: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();
    let mut actor_members: HashMap<ActorId, Vec<NodeId>> = HashMap::new();

    for edge_id in graph.edge_ids() {
        let Some(edge) = graph.edge(edge_id) else {
            continue;
        };
        match &edge.kind {
            OpKind::DataFlow
            | OpKind::StructuralBarrier
            | OpKind::Sequence
            | OpKind::Batch
            | OpKind::All
            | OpKind::Any
            | OpKind::Race
            | OpKind::Request { .. }
            | OpKind::Schedule { .. }
            | OpKind::CacheMemo { .. } => {
                let inputs: Vec<_> = edge
                    .ports
                    .iter()
                    .filter(|p| p.role == PortRole::Input)
                    .map(|p| p.node)
                    .collect();
                let outputs: Vec<_> = edge
                    .ports
                    .iter()
                    .filter(|p| p.role == PortRole::Output)
                    .map(|p| p.node)
                    .collect();
                for input in &inputs {
                    for output in &outputs {
                        precedes.entry(*input).or_default().insert(*output);
                    }
                }
            }
            OpKind::ActorSerial { actor } => {
                for port in &edge.ports {
                    actor_members.entry(*actor).or_default().push(port.node);
                }
            }
            _ => {}
        }
    }

    for members in actor_members.values_mut() {
        members.dedup();
        for window in members.windows(2) {
            precedes.entry(window[0]).or_default().insert(window[1]);
        }
    }

    topological_clusters(graph, &precedes)
}

fn topological_clusters(
    graph: &HGraph,
    precedes: &HashMap<NodeId, HashSet<NodeId>>,
) -> Result<Schedule, String> {
    let mut indegree: HashMap<NodeId, usize> =
        graph.node_ids().into_iter().map(|id| (id, 0)).collect();
    let mut successors: HashMap<NodeId, BTreeSet<NodeId>> = HashMap::new();

    for (from, tos) in precedes {
        for to in tos {
            successors.entry(*from).or_default().insert(*to);
            *indegree.entry(*to).or_insert(0) += 1;
        }
    }

    let mut ready: BTreeSet<NodeId> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect();
    let mut clusters = Vec::new();

    while !ready.is_empty() {
        let batch = ready.iter().copied().collect::<Vec<_>>();
        ready.clear();
        clusters.push(ExecutionCluster {
            can_parallelize: batch.len() > 1,
            actor: None,
            nodes: batch.clone(),
        });

        for node in batch {
            if let Some(succs) = successors.get(&node) {
                for successor in succs {
                    let degree = indegree
                        .get_mut(successor)
                        .expect("successor should be known to the graph");
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(*successor);
                    }
                }
            }
        }
    }

    let scheduled: usize = clusters.iter().map(|cluster| cluster.nodes.len()).sum();
    if scheduled != graph.nodes.len() {
        return Err(format!(
            "hypergraph dependency graph contains a cycle or invalid dependency: scheduled {scheduled} of {} nodes",
            graph.nodes.len()
        ));
    }

    Ok(Schedule { clusters })
}
