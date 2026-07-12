use std::collections::HashMap;

use crate::ir::{OIr, PlanNodeId};
use crate::value::{Fidelity, OValue};

use super::kinds::{
    ConstraintOp, DomainFlags, ExecutableOp, HEdgeKind, OpKind, RepFlags, ValueState,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EdgeId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ActorId {
    pub lang: u32,
    pub env: u32,
}

/// A value node. Type/fidelity facts live here; operations are hyperedges over
/// value nodes. `state` tracks materialization for the graph executor.
#[derive(Clone, Debug)]
pub struct HNode {
    pub id: NodeId,
    pub domain: DomainFlags,
    pub rep: RepFlags,
    pub value: Option<OValue>,
    pub actor: Option<ActorId>,
    pub fidelity: Option<Fidelity>,
    pub incident: Vec<EdgeId>,
    /// Materialization state driven by the graph executor.
    pub state: ValueState,
    /// The Execute hyperedge that produces this value node, if any.
    pub producer: Option<EdgeId>,
    /// The Execute hyperedges that consume this value node.
    pub consumers: Vec<EdgeId>,
    /// Originating plan node identity (provenance).
    pub plan_node: Option<PlanNodeId>,
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
            state: ValueState::Unresolved,
            producer: None,
            consumers: Vec::new(),
            plan_node: None,
        }
    }

    /// A literal value node. Literal text nodes start `Materialized`.
    pub fn with_value(value: OValue) -> Self {
        let mut node = Self::fresh();
        node.value = Some(value);
        node.state = ValueState::Materialized;
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

/// A hyperedge. Its ontology classification is `op`; the legacy `kind` field
/// carries the typed/fidelity `OpKind` used by the type solver and the DOT
/// exporter (which is why constraint/type edges keep their historical shape).
#[derive(Clone, Debug)]
pub struct HEdge {
    pub id: EdgeId,
    pub kind: OpKind,
    pub op: HEdgeKind,
    pub ports: Vec<Port>,
}

impl HEdge {
    /// Build a constraint/type hyperedge from a legacy `OpKind` relation. The
    /// ontology classification is derived automatically.
    pub fn constraint(kind: OpKind, ports: Vec<Port>) -> Self {
        let op = HEdgeKind::Constraint(ConstraintOp::from_op_kind(&kind));
        Self {
            id: EdgeId(0),
            kind,
            op,
            ports,
        }
    }

    /// Build an executable operation hyperedge.
    pub fn execute(op: ExecutableOp, ports: Vec<Port>) -> Self {
        Self {
            id: EdgeId(0),
            // Legacy `kind` is only meaningful for constraint/type edges; for
            // Execute edges we use a neutral relation so the (separately stored)
            // executable edges never perturb the type solver if inspected.
            kind: OpKind::Sequence,
            op: HEdgeKind::Execute(op),
            ports,
        }
    }

    pub fn is_execute(&self) -> bool {
        matches!(self.op, HEdgeKind::Execute(_))
    }
}

/// The resolved handle to an operation hyperedge for a plan node: the Execute
/// `EdgeId`, its produced value node, its consumed value nodes, and its stable
/// source ordinal.
#[derive(Clone, Debug)]
pub struct ExecInfo {
    pub edge: EdgeId,
    pub output: NodeId,
    pub inputs: Vec<NodeId>,
    pub ordinal: u64,
    pub plan_node: PlanNodeId,
}

#[derive(Default, Debug)]
pub struct HGraph {
    pub nodes: HashMap<NodeId, HNode>,
    /// Constraint/type hyperedges. Traversed by the type solver and the DOT
    /// exporter.
    pub edges: HashMap<EdgeId, HEdge>,
    /// Executable operation hyperedges, keyed by their own `EdgeId`.
    pub exec_edges: HashMap<EdgeId, HEdge>,
    /// PlanNodeId → executable-operation handle.
    pub op_map: HashMap<PlanNodeId, ExecInfo>,
    pub bindings: HashMap<String, NodeId>,
    pub ir_map: HashMap<NodeId, OIr>,
    pub root_nodes: Vec<NodeId>,
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

    /// Register an executable operation hyperedge for `plan_node`, wiring the
    /// producer/consumer provenance on the incident value nodes.
    pub fn add_exec_edge(
        &mut self,
        plan_node: PlanNodeId,
        op: ExecutableOp,
        inputs: Vec<NodeId>,
        output: NodeId,
        ordinal: u64,
    ) -> EdgeId {
        let id = EdgeId(self.next_edge);
        self.next_edge += 1;

        let mut ports = inputs
            .iter()
            .copied()
            .map(|node| Port {
                node,
                role: PortRole::Input,
            })
            .collect::<Vec<_>>();
        ports.push(Port {
            node: output,
            role: PortRole::Output,
        });

        let mut edge = HEdge::execute(op, ports);
        edge.id = id;

        for &input in &inputs {
            if let Some(node) = self.nodes.get_mut(&input) {
                node.incident.push(id);
                node.consumers.push(id);
            }
        }
        if let Some(node) = self.nodes.get_mut(&output) {
            node.incident.push(id);
            node.producer = Some(id);
            node.plan_node = Some(plan_node);
        }

        self.exec_edges.insert(id, edge);
        self.op_map.insert(
            plan_node,
            ExecInfo {
                edge: id,
                output,
                inputs,
                ordinal,
                plan_node,
            },
        );
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

    /// Look up an executable operation hyperedge by id.
    pub fn exec_edge(&self, id: EdgeId) -> Option<&HEdge> {
        self.exec_edges.get(&id)
    }

    /// The executable-operation handle for a plan node, if one was lowered.
    pub fn op_for(&self, plan_node: PlanNodeId) -> Option<&ExecInfo> {
        self.op_map.get(&plan_node)
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

    /// Stable-ordinal-sorted executable operation handles.
    pub fn exec_ops_ordered(&self) -> Vec<ExecInfo> {
        let mut ops = self.op_map.values().cloned().collect::<Vec<_>>();
        ops.sort_by_key(|info| (info.ordinal, info.plan_node.0));
        ops
    }

    pub fn record_ir(&mut self, node: NodeId, ir: &OIr) {
        self.ir_map.insert(node, ir.clone());
    }

    pub fn push_root(&mut self, node: NodeId) {
        self.root_nodes.push(node);
    }
}
