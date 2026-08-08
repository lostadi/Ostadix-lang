use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use crate::effects::{
    effect_summary_for_plan_node, ActorResourceId, EffectSummary, Fallibility, ResourceKey,
};
use crate::ir::{ExecutionPlan, OIr, PlanEdgeKind, PlanNodeId, PlanNodeKind};
use crate::value::{Fidelity, OValue};

use super::kinds::{
    AdmissionFactKind, ConstraintOp, DomainFlags, ExecutableOp, HEdgeKind, OpKind,
    ReadyInputPolicy, RepFlags, ValueState,
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

/// The semantic role of an HGraph node.
///
/// Only [`HNodeKind::Value`] nodes participate in OValue type/fidelity solving.
/// Resource and control nodes are synthetic scheduling values and never carry
/// an [`OValue`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HNodeKind {
    Value,
    ResourceState {
        resource: ResourceKey,
        version: u64,
    },
    Completion {
        plan_node: PlanNodeId,
    },
    BranchControl {
        label: String,
        version: u64,
    },
    /// A successful, digest-bound pre-execution admission fact.  Admission
    /// nodes are materialized before the coordinator receives the graph and
    /// are ordinary readiness inputs with no runtime producer.
    AdmissionEvidence {
        plan_node: PlanNodeId,
        fact: AdmissionFactKind,
    },
}

/// A semantic graph node. Type/fidelity facts apply only to ordinary Value
/// nodes. `state` tracks materialization for the graph executor.
#[derive(Clone, Debug, PartialEq)]
pub struct HNode {
    pub id: NodeId,
    pub kind: HNodeKind,
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
            kind: HNodeKind::Value,
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

    /// Construct a synthetic scheduling node. Synthetic nodes deliberately
    /// have empty type/fidelity domains and no OValue payload.
    pub fn synthetic(kind: HNodeKind, state: ValueState) -> Self {
        assert!(
            !matches!(&kind, HNodeKind::Value),
            "HNode::synthetic cannot construct an ordinary Value node"
        );
        Self {
            kind,
            domain: DomainFlags::empty(),
            rep: RepFlags::empty(),
            state,
            ..Self::fresh()
        }
    }

    pub fn resource_state(resource: ResourceKey, version: u64) -> Self {
        let state = if version == 0 {
            ValueState::Materialized
        } else {
            ValueState::Unresolved
        };
        Self::synthetic(HNodeKind::ResourceState { resource, version }, state)
    }

    pub fn completion(plan_node: PlanNodeId) -> Self {
        Self::synthetic(HNodeKind::Completion { plan_node }, ValueState::Unresolved)
    }

    pub fn branch_control(label: impl Into<String>, version: u64, materialized: bool) -> Self {
        Self::synthetic(
            HNodeKind::BranchControl {
                label: label.into(),
                version,
            },
            if materialized {
                ValueState::Materialized
            } else {
                ValueState::Unresolved
            },
        )
    }

    pub fn admission_evidence(plan_node: PlanNodeId, fact: AdmissionFactKind) -> Self {
        Self::synthetic(
            HNodeKind::AdmissionEvidence { plan_node, fact },
            ValueState::Materialized,
        )
    }

    pub fn is_value(&self) -> bool {
        matches!(&self.kind, HNodeKind::Value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
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

/// The resolved handle to an operation hyperedge for a plan node. An operation
/// has one distinguished ordinary OValue result plus completion and optional
/// successor state/control outputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecInfo {
    pub edge: EdgeId,
    pub value_output: NodeId,
    pub inputs: Vec<NodeId>,
    pub outputs: Vec<NodeId>,
    pub ordinal: u64,
    pub plan_node: PlanNodeId,
}

impl ExecInfo {
    /// Derive the coordinator input policy from this exact graph operation.
    /// Ordered first-success is admitted only for a nonempty ordered list of
    /// ordinary values produced directly by `RunRoute` operations.
    pub fn ready_input_policy(&self, graph: &HGraph) -> Result<ReadyInputPolicy, String> {
        let registered = graph.op_for(self.plan_node).ok_or_else(|| {
            format!(
                "operation {} is absent from the supplied HGraph",
                self.plan_node.0
            )
        })?;
        if registered != self {
            return Err(format!(
                "operation {} does not match the supplied HGraph projection",
                self.plan_node.0
            ));
        }
        let edge = graph.exec_edge(self.edge).ok_or_else(|| {
            format!(
                "operation {} references missing executable edge {}",
                self.plan_node.0, self.edge.0
            )
        })?;
        let HEdgeKind::Execute(ExecutableOp::SelectRoute { policy }) = &edge.op else {
            return Ok(ReadyInputPolicy::All);
        };
        if !matches!(policy.as_str(), "fallback" | "any_success") {
            return Ok(ReadyInputPolicy::All);
        }
        if self.inputs.is_empty() {
            return Err(format!(
                "ordered first-success SelectRoute operation {} has no alternative-result inputs",
                self.plan_node.0
            ));
        }
        for (position, input) in self.inputs.iter().enumerate() {
            let node = graph.node(*input).ok_or_else(|| {
                format!(
                    "ordered first-success SelectRoute operation {} references missing input node {}",
                    self.plan_node.0, input.0
                )
            })?;
            if !node.is_value() {
                return Err(format!(
                    "ordered first-success SelectRoute operation {} input {} (node {}) is not an ordinary Value",
                    self.plan_node.0, position, input.0
                ));
            }
            let producer = node.producer.ok_or_else(|| {
                format!(
                    "ordered first-success SelectRoute operation {} input {} (node {}) has no RunRoute producer",
                    self.plan_node.0, position, input.0
                )
            })?;
            let producer_edge = graph.exec_edge(producer).ok_or_else(|| {
                format!(
                    "ordered first-success SelectRoute operation {} input {} (node {}) references missing producer edge {}",
                    self.plan_node.0, position, input.0, producer.0
                )
            })?;
            if !matches!(
                &producer_edge.op,
                HEdgeKind::Execute(ExecutableOp::RunRoute { .. })
            ) {
                return Err(format!(
                    "ordered first-success SelectRoute operation {} input {} (node {}) is not produced by RunRoute",
                    self.plan_node.0, position, input.0
                ));
            }
        }
        Ok(ReadyInputPolicy::OrderedFirstSuccess)
    }
}

/// One preserved source-sequence relation lowered through the predecessor's
/// successful-completion token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceDependency {
    pub predecessor: PlanNodeId,
    pub successor: PlanNodeId,
    pub completion: NodeId,
}

#[derive(Default, Debug, PartialEq)]
pub struct HGraph {
    pub nodes: HashMap<NodeId, HNode>,
    /// Constraint/type hyperedges. Traversed by the type solver and the DOT
    /// exporter.
    pub edges: HashMap<EdgeId, HEdge>,
    /// Executable operation hyperedges, keyed by their own `EdgeId`.
    pub exec_edges: HashMap<EdgeId, HEdge>,
    /// PlanNodeId → executable-operation handle.
    pub op_map: HashMap<PlanNodeId, ExecInfo>,
    /// The exact semantic summary used to lower each executable operation.
    pub effect_summaries: HashMap<PlanNodeId, EffectSummary>,
    /// PlanNodeId → its successful-completion output node.
    pub completion_nodes: HashMap<PlanNodeId, NodeId>,
    /// Preserved sequence relations and the completion token implementing each.
    pub sequence_dependencies: Vec<SequenceDependency>,
    /// `(operation, fact kind)` to its materialized pre-execution evidence
    /// input. Draft/analyzed graphs leave this empty; admitted graphs contain
    /// exactly one node for every required fact of every operation.
    pub admission_evidence_nodes: BTreeMap<(PlanNodeId, AdmissionFactKind), NodeId>,
    pub bindings: HashMap<String, NodeId>,
    pub ir_map: HashMap<NodeId, OIr>,
    pub root_nodes: Vec<NodeId>,
    /// Exact semantic plan from which this executable graph was projected.
    /// Manually assembled compatibility graphs may omit it, but production
    /// lowering always attaches it so validation can prove completeness rather
    /// than trusting the graph's own dependency bookkeeping.
    source_plan: Option<ExecutionPlan>,
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
    /// producer/consumer provenance on every incident semantic node.
    pub fn add_exec_edge(
        &mut self,
        plan_node: PlanNodeId,
        op: ExecutableOp,
        inputs: Vec<NodeId>,
        outputs: Vec<NodeId>,
        value_output: NodeId,
        ordinal: u64,
    ) -> Result<EdgeId, String> {
        if self.op_map.contains_key(&plan_node) {
            return Err(format!(
                "plan node {} already has an executable operation",
                plan_node.0
            ));
        }

        let mut seen_inputs = HashSet::new();
        for input in &inputs {
            if !seen_inputs.insert(*input) {
                return Err(format!(
                    "executable operation {} repeats input node {}",
                    plan_node.0, input.0
                ));
            }
            if !self.nodes.contains_key(input) {
                return Err(format!(
                    "executable operation {} references missing input node {}",
                    plan_node.0, input.0
                ));
            }
        }

        let mut seen_outputs = HashSet::new();
        for output in &outputs {
            if !seen_outputs.insert(*output) {
                return Err(format!(
                    "executable operation {} repeats output node {}",
                    plan_node.0, output.0
                ));
            }
            let node = self.nodes.get(output).ok_or_else(|| {
                format!(
                    "executable operation {} references missing output node {}",
                    plan_node.0, output.0
                )
            })?;
            if node.producer.is_some() {
                return Err(format!(
                    "output node {} already has producer {:?}",
                    output.0, node.producer
                ));
            }
            if node.state != ValueState::Unresolved {
                return Err(format!(
                    "output node {} for operation {} must start unresolved",
                    output.0, plan_node.0
                ));
            }
            if seen_inputs.contains(output) {
                return Err(format!(
                    "node {} cannot be both an input and output of operation {}",
                    output.0, plan_node.0
                ));
            }
        }

        if !seen_outputs.contains(&value_output) {
            return Err(format!(
                "distinguished value output {} is not an output of operation {}",
                value_output.0, plan_node.0
            ));
        }
        let value_node = self.nodes.get(&value_output).ok_or_else(|| {
            format!(
                "operation {} has missing distinguished value output {}",
                plan_node.0, value_output.0
            )
        })?;
        if !value_node.is_value() {
            return Err(format!(
                "distinguished output {} for operation {} is not an ordinary Value node",
                value_output.0, plan_node.0
            ));
        }
        let ordinary_outputs = outputs
            .iter()
            .filter(|output| self.nodes.get(output).is_some_and(HNode::is_value))
            .count();
        if ordinary_outputs != 1 {
            return Err(format!(
                "operation {} must have exactly one ordinary Value output, found {ordinary_outputs}",
                plan_node.0
            ));
        }

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
        ports.extend(outputs.iter().copied().map(|node| Port {
            node,
            role: PortRole::Output,
        }));

        let mut edge = HEdge::execute(op, ports);
        edge.id = id;

        for &input in &inputs {
            if let Some(node) = self.nodes.get_mut(&input) {
                node.incident.push(id);
                node.consumers.push(id);
            }
        }
        for &output in &outputs {
            let node = self
                .nodes
                .get_mut(&output)
                .expect("all executable outputs were validated above");
            node.incident.push(id);
            node.producer = Some(id);
            if node.plan_node.is_none() {
                node.plan_node = Some(plan_node);
            }
        }

        self.exec_edges.insert(id, edge);
        self.op_map.insert(
            plan_node,
            ExecInfo {
                edge: id,
                value_output,
                inputs,
                outputs,
                ordinal,
                plan_node,
            },
        );
        Ok(id)
    }

