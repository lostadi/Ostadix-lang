use std::collections::{BTreeSet, HashMap, HashSet};

use super::{
    graph::{ActorId, HGraph, NodeId, PortRole},
    kinds::OpKind,
};

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
            | OpKind::Race => {
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
