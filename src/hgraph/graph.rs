use std::collections::HashMap;

use crate::value::{Fidelity, OValue};

use super::kinds::{DomainFlags, OpKind, RepFlags};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EdgeId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ActorId {
    pub lang: u32,
    pub env: u32,
}

#[derive(Clone, Debug)]
pub struct HNode {
    pub id: NodeId,
    pub domain: DomainFlags,
    pub rep: RepFlags,
    pub value: Option<OValue>,
    pub actor: Option<ActorId>,
    pub fidelity: Option<Fidelity>,
    pub incident: Vec<EdgeId>,
}

impl HNode {
    pub fn fresh() -> Self {
        Self {
            id: NodeId(0),
            domain: DomainFlags::ANY,
            rep: RepFlags::ANY,
            value: None,
            actor: None,
            fidelity: None,
            incident: Vec::new(),
        }
    }

    pub fn with_value(value: OValue) -> Self {
        let mut node = Self::fresh();
        node.value = Some(value);
        node
    }
}

#[derive(Clone, Debug)]
pub struct Port {
    pub node: NodeId,
    pub role: PortRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortRole {
    Input,
    Output,
    InOut,
}

#[derive(Clone, Debug)]
pub struct HEdge {
    pub id: EdgeId,
    pub kind: OpKind,
    pub ports: Vec<Port>,
}

#[derive(Default, Debug)]
pub struct HGraph {
    pub nodes: HashMap<NodeId, HNode>,
    pub edges: HashMap<EdgeId, HEdge>,
    pub bindings: HashMap<String, NodeId>,
    next_node: u64,
    next_edge: u64,
}

impl HGraph {
    pub fn add_node(&mut self, mut node: HNode) -> NodeId {
        let id = NodeId(self.next_node);
        self.next_node += 1;
        node.id = id;
        self.nodes.insert(id, node);
        id
    }

    pub fn add_edge(&mut self, mut edge: HEdge) -> EdgeId {
        let id = EdgeId(self.next_edge);
        self.next_edge += 1;
        edge.id = id;
        for port in &edge.ports {
            if let Some(node) = self.nodes.get_mut(&port.node) {
                node.incident.push(id);
            }
        }
        self.edges.insert(id, edge);
        id
    }

    pub fn bind(&mut self, name: String, node: NodeId) {
        self.bindings.insert(name, node);
    }

    pub fn lookup(&self, name: &str) -> Option<NodeId> {
        self.bindings.get(name).copied()
    }

    pub fn node(&self, id: NodeId) -> Option<&HNode> {
        self.nodes.get(&id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut HNode> {
        self.nodes.get_mut(&id)
    }

    pub fn edge(&self, id: EdgeId) -> Option<&HEdge> {
        self.edges.get(&id)
    }

    pub fn node_ids(&self) -> Vec<NodeId> {
        let mut ids = self.nodes.keys().copied().collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn edge_ids(&self) -> Vec<EdgeId> {
        let mut ids = self.edges.keys().copied().collect::<Vec<_>>();
        ids.sort();
        ids
    }
}