    pub fn set_effect_summary(&mut self, plan_node: PlanNodeId, summary: EffectSummary) {
        self.effect_summaries.insert(plan_node, summary);
    }

    pub fn effect_summary(&self, plan_node: PlanNodeId) -> Option<&EffectSummary> {
        self.effect_summaries.get(&plan_node)
    }

    pub fn set_completion_node(
        &mut self,
        plan_node: PlanNodeId,
        node: NodeId,
    ) -> Result<(), String> {
        let graph_node = self
            .node(node)
            .ok_or_else(|| format!("completion node {} does not exist", node.0))?;
        if graph_node.kind != (HNodeKind::Completion { plan_node }) {
            return Err(format!(
                "node {} is not Completion({})",
                node.0, plan_node.0
            ));
        }
        if let Some(existing) = self.completion_nodes.get(&plan_node) {
            if *existing != node {
                return Err(format!(
                    "plan node {} already maps to completion node {}",
                    plan_node.0, existing.0
                ));
            }
        }
        self.completion_nodes.insert(plan_node, node);
        Ok(())
    }

    pub fn completion_node(&self, plan_node: PlanNodeId) -> Option<NodeId> {
        self.completion_nodes.get(&plan_node).copied()
    }

    pub fn add_completion_node(&mut self, plan_node: PlanNodeId) -> Result<NodeId, String> {
        if let Some(existing) = self.completion_node(plan_node) {
            return Err(format!(
                "plan node {} already maps to completion node {}",
                plan_node.0, existing.0
            ));
        }
        let node = self.add_node(HNode::completion(plan_node));
        self.completion_nodes.insert(plan_node, node);
        Ok(node)
    }

    /// Materialize and attach one digest-bound admission fact to an existing
    /// operation. Only the admission compiler should call this; graph
    /// validation independently checks the resulting inventory.
    pub(crate) fn add_admission_evidence_input(
        &mut self,
        plan_node: PlanNodeId,
        fact: AdmissionFactKind,
    ) -> Result<NodeId, String> {
        if self
            .admission_evidence_nodes
            .contains_key(&(plan_node, fact))
        {
            return Err(format!(
                "operation {} already has {} admission evidence",
                plan_node.0,
                fact.name()
            ));
        }
        let edge = self
            .op_map
            .get(&plan_node)
            .map(|info| info.edge)
            .ok_or_else(|| format!("operation {} does not exist", plan_node.0))?;
        let node = self.add_node(HNode::admission_evidence(plan_node, fact));

        self.nodes
            .get_mut(&node)
            .expect("newly added evidence node exists")
            .consumers
            .push(edge);
        self.nodes
            .get_mut(&node)
            .expect("newly added evidence node exists")
            .incident
            .push(edge);
        self.exec_edges
            .get_mut(&edge)
            .expect("registered operation edge exists")
            .ports
            .push(Port {
                node,
                role: PortRole::Input,
            });
        self.op_map
            .get_mut(&plan_node)
            .expect("registered operation exists")
            .inputs
            .push(node);
        self.admission_evidence_nodes
            .insert((plan_node, fact), node);
        Ok(node)
    }

    pub fn record_sequence_dependency(
        &mut self,
        predecessor: PlanNodeId,
        successor: PlanNodeId,
        completion: NodeId,
    ) -> Result<(), String> {
        let node = self
            .node(completion)
            .ok_or_else(|| format!("sequence completion node {} does not exist", completion.0))?;
        if node.kind
            != (HNodeKind::Completion {
                plan_node: predecessor,
            })
        {
            return Err(format!(
                "sequence {} -> {} uses node {} which is not the predecessor completion",
                predecessor.0, successor.0, completion.0
            ));
        }
        let dependency = SequenceDependency {
            predecessor,
            successor,
            completion,
        };
        if !self.sequence_dependencies.contains(&dependency) {
            self.sequence_dependencies.push(dependency);
        }
        Ok(())
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

    /// Stable textual inspection of the directed execution HGraph. Unlike the
    /// OIR/ExecutionPlan dump, this exposes the actual value, state, completion,
    /// and operation vertices from which readiness is derived.
    pub fn to_execution_text(&self) -> String {
        let mut out = String::from("; HGraph\n");
        for id in self.node_ids() {
            let node = &self.nodes[&id];
            let plan = node
                .plan_node
                .map(|plan_node| plan_node.0.to_string())
                .unwrap_or_else(|| "-".to_string());
            let producer = node
                .producer
                .map(|edge| format!("e{}", edge.0))
                .unwrap_or_else(|| "-".to_string());
            let mut consumers = node.consumers.clone();
            consumers.sort();
            let consumers = consumers
                .iter()
                .map(|edge| format!("e{}", edge.0))
                .collect::<Vec<_>>()
                .join(",");
            let kind = match &node.kind {
                HNodeKind::Value => "Value".to_string(),
                HNodeKind::ResourceState { resource, version } => {
                    format!("ResourceState({resource}@{version})")
                }
                HNodeKind::Completion { plan_node } => {
                    format!("Completion({})", plan_node.0)
                }
                HNodeKind::BranchControl { label, version } => {
                    format!("BranchControl({label}@{version})")
                }
                HNodeKind::AdmissionEvidence { plan_node, fact } => {
                    format!("AdmissionEvidence({}:P{})", fact.name(), plan_node.0)
                }
            };
            writeln!(
                out,
                "node n{} {kind} plan={plan} state={:?} producer={producer} consumers=[{consumers}]",
                id.0, node.state
            )
            .expect("writing to a String cannot fail");
        }

        for info in self.exec_ops_ordered() {
            let edge = &self.exec_edges[&info.edge];
            let op = match &edge.op {
                HEdgeKind::Execute(op) => format!("{op:?}"),
                HEdgeKind::Constraint(_) => "<invalid constraint>".to_string(),
            };
            let input_policy = match info.ready_input_policy(self) {
                Ok(ReadyInputPolicy::All) => "",
                Ok(ReadyInputPolicy::OrderedFirstSuccess) => " input-policy=ordered-first-success",
                Err(_) => " input-policy=invalid",
            };
            let inputs = format_node_ids(&info.inputs);
            let outputs = format_node_ids(&info.outputs);
            writeln!(
                out,
                "execute e{} plan={} op={op}{input_policy} inputs=[{inputs}] -> outputs=[{outputs}] value=n{}",
                info.edge.0, info.plan_node.0, info.value_output.0
            )
            .expect("writing to a String cannot fail");
        }

        for edge_id in self.edge_ids() {
            let edge = &self.edges[&edge_id];
            let ports = edge
                .ports
                .iter()
                .map(|port| format!("{:?}:n{}", port.role, port.node.0))
                .collect::<Vec<_>>()
                .join(",");
            writeln!(
                out,
                "constraint e{} op={:?} ports=[{ports}]",
                edge_id.0, edge.op
            )
            .expect("writing to a String cannot fail");
        }
        out
    }

    pub fn record_ir(&mut self, node: NodeId, ir: &OIr) {
        self.ir_map.insert(node, ir.clone());
    }

    pub fn push_root(&mut self, node: NodeId) {
        self.root_nodes.push(node);
    }

    pub(super) fn set_source_plan(&mut self, plan: ExecutionPlan) {
        self.source_plan = Some(plan);
    }

    /// Validate that the executable graph is a complete, directed scheduling
    /// graph over ordinary values plus state/control tokens.
    pub fn validate_execution_graph(&self) -> Result<(), String> {
        self.validate_node_kinds()?;
        self.validate_operation_shapes()?;
        self.validate_resource_chains()?;
        self.validate_sequence_dependencies()?;
        self.validate_source_plan_dependencies()?;
        self.validate_executable_acyclicity()?;
        Ok(())
    }

    /// Validate the additional invariant required at the coordinator
    /// boundary: every operation consumes exactly one materialized token for
    /// every admission fact kind, and no unattached/forged token exists.
    pub fn validate_admitted_execution_graph(&self) -> Result<(), String> {
        self.validate_execution_graph()?;
        let expected = self
            .op_map
            .len()
            .saturating_mul(AdmissionFactKind::ALL.len());
        if self.admission_evidence_nodes.len() != expected {
            return Err(format!(
                "admitted graph has {} evidence nodes; expected {expected}",
                self.admission_evidence_nodes.len()
            ));
        }
        for plan_node in self.op_map.keys().copied() {
            let info = &self.op_map[&plan_node];
            for fact in AdmissionFactKind::ALL {
                let node_id = self
                    .admission_evidence_nodes
                    .get(&(plan_node, fact))
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "operation {} lacks {} admission evidence",
                            plan_node.0,
                            fact.name()
                        )
                    })?;
                let node = self.nodes.get(&node_id).ok_or_else(|| {
                    format!("admission evidence node {} does not exist", node_id.0)
                })?;
                if node.kind != (HNodeKind::AdmissionEvidence { plan_node, fact })
                    || node.state != ValueState::Materialized
                    || node.producer.is_some()
                    || node.consumers != [info.edge]
                    || !info.inputs.contains(&node_id)
                {
                    return Err(format!(
                        "operation {} has invalid {} admission evidence node {}",
                        plan_node.0,
                        fact.name(),
                        node_id.0
                    ));
                }
            }
        }
        for (node_id, node) in &self.nodes {
            if let HNodeKind::AdmissionEvidence { plan_node, fact } = node.kind {
                if self.admission_evidence_nodes.get(&(plan_node, fact)) != Some(node_id) {
                    return Err(format!(
                        "unregistered admission evidence node {} for operation {}",
                        node_id.0, plan_node.0
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validate the executable graph and prove that it was projected from this
    /// exact logical execution plan.
    ///
    /// Consumers such as planner inspection must not pair a valid graph with a
    /// different plan whose node identifiers happen to overlap.
    pub fn validate_execution_plan(&self, plan: &ExecutionPlan) -> Result<(), String> {
        self.validate_execution_graph()?;
        if self.source_plan.as_ref() != Some(plan) {
            return Err("execution plan does not match the HGraph source plan".to_string());
        }
        Ok(())
    }

    /// Validate both graph completeness and the exact program/plan provenance
    /// that will be executed by the coordinator. A state-complete graph is not
    /// a transferable scheduling hint: pairing it with different OIR could run
    /// effectful instructions under another program's pure/resource model.
    pub fn validate_execution_source(
        &self,
        program: &crate::ir::OIrProgram,
        plan: &ExecutionPlan,
    ) -> Result<(), String> {
        self.validate_execution_plan(plan)?;

        let flat = program.flatten_for_plan();
        if flat.len() != plan.nodes.len() {
            return Err(format!(
                "coordinator OIR has {} flattened nodes for source plan with {} nodes",
                flat.len(),
                plan.nodes.len()
            ));
        }
        let value_for_plan = self
            .nodes
            .iter()
            .filter_map(|(node_id, node)| (node.is_value()).then_some((node.plan_node, *node_id)))
            .collect::<Vec<_>>();
        for (index, oir) in flat.into_iter().enumerate() {
            let plan_node = PlanNodeId(index);
            let value = value_for_plan
                .iter()
                .find_map(|(candidate, node)| (*candidate == Some(plan_node)).then_some(*node))
                .ok_or_else(|| {
                    format!(
                        "coordinator plan node {} has no HGraph value provenance",
                        plan_node.0
                    )
                })?;
            let recorded = self.ir_map.get(&value).ok_or_else(|| {
                format!(
                    "HGraph value node {} has no OIR provenance for coordinator plan node {}",
                    value.0, plan_node.0
                )
            })?;
            if recorded != oir {
                return Err(format!(
                    "coordinator OIR node {} does not match HGraph source provenance",
                    plan_node.0
                ));
            }
        }
        Ok(())
    }

    fn validate_node_kinds(&self) -> Result<(), String> {
        for (id, node) in &self.nodes {
            if !node.is_value() {
                if node.value.is_some()
                    || !node.domain.is_empty()
                    || !node.rep.is_empty()
                    || node.actor.is_some()
                    || node.fidelity.is_some()
                {
                    return Err(format!(
                        "synthetic node {} ({:?}) carries ordinary value/type metadata",
                        id.0, node.kind
                    ));
                }
                if let HNodeKind::Completion { plan_node } = &node.kind {
                    if self.completion_node(*plan_node) != Some(*id) {
                        return Err(format!(
                            "completion node {} for operation {} is not registered",
                            id.0, plan_node.0
                        ));
                    }
                }
            }
            if let HNodeKind::ResourceState { version: 0, .. } = &node.kind {
                if node.producer.is_some() || node.state != ValueState::Materialized {
                    return Err(format!(
                        "initial resource-state node {} must be materialized with no producer",
                        id.0
                    ));
                }
            }
            if let Some(producer) = node.producer {
                let edge = self.exec_edges.get(&producer).ok_or_else(|| {
                    format!(
                        "node {} names missing executable producer edge {}",
                        id.0, producer.0
                    )
                })?;
                if !edge
                    .ports
                    .iter()
                    .any(|port| port.node == *id && port.role == PortRole::Output)
                {
                    return Err(format!(
                        "node {} producer edge {} does not expose it as an output",
                        id.0, producer.0
                    ));
                }
            }
            let mut seen_consumers = HashSet::new();
            for consumer in &node.consumers {
                if !seen_consumers.insert(*consumer) {
                    return Err(format!(
                        "node {} repeats executable consumer edge {}",
                        id.0, consumer.0
                    ));
                }
                let edge = self.exec_edges.get(consumer).ok_or_else(|| {
                    format!(
                        "node {} names missing executable consumer edge {}",
                        id.0, consumer.0
                    )
                })?;
                if !edge
                    .ports
                    .iter()
                    .any(|port| port.node == *id && port.role == PortRole::Input)
                {
                    return Err(format!(
                        "node {} consumer edge {} does not expose it as an input",
                        id.0, consumer.0
                    ));
                }
            }
        }

        for node in self.ir_map.keys() {
            let graph_node = self
                .nodes
                .get(node)
                .ok_or_else(|| format!("IR provenance references missing graph node {}", node.0))?;
            if !graph_node.is_value() {
                return Err(format!(
                    "IR provenance is attached to synthetic node {} ({:?})",
                    node.0, graph_node.kind
                ));
            }
        }

        for root in &self.root_nodes {
            let node = self
                .nodes
                .get(root)
                .ok_or_else(|| format!("root node {} does not exist", root.0))?;
            if !node.is_value() {
                return Err(format!(
                    "root node {} ({:?}) is not an ordinary Value node",
                    root.0, node.kind
                ));
            }
        }
        Ok(())
    }

    fn validate_operation_shapes(&self) -> Result<(), String> {
        let mut produced_by: HashMap<NodeId, EdgeId> = HashMap::new();
        let mut mapped_edges = HashSet::new();

        for (plan_node, info) in &self.op_map {
            if info.plan_node != *plan_node {
                return Err(format!(
                    "operation map key {} disagrees with ExecInfo plan node {}",
                    plan_node.0, info.plan_node.0
                ));
            }
            let edge = self.exec_edges.get(&info.edge).ok_or_else(|| {
                format!(
                    "operation {} references missing executable edge {}",
                    plan_node.0, info.edge.0
                )
            })?;
            if !mapped_edges.insert(info.edge) {
                return Err(format!(
                    "executable edge {} is mapped to more than one plan operation",
                    info.edge.0
                ));
            }
            if !edge.is_execute() {
                return Err(format!(
                    "operation {} edge {} is not executable",
                    plan_node.0, info.edge.0
                ));
            }

            let edge_inputs = edge
                .ports
                .iter()
                .filter(|port| port.role == PortRole::Input)
                .map(|port| port.node)
                .collect::<Vec<_>>();
            let edge_outputs = edge
                .ports
                .iter()
                .filter(|port| port.role == PortRole::Output)
                .map(|port| port.node)
                .collect::<Vec<_>>();
            let unique_inputs = info.inputs.iter().copied().collect::<HashSet<_>>();
            let unique_outputs = info.outputs.iter().copied().collect::<HashSet<_>>();
            if unique_inputs.len() != info.inputs.len()
                || unique_outputs.len() != info.outputs.len()
            {
                return Err(format!(
                    "operation {} repeats an input or output node",
                    plan_node.0
                ));
            }
            if let Some(overlap) = unique_inputs.intersection(&unique_outputs).next() {
                return Err(format!(
                    "operation {} uses node {} as both input and output",
                    plan_node.0, overlap.0
                ));
            }
            if edge_inputs != info.inputs || edge_outputs != info.outputs {
                return Err(format!(
                    "operation {} ExecInfo ports disagree with executable edge {}",
                    plan_node.0, info.edge.0
                ));
            }

            for input in &info.inputs {
                let node = self.nodes.get(input).ok_or_else(|| {
                    format!(
                        "operation {} consumes missing input node {}",
                        plan_node.0, input.0
                    )
                })?;
                if node.state != ValueState::Materialized && node.producer.is_none() {
                    return Err(format!(
                        "operation {} input node {} is neither initially materialized nor produced",
                        plan_node.0, input.0
                    ));
                }
                if !node.consumers.contains(&info.edge) {
                    return Err(format!(
                        "operation {} input node {} lacks consumer provenance",
                        plan_node.0, input.0
                    ));
                }
            }

            if !info.outputs.contains(&info.value_output) {
                return Err(format!(
                    "operation {} distinguished value output {} is absent from outputs",
                    plan_node.0, info.value_output.0
                ));
            }
            let ordinary_outputs = info
                .outputs
                .iter()
                .filter(|output| self.nodes.get(output).is_some_and(HNode::is_value))
                .copied()
                .collect::<Vec<_>>();
            if ordinary_outputs != [info.value_output] {
                return Err(format!(
                    "operation {} must have exactly distinguished Value output {}, got {:?}",
                    plan_node.0, info.value_output.0, ordinary_outputs
                ));
            }

            let completions = info
                .outputs
                .iter()
                .filter(|output| {
                    self.nodes.get(output).is_some_and(|node| {
                        node.kind
                            == (HNodeKind::Completion {
                                plan_node: *plan_node,
                            })
                    })
                })
                .copied()
                .collect::<Vec<_>>();
            if completions.len() != 1 {
                return Err(format!(
                    "operation {} must have exactly one matching Completion output, found {}",
                    plan_node.0,
                    completions.len()
                ));
            }
            if self.completion_node(*plan_node) != Some(completions[0]) {
                return Err(format!(
                    "operation {} completion map does not name output node {}",
                    plan_node.0, completions[0].0
                ));
            }

            for output in &info.outputs {
                let node = self.nodes.get(output).ok_or_else(|| {
                    format!(
                        "operation {} produces missing output node {}",
                        plan_node.0, output.0
                    )
                })?;
                if node.producer != Some(info.edge) {
                    return Err(format!(
                        "operation {} output node {} has inconsistent producer {:?}",
                        plan_node.0, output.0, node.producer
                    ));
                }
                if node.state != ValueState::Unresolved {
                    return Err(format!(
                        "operation {} output node {} must be unresolved before execution",
                        plan_node.0, output.0
                    ));
                }
                if let Some(previous) = produced_by.insert(*output, info.edge) {
                    if previous != info.edge {
                        return Err(format!(
                            "node {} has multiple executable producers {} and {}",
                            output.0, previous.0, info.edge.0
                        ));
                    }
                }
            }

            let summary = self.effect_summaries.get(plan_node).ok_or_else(|| {
                format!("operation {} has no semantic effect summary", plan_node.0)
            })?;
            self.validate_operation_semantics(info, edge, summary)?;
            let (reads, writes) = summary.scheduling_accesses();
            let actual_inputs = resource_keys(self, &info.inputs);
            let actual_outputs = resource_keys(self, &info.outputs);
            let autonomous_ephemeral = self.source_plan.as_ref().is_some_and(|plan| {
                self.ir_map.get(&info.value_output).is_some_and(|oir| {
                    crate::hgraph::from_oir::autonomous_ephemeral_group(plan, info.plan_node, oir)
                        .is_some()
                })
            });
            let required = if autonomous_ephemeral {
                BTreeSet::new()
            } else {
                reads.union(&writes).cloned().collect::<BTreeSet<_>>()
            };
            let writes = if autonomous_ephemeral {
                BTreeSet::new()
            } else {
                writes
            };
            if actual_inputs != required || actual_outputs != writes {
                return Err(format!(
                    "operation {} resource inputs/outputs {:?}/{:?} differ from admitted accesses {:?}/{:?}",
                    plan_node.0, actual_inputs, actual_outputs, required, writes
                ));
            }
            for resource in required {
                if writes.contains(&resource) {
                    self.validate_operation_resource_write(info, &resource)?;
                } else {
                    self.validate_operation_resource_read(info, &resource)?;
                }
            }
        }

        for edge in self.exec_edges.keys() {
            if !mapped_edges.contains(edge) {
                return Err(format!(
                    "executable edge {} has no plan-operation mapping",
                    edge.0
                ));
            }
        }

        for (plan_node, node) in &self.completion_nodes {
            if !self.op_map.contains_key(plan_node) {
                return Err(format!(
                    "completion node {} is registered for non-executable plan node {}",
                    node.0, plan_node.0
                ));
            }
        }
        for plan_node in self.effect_summaries.keys() {
            if !self.op_map.contains_key(plan_node) {
                return Err(format!(
                    "effect summary is registered for non-executable plan node {}",
                    plan_node.0
                ));
            }
        }
        Ok(())
    }

    /// Validate irreducible operation semantics independently of the supplied
    /// effect summary. This prevents a malformed graph from making hosted or
    /// persistent work look pure simply by attaching a forged summary.
    fn validate_operation_semantics(
        &self,
        info: &ExecInfo,
        edge: &HEdge,
        summary: &EffectSummary,
    ) -> Result<(), String> {
        if summary.unknown {
            require_read_write(summary, &ResourceKey::HostWorld, info.plan_node)?;
        }

        let HEdgeKind::Execute(op) = &edge.op else {
            return Err(format!(
                "operation {} does not carry executable semantics",
                info.plan_node.0
            ));
        };
        match op {
            ExecutableOp::EvalBackend { lang, env } => {
                if !summary.unknown
                    || summary.deterministic
                    || summary.fallibility != Fallibility::MayFail
                {
                    return Err(format!(
                        "EvalBackend operation {} must remain unknown, nondeterministic, and fallible",
                        info.plan_node.0
                    ));
                }
                let autonomous_ephemeral = self.source_plan.as_ref().is_some_and(|plan| {
                    self.ir_map.get(&info.value_output).is_some_and(|oir| {
                        crate::hgraph::from_oir::autonomous_ephemeral_group(
                            plan,
                            info.plan_node,
                            oir,
                        )
                        .is_some()
                    })
                });
                for resource in [ResourceKey::HostWorld, ResourceKey::EvaluatorState] {
                    require_read_write(summary, &resource, info.plan_node)?;
                    if !autonomous_ephemeral {
                        self.validate_operation_resource_write(info, &resource)?;
                    }
                }
                if *env != u32::MAX {
                    let actor = ActorResourceId::new(lang.clone(), *env);
                    if summary.actor_state.as_ref() != Some(&actor) {
                        return Err(format!(
                            "persistent EvalBackend operation {} requires actor state {}",
                            info.plan_node.0, actor
                        ));
                    }
                    self.validate_operation_resource_write(info, &ResourceKey::ActorState(actor))?;
                }
            }
            ExecutableOp::Invoke { .. }
            | ExecutableOp::Request { .. }
            | ExecutableOp::ForceRequest { .. }
            | ExecutableOp::Schedule { .. } => {
                for resource in [ResourceKey::HostWorld, ResourceKey::EvaluatorState] {
                    require_read_write(summary, &resource, info.plan_node)?;
                    self.validate_operation_resource_write(info, &resource)?;
                }
            }
            ExecutableOp::MaterializeProject => {
                require_hosted_project_operation(summary, info.plan_node, false)?;
            }
            ExecutableOp::RunRoute { .. } => {
                require_hosted_project_operation(summary, info.plan_node, true)?;
            }
            ExecutableOp::BuildRoute { .. } => {
                if !summary.is_verified_pure_infallible() {
                    return Err(format!(
                        "BuildRoute operation {} must be verified logical preparation",
                        info.plan_node.0
                    ));
                }
            }
            ExecutableOp::SelectRoute { policy } => {
                if policy.is_empty()
                    || summary.unknown
                    || summary.fallibility != Fallibility::MayFail
                    || summary.actor_state.is_some()
                    || !summary.reads.is_empty()
                    || !summary.writes.is_empty()
                    || summary.spawn
                {
                    return Err(format!(
                        "SelectRoute operation {} has invalid logical policy effects",
                        info.plan_node.0
                    ));
                }
            }
            ExecutableOp::CompareRouteResults
                if summary.unknown
                    || !summary.deterministic
                    || summary.fallibility != Fallibility::MayFail
                    || summary.actor_state.is_some()
                    || !summary.reads.is_empty()
                    || !summary.writes.is_empty()
                    || summary.spawn =>
            {
                return Err(format!(
                    "CompareRouteResults operation {} has invalid logical comparison effects",
                    info.plan_node.0
                ));
            }
            ExecutableOp::CompareRouteResults => {}
            _ => {}
        }
        Ok(())
    }

    fn validate_operation_resource_read(
        &self,
        info: &ExecInfo,
        resource: &ResourceKey,
    ) -> Result<(), String> {
        let input_versions = resource_versions(self, &info.inputs, resource);
        let output_versions = resource_versions(self, &info.outputs, resource);
        if input_versions.len() != 1 || !output_versions.is_empty() {
            return Err(format!(
                "operation {} requires exactly one {:?} read-state input and no output, found {}/{}",
                info.plan_node.0,
                resource,
                input_versions.len(),
                output_versions.len()
            ));
        }
        Ok(())
    }

    fn validate_operation_resource_write(
        &self,
        info: &ExecInfo,
        resource: &ResourceKey,
    ) -> Result<(), String> {
        let input_versions = resource_versions(self, &info.inputs, resource);
        let output_versions = resource_versions(self, &info.outputs, resource);
        if input_versions.len() != 1 || output_versions.len() != 1 {
            return Err(format!(
                "operation {} requires exactly one {:?} state input/output, found {}/{}",
                info.plan_node.0,
                resource,
                input_versions.len(),
                output_versions.len()
            ));
        }
        if output_versions[0] != input_versions[0].saturating_add(1) {
            return Err(format!(
                "operation {} {:?} state must advance {} -> {}, got {} -> {}",
                info.plan_node.0,
                resource,
                input_versions[0],
                input_versions[0].saturating_add(1),
                input_versions[0],
                output_versions[0]
            ));
        }
        Ok(())
    }

    fn validate_resource_chains(&self) -> Result<(), String> {
        let mut versions: BTreeMap<ResourceKey, BTreeMap<u64, NodeId>> = BTreeMap::new();
        let edge_to_info = self
            .op_map
            .values()
            .map(|info| (info.edge, info))
            .collect::<HashMap<_, _>>();

        for (id, node) in &self.nodes {
            let HNodeKind::ResourceState { resource, version } = &node.kind else {
                continue;
            };
            if let Some(existing) = versions
                .entry(resource.clone())
                .or_default()
                .insert(*version, *id)
            {
                return Err(format!(
                    "resource {:?} version {} has duplicate nodes {} and {}",
                    resource, version, existing.0, id.0
                ));
            }

            if *version == 0 {
                continue;
            }
            let producer = node.producer.ok_or_else(|| {
                format!(
                    "resource {:?} version {} node {} has no producer",
                    resource, version, id.0
                )
            })?;
            let info = edge_to_info.get(&producer).ok_or_else(|| {
                format!(
                    "resource {:?} version {} has unregistered producer edge {}",
                    resource, version, producer.0
                )
            })?;
            let prior = resource_versions(self, &info.inputs, resource);
            if prior.len() != 1 || prior[0].checked_add(1) != Some(*version) {
                return Err(format!(
                    "resource {:?} output version {} from operation {} does not consume exactly version {}",
                    resource,
                    version,
                    info.plan_node.0,
                    version - 1
                ));
            }
        }

        for (resource, chain) in versions {
            let Some((&highest, _)) = chain.last_key_value() else {
                continue;
            };
            for version in 0..=highest {
                if !chain.contains_key(&version) {
                    return Err(format!(
                        "resource {:?} state chain skips version {}",
                        resource, version
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_sequence_dependencies(&self) -> Result<(), String> {
        for dependency in &self.sequence_dependencies {
            let predecessor = self.op_map.get(&dependency.predecessor).ok_or_else(|| {
                format!(
                    "sequence predecessor {} has no executable operation",
                    dependency.predecessor.0
                )
            })?;
            let successor = self.op_map.get(&dependency.successor).ok_or_else(|| {
                format!(
                    "sequence successor {} has no executable operation",
                    dependency.successor.0
                )
            })?;
            let completion = self.nodes.get(&dependency.completion).ok_or_else(|| {
                format!(
                    "sequence {} -> {} names missing completion node {}",
                    dependency.predecessor.0, dependency.successor.0, dependency.completion.0
                )
            })?;
            if completion.kind
                != (HNodeKind::Completion {
                    plan_node: dependency.predecessor,
                })
                || completion.producer != Some(predecessor.edge)
                || !predecessor.outputs.contains(&dependency.completion)
                || !successor.inputs.contains(&dependency.completion)
            {
                return Err(format!(
                    "sequence {} -> {} is not implemented by predecessor completion node {}",
                    dependency.predecessor.0, dependency.successor.0, dependency.completion.0
                ));
            }
        }
        Ok(())
    }

    /// Prove that the executable projection contains every dependency required
    /// by its source plan. Validation of only the dependencies already recorded
    /// in the HGraph is circular: an omitted edge and omitted bookkeeping entry
    /// would otherwise agree. Production lowering always attaches a plan and
    /// therefore takes this independent validation path.
    fn validate_source_plan_dependencies(&self) -> Result<(), String> {
        let Some(plan) = &self.source_plan else {
            return Ok(());
        };
        plan.validate(plan.roots.len())?;

        let mut value_for_plan = BTreeMap::new();
        for (node_id, node) in &self.nodes {
            if !node.is_value() {
                continue;
            }
            let plan_node = node.plan_node.ok_or_else(|| {
                format!(
                    "ordinary Value node {} has no source-plan identity",
                    node_id.0
                )
            })?;
            if plan_node.0 >= plan.nodes.len() {
                return Err(format!(
                    "ordinary Value node {} names out-of-bounds plan node {}",
                    node_id.0, plan_node.0
                ));
            }
            if let Some(previous) = value_for_plan.insert(plan_node, *node_id) {
                return Err(format!(
                    "plan node {} has multiple ordinary Value nodes {} and {}",
                    plan_node.0, previous.0, node_id.0
                ));
            }
            if !self.ir_map.contains_key(node_id) {
                return Err(format!(
                    "ordinary Value node {} for plan node {} has no OIR provenance",
                    node_id.0, plan_node.0
                ));
            }
        }
        if value_for_plan.len() != plan.nodes.len() || self.ir_map.len() != plan.nodes.len() {
            return Err(format!(
                "source plan has {} nodes but projection has {} ordinary values and {} OIR mappings",
                plan.nodes.len(),
                value_for_plan.len(),
                self.ir_map.len()
            ));
        }

        let expected_roots = plan
            .roots
            .iter()
            .map(|root| value_for_plan[root])
            .collect::<Vec<_>>();
        if self.root_nodes != expected_roots {
            return Err(format!(
                "HGraph roots {:?} do not match source-plan roots {:?}",
                self.root_nodes, expected_roots
            ));
        }

        for plan_node in &plan.nodes {
            let value = value_for_plan[&plan_node.id];
            let expected_op = super::from_oir::executable_op(&plan_node.kind);
            let Some(expected_op) = expected_op else {
                if !matches!(&plan_node.kind, PlanNodeKind::Text) {
                    return Err(format!(
                        "plan node {} has no executable lowering",
                        plan_node.id.0
                    ));
                }
                let node = &self.nodes[&value];
                if node.state != ValueState::Materialized
                    || node.producer.is_some()
                    || self.op_map.contains_key(&plan_node.id)
                {
                    return Err(format!(
                        "literal plan node {} must be an initially materialized, producer-free value",
                        plan_node.id.0
                    ));
                }
                continue;
            };

            let info = self.op_map.get(&plan_node.id).ok_or_else(|| {
                format!(
                    "executable source-plan node {} has no operation",
                    plan_node.id.0
                )
            })?;
            if info.value_output != value {
                return Err(format!(
                    "operation {} produces Value node {}, expected source-plan node {}",
                    plan_node.id.0, info.value_output.0, value.0
                ));
            }
            let edge = &self.exec_edges[&info.edge];
            if edge.op != HEdgeKind::Execute(expected_op) {
                return Err(format!(
                    "operation {} semantics {:?} do not match source plan {:?}",
                    plan_node.id.0, edge.op, plan_node.kind
                ));
            }

            let derived = effect_summary_for_plan_node(plan_node.id, &plan_node.kind)?;
            let actual = &self.effect_summaries[&plan_node.id];
            if *actual != derived {
                return Err(format!(
                    "operation {} effect summary does not match independently derived source semantics",
                    plan_node.id.0
                ));
            }

            let expected_value_inputs = plan
                .edges
                .iter()
                .filter_map(|edge| {
                    (edge.to == plan_node.id
                        && matches!(edge.kind, PlanEdgeKind::Structural | PlanEdgeKind::Data))
                    .then_some(value_for_plan[&edge.from])
                })
                .collect::<BTreeSet<_>>();
            let actual_value_inputs = info
                .inputs
                .iter()
                .filter(|node| self.nodes[node].is_value())
                .copied()
                .collect::<BTreeSet<_>>();
            if actual_value_inputs != expected_value_inputs {
                return Err(format!(
                    "operation {} ordinary inputs {:?} do not exactly implement source-plan inputs {:?}",
                    plan_node.id.0, actual_value_inputs, expected_value_inputs
                ));
            }
        }

        let mut expected_sequences = BTreeSet::new();
        for target in plan.nodes.iter().map(|node| node.id) {
            if super::from_oir::executable_op(&plan.nodes[target.0].kind).is_none() {
                continue;
            }
            for predecessor in super::from_oir::executable_sequence_predecessors(plan, target) {
                if !super::from_oir::sequence_can_relax(
                    plan,
                    predecessor,
                    target,
                    &self.effect_summaries,
                ) {
                    expected_sequences.insert((predecessor, target));
                    let completion = self.completion_node(predecessor).ok_or_else(|| {
                        format!(
                            "source sequence {} -> {} has no predecessor completion",
                            predecessor.0, target.0
                        )
                    })?;
                    if !self.op_map[&target].inputs.contains(&completion) {
                        return Err(format!(
                            "source sequence {} -> {} is missing completion input {}",
                            predecessor.0, target.0, completion.0
                        ));
                    }
                }
            }
        }

        let actual_sequences = self
            .sequence_dependencies
            .iter()
            .map(|dependency| (dependency.predecessor, dependency.successor))
            .collect::<BTreeSet<_>>();
        if actual_sequences.len() != self.sequence_dependencies.len() {
            return Err("source sequence dependency bookkeeping contains duplicates".to_string());
        }
        if actual_sequences != expected_sequences {
            return Err(format!(
                "preserved sequence dependencies {:?} do not exactly match source-plan requirements {:?}",
                actual_sequences, expected_sequences
            ));
        }
        self.validate_source_resource_frontiers(plan)?;
        Ok(())
    }

    /// Replay the canonical reader/writer lowering from independently derived
    /// source effects. Local version monotonicity alone cannot prove that a
    /// writer drains every earlier reader completion, so this closes that
    /// omission attack at validation time.
    fn validate_source_resource_frontiers(&self, plan: &ExecutionPlan) -> Result<(), String> {
        #[derive(Default)]
        struct ExpectedFrontier {
            last_write: Option<NodeId>,
            open_reads: BTreeSet<NodeId>,
            version: u64,
        }

        let mut initial = BTreeMap::new();
        for (node_id, node) in &self.nodes {
            if let HNodeKind::ResourceState {
                resource,
                version: 0,
            } = &node.kind
            {
                if initial.insert(resource.clone(), *node_id).is_some() {
                    return Err(format!(
                        "resource {:?} has more than one initial state",
                        resource
                    ));
                }
            }
        }

        let mut frontiers: BTreeMap<ResourceKey, ExpectedFrontier> = BTreeMap::new();
        for plan_node in plan.topological_order()? {
            let Some(info) = self.op_map.get(&plan_node) else {
                continue;
            };
            let summary = effect_summary_for_plan_node(plan_node, &plan.nodes[plan_node.0].kind)?;
            let oir = self.ir_map.get(&info.value_output).ok_or_else(|| {
                format!("operation {} has no recorded OIR value node", plan_node.0)
            })?;
            if crate::hgraph::from_oir::autonomous_ephemeral_group(plan, plan_node, oir).is_some() {
                continue;
            }
            let (reads, writes) = summary.scheduling_accesses();
            let resources = reads.union(&writes).cloned().collect::<BTreeSet<_>>();
            let completion = self
                .completion_node(plan_node)
                .ok_or_else(|| format!("operation {} has no completion node", plan_node.0))?;

            for resource in resources {
                let frontier = frontiers.entry(resource.clone()).or_default();
                let expected_state = match frontier.last_write {
                    Some(node) => node,
                    None => initial
                        .get(&resource)
                        .copied()
                        .ok_or_else(|| format!("resource {:?} has no initial state", resource))?,
                };
                if !info.inputs.contains(&expected_state) {
                    return Err(format!(
                        "operation {} does not consume the current {:?} writer state node {}",
                        plan_node.0, resource, expected_state.0
                    ));
                }

                if writes.contains(&resource) {
                    for reader in &frontier.open_reads {
                        if !info.inputs.contains(reader) {
                            return Err(format!(
                                "operation {} {:?} write omits open reader completion node {}",
                                plan_node.0, resource, reader.0
                            ));
                        }
                    }
                    let outputs = info
                        .outputs
                        .iter()
                        .filter_map(|node_id| match &self.nodes[node_id].kind {
                            HNodeKind::ResourceState {
                                resource: candidate,
                                version,
                            } if candidate == &resource => Some((*node_id, *version)),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    if outputs.len() != 1 || outputs[0].1 != frontier.version + 1 {
                        return Err(format!(
                            "operation {} {:?} write does not produce version {}",
                            plan_node.0,
                            resource,
                            frontier.version + 1
                        ));
                    }
                    frontier.last_write = Some(outputs[0].0);
                    frontier.version += 1;
                    frontier.open_reads.clear();
                } else {
                    frontier.open_reads.insert(completion);
                }
            }
        }
        Ok(())
    }

    fn validate_executable_acyclicity(&self) -> Result<(), String> {
        let edge_to_plan = self
            .op_map
            .values()
            .map(|info| (info.edge, info.plan_node))
            .collect::<HashMap<_, _>>();
        let mut indegree = self
            .op_map
            .keys()
            .copied()
            .map(|plan_node| (plan_node, 0usize))
            .collect::<BTreeMap<_, _>>();
        let mut successors: BTreeMap<PlanNodeId, BTreeSet<PlanNodeId>> = BTreeMap::new();

        for info in self.op_map.values() {
            let mut dependencies = BTreeSet::new();
            for input in &info.inputs {
                let Some(producer) = self.nodes.get(input).and_then(|node| node.producer) else {
                    continue;
                };
                let predecessor = edge_to_plan.get(&producer).copied().ok_or_else(|| {
                    format!(
                        "operation {} input {} has producer edge {} with no operation",
                        info.plan_node.0, input.0, producer.0
                    )
                })?;
                dependencies.insert(predecessor);
            }
            for predecessor in dependencies {
                successors
                    .entry(predecessor)
                    .or_default()
                    .insert(info.plan_node);
                *indegree
                    .get_mut(&info.plan_node)
                    .expect("every operation has indegree storage") += 1;
            }
        }

        let mut ready = indegree
            .iter()
            .filter_map(|(plan_node, degree)| (*degree == 0).then_some(*plan_node))
            .collect::<BTreeSet<_>>();
        let mut scheduled = 0usize;
        while let Some(plan_node) = ready.pop_first() {
            scheduled += 1;
            if let Some(next) = successors.get(&plan_node) {
                for successor in next {
                    let degree = indegree
                        .get_mut(successor)
                        .expect("successor operation has indegree storage");
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(*successor);
                    }
                }
            }
        }

        if scheduled != self.op_map.len() {
            return Err(format!(
                "executable dependency graph contains a cycle: scheduled {scheduled} of {} operations",
                self.op_map.len()
            ));
        }
        Ok(())
    }
}

fn resource_versions(graph: &HGraph, nodes: &[NodeId], resource: &ResourceKey) -> Vec<u64> {
    nodes
        .iter()
        .filter_map(|node| match &graph.nodes.get(node)?.kind {
            HNodeKind::ResourceState {
                resource: candidate,
                version,
            } if candidate == resource => Some(*version),
            _ => None,
        })
        .collect()
}

fn resource_keys(graph: &HGraph, nodes: &[NodeId]) -> BTreeSet<ResourceKey> {
    nodes
        .iter()
        .filter_map(|node| match &graph.nodes.get(node)?.kind {
            HNodeKind::ResourceState { resource, .. } => Some(resource.clone()),
            _ => None,
        })
        .collect()
}

fn format_node_ids(nodes: &[NodeId]) -> String {
    nodes
        .iter()
        .map(|node| format!("n{}", node.0))
        .collect::<Vec<_>>()
        .join(",")
}

fn require_hosted_project_operation(
    summary: &EffectSummary,
    plan_node: PlanNodeId,
    must_spawn: bool,
) -> Result<(), String> {
    if !summary.unknown
        || summary.deterministic
        || summary.fallibility != Fallibility::MayFail
        || summary.spawn != must_spawn
    {
        return Err(format!(
            "hosted project operation {} must remain unknown, nondeterministic, fallible, and spawn={must_spawn}",
            plan_node.0
        ));
    }
    require_read_write(summary, &ResourceKey::HostWorld, plan_node)
}

fn require_read_write(
    summary: &EffectSummary,
    resource: &ResourceKey,
    plan_node: PlanNodeId,
) -> Result<(), String> {
    if summary.reads.contains(resource) && summary.writes.contains(resource) {
        Ok(())
    } else {
        Err(format!(
            "operation {} must conservatively read and write {:?}",
            plan_node.0, resource
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::effects::EffectSummary;

    use super::*;

    fn add_pure_operation(
        graph: &mut HGraph,
        plan_node: PlanNodeId,
        inputs: Vec<NodeId>,
    ) -> (NodeId, NodeId, EdgeId) {
        let value = graph.add_node(HNode::fresh());
        let completion = graph.add_completion_node(plan_node).unwrap();
        graph.set_effect_summary(plan_node, EffectSummary::pure());
        let edge = graph
            .add_exec_edge(
                plan_node,
                ExecutableOp::Store,
                inputs,
                vec![value, completion],
                value,
                plan_node.0 as u64,
            )
            .unwrap();
        (value, completion, edge)
    }

    #[test]
    fn multi_output_operation_wires_value_completion_and_resource_state() {
        let mut graph = HGraph::default();
        let plan_node = PlanNodeId(0);
        let host_initial = graph.add_node(HNode::resource_state(ResourceKey::HostWorld, 0));
        let host_successor = graph.add_node(HNode::resource_state(ResourceKey::HostWorld, 1));
        let evaluator_initial =
            graph.add_node(HNode::resource_state(ResourceKey::EvaluatorState, 0));
        let evaluator_successor =
            graph.add_node(HNode::resource_state(ResourceKey::EvaluatorState, 1));
        let value = graph.add_node(HNode::fresh());
        let completion = graph.add_completion_node(plan_node).unwrap();
        graph.set_effect_summary(plan_node, EffectSummary::conservative_evaluator());

        let edge = graph
            .add_exec_edge(
                plan_node,
                ExecutableOp::EvalBackend {
                    lang: "python".into(),
                    env: u32::MAX,
                },
                vec![host_initial, evaluator_initial],
                vec![value, completion, host_successor, evaluator_successor],
                value,
                0,
            )
            .unwrap();

        graph.validate_execution_graph().unwrap();
        let info = graph.op_for(plan_node).unwrap();
        assert_eq!(info.value_output, value);
        assert_eq!(
            info.outputs,
            vec![value, completion, host_successor, evaluator_successor]
        );
        assert_eq!(graph.node(host_initial).unwrap().consumers, vec![edge]);
        assert_eq!(graph.node(evaluator_initial).unwrap().consumers, vec![edge]);
        for output in [value, completion, host_successor, evaluator_successor] {
            assert_eq!(graph.node(output).unwrap().producer, Some(edge));
        }
    }

    #[test]
    fn add_exec_edge_rejects_malformed_output_sets() {
        let mut graph = HGraph::default();
        let value = graph.add_node(HNode::fresh());
        let completion = graph.add_completion_node(PlanNodeId(0)).unwrap();

        let duplicate = graph
            .add_exec_edge(
                PlanNodeId(0),
                ExecutableOp::Store,
                vec![],
                vec![value, value, completion],
                value,
                0,
            )
            .unwrap_err();
        assert!(duplicate.contains("repeats output"), "{duplicate}");

        let synthetic_value = graph
            .add_exec_edge(
                PlanNodeId(0),
                ExecutableOp::Store,
                vec![],
                vec![value, completion],
                completion,
                0,
            )
            .unwrap_err();
        assert!(
            synthetic_value.contains("not an ordinary Value"),
            "{synthetic_value}"
        );

        let missing = graph
            .add_exec_edge(
                PlanNodeId(0),
                ExecutableOp::Store,
                vec![],
                vec![NodeId(999)],
                NodeId(999),
                0,
            )
            .unwrap_err();
        assert!(missing.contains("missing output"), "{missing}");

        let materialized = graph.add_node(HNode::with_value(OValue::str_("premature")));
        let error = graph
            .add_exec_edge(
                PlanNodeId(0),
                ExecutableOp::Store,
                vec![],
                vec![materialized, completion],
                materialized,
                0,
            )
            .unwrap_err();
        assert!(error.contains("must start unresolved"), "{error}");
    }

    #[test]
    fn validation_rejects_forged_pure_persistent_backend_summary() {
        let mut graph = HGraph::default();
        let plan_node = PlanNodeId(0);
        let value = graph.add_node(HNode::fresh());
        let completion = graph.add_completion_node(plan_node).unwrap();
        graph.set_effect_summary(plan_node, EffectSummary::pure());
        graph
            .add_exec_edge(
                plan_node,
                ExecutableOp::EvalBackend {
                    lang: "python".into(),
                    env: 7,
                },
                vec![],
                vec![value, completion],
                value,
                0,
            )
            .unwrap();

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(error.contains("must remain unknown"), "{error}");
    }

    #[test]
    fn source_plan_validation_detects_omitted_sequence_dependency() {
        let program = crate::ir::OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "left".into(),
                    expr: Box::new(OIr::Text("one".into())),
                },
                OIr::Store {
                    name: "right".into(),
                    expr: Box::new(OIr::Text("two".into())),
                },
            ],
        };
        let mut graph = program.hgraph();
        let dependency = graph
            .sequence_dependencies
            .first()
            .cloned()
            .expect("effectful stores preserve source sequence");
        let successor_edge = graph.op_map[&dependency.successor].edge;

        graph
            .op_map
            .get_mut(&dependency.successor)
            .unwrap()
            .inputs
            .retain(|node| *node != dependency.completion);
        graph
            .exec_edges
            .get_mut(&successor_edge)
            .unwrap()
            .ports
            .retain(|port| !(port.node == dependency.completion && port.role == PortRole::Input));
        graph
            .nodes
            .get_mut(&dependency.completion)
            .unwrap()
            .consumers
            .retain(|edge| *edge != successor_edge);
        graph.sequence_dependencies.clear();

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(error.contains("missing completion input"), "{error}");
    }

    #[test]
    fn textual_execution_graph_exposes_directed_state_and_completion_ports() {
        let backend = crate::ir::BackendRegistry::global().interface_for("python");
        let program = crate::ir::OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "python".into(),
                env_id: 0,
                attr: None,
                backend,
                body: vec![OIr::Text("__oval_result__ = 1".into())],
            }],
        };
        let text = program.hgraph().to_execution_text();

        assert!(text.contains("ResourceState(HostWorld@0)"), "{text}");
        assert!(text.contains("ResourceState(actor:python[0]@0)"), "{text}");
        assert!(text.contains("Completion("), "{text}");
        assert!(text.contains("inputs=[") && text.contains(" -> outputs=["));
    }

    #[test]
    fn add_exec_edge_rejects_a_second_producer() {
        let mut graph = HGraph::default();
        let first = PlanNodeId(0);
        let second = PlanNodeId(1);
        let shared_value = graph.add_node(HNode::fresh());
        let first_completion = graph.add_completion_node(first).unwrap();
        graph
            .add_exec_edge(
                first,
                ExecutableOp::Store,
                vec![],
                vec![shared_value, first_completion],
                shared_value,
                0,
            )
            .unwrap();
        let second_completion = graph.add_completion_node(second).unwrap();

        let error = graph
            .add_exec_edge(
                second,
                ExecutableOp::Store,
                vec![],
                vec![shared_value, second_completion],
                shared_value,
                1,
            )
            .unwrap_err();
        assert!(error.contains("already has producer"), "{error}");
    }

    #[test]
    fn validation_rejects_missing_completion_output() {
        let mut graph = HGraph::default();
        let plan_node = PlanNodeId(0);
        let value = graph.add_node(HNode::fresh());
        graph.set_effect_summary(plan_node, EffectSummary::pure());
        graph
            .add_exec_edge(
                plan_node,
                ExecutableOp::Store,
                vec![],
                vec![value],
                value,
                0,
            )
            .unwrap();

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(error.contains("Completion output"), "{error}");
    }

    #[test]
    fn validation_rejects_non_monotonic_resource_transition() {
        let mut graph = HGraph::default();
        let plan_node = PlanNodeId(0);
        let initial = graph.add_node(HNode::resource_state(ResourceKey::HostWorld, 0));
        let skipped = graph.add_node(HNode::resource_state(ResourceKey::HostWorld, 2));
        let value = graph.add_node(HNode::fresh());
        let completion = graph.add_completion_node(plan_node).unwrap();
        graph.set_effect_summary(plan_node, EffectSummary::unknown());
        graph
            .add_exec_edge(
                plan_node,
                ExecutableOp::Store,
                vec![initial],
                vec![value, completion, skipped],
                value,
                0,
            )
            .unwrap();

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(error.contains("must advance"), "{error}");
    }

    #[test]
    fn validation_rejects_unconsumed_sequence_completion() {
        let mut graph = HGraph::default();
        let predecessor = PlanNodeId(0);
        let successor = PlanNodeId(1);
        let (_, predecessor_completion, _) = add_pure_operation(&mut graph, predecessor, vec![]);
        add_pure_operation(&mut graph, successor, vec![]);
        graph
            .record_sequence_dependency(predecessor, successor, predecessor_completion)
            .unwrap();

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(
            error.contains("not implemented by predecessor completion"),
            "{error}"
        );
    }

    #[test]
    fn validation_rejects_executable_dependency_cycles() {
        let mut graph = HGraph::default();
        let first = PlanNodeId(0);
        let second = PlanNodeId(1);
        let first_value = graph.add_node(HNode::fresh());
        let second_value = graph.add_node(HNode::fresh());
        let first_completion = graph.add_completion_node(first).unwrap();
        let second_completion = graph.add_completion_node(second).unwrap();
        graph.set_effect_summary(first, EffectSummary::pure());
        graph.set_effect_summary(second, EffectSummary::pure());
        graph
            .add_exec_edge(
                first,
                ExecutableOp::Store,
                vec![second_value],
                vec![first_value, first_completion],
                first_value,
                0,
            )
            .unwrap();
        graph
            .add_exec_edge(
                second,
                ExecutableOp::Store,
                vec![first_value],
                vec![second_value, second_completion],
                second_value,
                1,
            )
            .unwrap();

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(error.contains("contains a cycle"), "{error}");
    }

    #[test]
    fn validation_rejects_an_unmapped_executable_edge() {
        let mut graph = HGraph::default();
        add_pure_operation(&mut graph, PlanNodeId(0), vec![]);
        graph.op_map.clear();

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(error.contains("has no plan-operation mapping"), "{error}");
    }

    #[test]
    fn validation_rejects_value_metadata_on_synthetic_nodes() {
        let mut graph = HGraph::default();
        let synthetic = graph.add_node(HNode::resource_state(ResourceKey::HostWorld, 0));
        graph.node_mut(synthetic).unwrap().domain = DomainFlags::ANY;

        let error = graph.validate_execution_graph().unwrap_err();
        assert!(error.contains("ordinary value/type metadata"), "{error}");
    }

    #[test]
    fn solver_rejects_synthetic_dataflow_outputs() {
        let mut graph = HGraph::default();
        let input = graph.add_node(HNode::with_value(OValue::str_("value")));
        let synthetic = graph.add_node(HNode::resource_state(ResourceKey::HostWorld, 0));
        let edge = graph.add_edge(HEdge::constraint(
            OpKind::DataFlow,
            vec![
                Port {
                    node: input,
                    role: PortRole::Input,
                },
                Port {
                    node: synthetic,
                    role: PortRole::Output,
                },
            ],
        ));

        let error = crate::hgraph::solve::solve_types(&mut graph).unwrap_err();
        assert_eq!(
            error,
            crate::hgraph::solve::SolveError::InvalidDataFlowShape {
                edge,
                value_inputs: 1,
                value_outputs: 0,
                non_value_ports: 1,
            }
        );
        let node = graph.node(synthetic).unwrap();
        assert!(node.value.is_none());
        assert!(node.domain.is_empty());
        assert!(node.rep.is_empty());
    }
}
