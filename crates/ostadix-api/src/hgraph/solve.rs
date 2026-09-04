use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
};

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::backend_catalog::{
    BackendRegistry, BackendValueCapabilities, IntegerExactness, RichNumberPreservation,
};
use crate::backend_morphism::{shadow_assess_backend_morphism_v1, BackendMorphismAssessmentV1};
use crate::value::{AnnotationKind, Fidelity, FidelityAssessmentV2, ONumber, OValue};

use super::{
    graph::{EdgeId, HEdge, HGraph, HNode, HNodeKind, NodeId, PortRole},
    kinds::{DomainFlags, OpKind, RepFlags},
};

/// Bounded evidence explaining where a solver convergence budget ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct BudgetDiagnostics {
    pub completed_passes: usize,
    pub slot_updates: usize,
    pub derived_pass_bound: usize,
    pub applied_pass_limit: usize,
    pub limit_is_below_derived_bound: bool,
    pub last_changed_edge: Option<EdgeId>,
    pub last_changed_node: Option<NodeId>,
    pub last_changed_slot: Option<&'static str>,
    pub last_before: Option<Box<str>>,
    pub last_after: Option<Box<str>>,
    pub recent_changed_edges: Vec<EdgeId>,
}

impl fmt::Display for BudgetDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} completed pass(es), {} strict slot update(s), derived bound {}, applied limit {}",
            self.completed_passes,
            self.slot_updates,
            self.derived_pass_bound,
            self.applied_pass_limit
        )?;
        if self.limit_is_below_derived_bound {
            write!(formatter, ", applied limit is below the derived bound")?;
        }
        write!(
            formatter,
            ", last change edge {:?} node {:?} slot {:?}: {:?} -> {:?}, recent changing edges {:?}",
            self.last_changed_edge,
            self.last_changed_node,
            self.last_changed_slot,
            self.last_before,
            self.last_after,
            self.recent_changed_edges
        )
    }
}

/// A type/fidelity solver failure that prevents returning a solved graph.
#[derive(Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum SolveError {
    #[error("value node {node:?} has invalid fidelity bounds: {reason}")]
    InvalidFidelityAssessment { node: NodeId, reason: String },
    #[error(
        "value node {node:?} has a legacy fidelity projection inconsistent with its V2 assessment"
    )]
    InconsistentFidelityProjection { node: NodeId },
    #[error(
        "DataFlow edge {edge:?} must have exactly one ordinary Value input and at least one ordinary Value output (found {value_inputs} input(s), {value_outputs} output(s), and {non_value_ports} missing or non-Value port(s))"
    )]
    InvalidDataFlowShape {
        edge: EdgeId,
        value_inputs: usize,
        value_outputs: usize,
        non_value_ports: usize,
    },
    #[error("DataFlow edge {edge:?} repeats destination node {node:?}")]
    DuplicateDataFlowDestination { edge: EdgeId, node: NodeId },
    #[error(
        "DataFlow destination node {node:?} has unsupported multiple producers {first:?} and {second:?}"
    )]
    MultipleDataFlowProducers {
        node: NodeId,
        first: EdgeId,
        second: EdgeId,
    },
    #[error(
        "constraint edge {edge:?} gives node {node:?} a materialized value that conflicts with its existing value"
    )]
    ConflictingMaterializedValue {
        edge: EdgeId,
        node: NodeId,
        existing: Box<OValue>,
        incoming: Box<OValue>,
    },
    #[error("HGraph type solver exhausted its convergence budget: {0}")]
    BudgetExhausted(Box<BudgetDiagnostics>),
}

const GENERATED_FIDELITY_KINDS: usize = 5;
const MAX_SOLVE_PASSES: usize = 1_000_000;
const RECENT_CHANGED_EDGE_LIMIT: usize = 16;

/// Solve type and fidelity constraints to a monotone fixed point.
///
/// `DataFlow` is directional compatibility, not equality: one source refines
/// each destination by domain/representation meet, fidelity join, and a
/// checked write-once materialized value. A preflight rejects malformed shapes
/// and multiple `DataFlow` producers for one destination before any facts
/// mutate; any existing materialized value must equal the incoming value.
///
/// The derived budget bounds every descending domain/representation bit,
/// write-once value, and ascending fidelity transition. The hard ceiling places
/// an absolute finite bound on whole-graph passes even for adversarially large
/// public graphs. Exhausting that ceiling fails closed; it does not assert that
/// a mathematical fixed point cannot exist.
///
/// Any error may leave `graph` partially refined; callers must discard it.
pub fn solve_types(graph: &mut HGraph) -> Result<(), SolveError> {
    let derived_pass_bound = derived_iteration_budget(graph);
    solve_types_with_limits(
        graph,
        derived_pass_bound,
        derived_pass_bound.min(MAX_SOLVE_PASSES),
    )
}

#[cfg(test)]
pub(super) fn solve_types_with_budget(graph: &mut HGraph, budget: usize) -> Result<(), SolveError> {
    solve_types_with_limits(graph, derived_iteration_budget(graph), budget)
}

fn solve_types_with_limits(
    graph: &mut HGraph,
    derived_pass_bound: usize,
    applied_pass_limit: usize,
) -> Result<(), SolveError> {
    validate_fidelity_assessments(graph)?;
    validate_dataflow_constraints(graph)?;
    hydrate_fidelity_assessments(graph);
    let mut trace = SolveTrace::default();
    let mut completed_passes = 0;
    for phase in [SolvePhase::TypeAndValue, SolvePhase::Fidelity] {
        loop {
            if completed_passes == applied_pass_limit {
                return Err(budget_exhausted(
                    &trace,
                    completed_passes,
                    derived_pass_bound,
                    applied_pass_limit,
                ));
            }
            completed_passes += 1;

            let mut changed = false;
            for eid in graph.edge_ids() {
                trace.begin_edge(eid);
                let updates_before = trace.slot_updates;
                let edge_changed = propagate(graph, eid, phase, &mut trace)?;
                debug_assert_eq!(edge_changed, trace.slot_updates != updates_before);
                changed |= edge_changed;
            }
            if !changed {
                break;
            }
        }
    }
    Ok(())
}

fn validate_fidelity_assessments(graph: &HGraph) -> Result<(), SolveError> {
    for (node, value) in &graph.nodes {
        if let Some(assessment) = &value.fidelity_assessment {
            assessment
                .validate()
                .map_err(|error| SolveError::InvalidFidelityAssessment {
                    node: *node,
                    reason: error.to_string(),
                })?;
            let projected = assessment.try_possible_fidelity().map_err(|error| {
                SolveError::InvalidFidelityAssessment {
                    node: *node,
                    reason: error.to_string(),
                }
            })?;
            if value.fidelity.as_ref() != Some(&projected) {
                return Err(SolveError::InconsistentFidelityProjection { node: *node });
            }
        }
    }
    Ok(())
}

fn hydrate_fidelity_assessments(graph: &mut HGraph) {
    for node in graph.nodes.values_mut().filter(|node| node.is_value()) {
        if node.fidelity_assessment.is_none() {
            node.fidelity_assessment = node
                .fidelity
                .clone()
                .map(FidelityAssessmentV2::from_concrete);
        }
    }
}

fn budget_exhausted(
    trace: &SolveTrace,
    completed_passes: usize,
    derived_pass_bound: usize,
    applied_pass_limit: usize,
) -> SolveError {
    let (last_changed_edge, last_changed_node, last_changed_slot, last_before, last_after) = trace
        .last_change
        .clone()
        .map(|change| {
            (
                Some(change.edge),
                Some(change.node),
                Some(change.slot),
                Some(change.before.describe()),
                Some(change.after.describe()),
            )
        })
        .unwrap_or((None, None, None, None, None));
    SolveError::BudgetExhausted(Box::new(BudgetDiagnostics {
        completed_passes,
        slot_updates: trace.slot_updates,
        derived_pass_bound,
        applied_pass_limit,
        limit_is_below_derived_bound: applied_pass_limit < derived_pass_bound,
        last_changed_edge,
        last_changed_node,
        last_changed_slot,
        last_before,
        last_after,
        recent_changed_edges: trace.recent_changed_edges.iter().copied().collect(),
    }))
}

fn validate_dataflow_constraints(graph: &HGraph) -> Result<(), SolveError> {
    let mut destination_producers = BTreeMap::<NodeId, EdgeId>::new();

    for edge_id in graph.edge_ids() {
        let Some(edge) = graph.edge(edge_id) else {
            continue;
        };
        if !matches!(edge.kind, OpKind::DataFlow) {
            continue;
        }

        let mut value_inputs = 0;
        let mut value_outputs = Vec::new();
        let mut non_value_ports = 0;
        for port in &edge.ports {
            let is_value = graph.node(port.node).is_some_and(HNode::is_value);
            if !is_value {
                non_value_ports += 1;
                continue;
            }
            if matches!(port.role, PortRole::Input | PortRole::InOut) {
                value_inputs += 1;
            }
            if matches!(port.role, PortRole::Output | PortRole::InOut) {
                value_outputs.push(port.node);
            }
        }

        if value_inputs != 1 || value_outputs.is_empty() || non_value_ports != 0 {
            return Err(SolveError::InvalidDataFlowShape {
                edge: edge_id,
                value_inputs,
                value_outputs: value_outputs.len(),
                non_value_ports,
            });
        }

        let mut unique_outputs = BTreeSet::new();
        for destination in value_outputs {
            if !unique_outputs.insert(destination) {
                return Err(SolveError::DuplicateDataFlowDestination {
                    edge: edge_id,
                    node: destination,
                });
            }
        }
        for destination in unique_outputs {
            if let Some(first) = destination_producers.insert(destination, edge_id) {
                if first != edge_id {
                    return Err(SolveError::MultipleDataFlowProducers {
                        node: destination,
                        first,
                        second: edge_id,
                    });
                }
            }
        }
    }

    Ok(())
}

fn derived_iteration_budget(graph: &HGraph) -> usize {
    let mut fidelity_kinds = BTreeSet::new();
    for node in graph.nodes.values() {
        if let Some(Fidelity::Structural { lost }) = &node.fidelity {
            fidelity_kinds.extend(lost.iter());
        }
        if let Some(FidelityAssessmentV2::Structural { definite, possible }) =
            &node.fidelity_assessment
        {
            fidelity_kinds.extend(possible.iter());
            if let Some(definite) = definite {
                fidelity_kinds.extend(definite.iter());
            }
        }
    }
    let existing_fidelity_kinds = fidelity_kinds.len();
    // These public bitflags retain unknown bits, so count the full underlying
    // storage width rather than only today's named ANY masks.
    let domain_height = u16::BITS as usize;
    let rep_height = u16::BITS as usize;
    // None -> a concrete fidelity, each distinct structural loss, then the
    // NativeCapsule and Unsupported top states.
    let fidelity_height = existing_fidelity_kinds
        .saturating_add(GENERATED_FIDELITY_KINDS)
        .saturating_add(3);
    let per_node_height = domain_height
        .saturating_add(rep_height)
        .saturating_add(fidelity_height.saturating_mul(2))
        .saturating_add(1); // value: None -> Some

    graph
        .nodes
        .len()
        .saturating_mul(per_node_height)
        .saturating_add(2) // one final no-change pass for each solver phase
        .max(1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SolvePhase {
    TypeAndValue,
    Fidelity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LastChange {
    edge: EdgeId,
    node: NodeId,
    slot: &'static str,
    before: SlotSnapshot,
    after: SlotSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SlotSnapshot {
    Domain(u16),
    Representation(u16),
    Fidelity(FidelitySnapshot),
    FidelityAssessment {
        definite_count: usize,
        possible_count: usize,
        possible_fingerprint: u64,
    },
    ValueMaterialized(bool),
}

impl SlotSnapshot {
    fn describe(self) -> Box<str> {
        match self {
            Self::Domain(bits) => format!("domain bits {bits:#06x}").into_boxed_str(),
            Self::Representation(bits) => {
                format!("representation bits {bits:#06x}").into_boxed_str()
            }
            Self::Fidelity(fidelity) => fidelity.describe(),
            Self::FidelityAssessment {
                definite_count,
                possible_count,
                possible_fingerprint,
            } => format!(
                "fidelity bounds with {definite_count} definite and {possible_count} possible loss kind(s), possible fingerprint {possible_fingerprint:016x}"
            )
            .into_boxed_str(),
            Self::ValueMaterialized(materialized) => {
                if materialized {
                    "materialized".into()
                } else {
                    "unmaterialized".into()
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FidelitySnapshot {
    None,
    Lossless,
    Structural {
        loss_count: usize,
        loss_fingerprint: u64,
    },
    NativeCapsule,
    Unsupported,
}

impl FidelitySnapshot {
    fn describe(self) -> Box<str> {
        match self {
            Self::None => "no fidelity".into(),
            Self::Lossless => "lossless".into(),
            Self::Structural {
                loss_count,
                loss_fingerprint,
            } => format!(
                "structural fidelity with {loss_count} loss kind(s), fingerprint {loss_fingerprint:016x}"
            )
            .into_boxed_str(),
            Self::NativeCapsule => "native capsule".into(),
            Self::Unsupported => "unsupported".into(),
        }
    }
}

#[derive(Default)]
struct SolveTrace {
    slot_updates: usize,
    last_change: Option<LastChange>,
    recent_changed_edges: VecDeque<EdgeId>,
    current_edge: Option<EdgeId>,
    current_edge_had_change: bool,
}

impl SolveTrace {
    fn begin_edge(&mut self, edge: EdgeId) {
        self.current_edge = Some(edge);
        self.current_edge_had_change = false;
    }

    fn record(
        &mut self,
        node: NodeId,
        slot: &'static str,
        before: SlotSnapshot,
        after: SlotSnapshot,
    ) {
        let edge = self
            .current_edge
            .expect("solver slot update must occur while propagating an edge");
        self.slot_updates = self.slot_updates.saturating_add(1);
        if !self.current_edge_had_change {
            if self.recent_changed_edges.len() == RECENT_CHANGED_EDGE_LIMIT {
                self.recent_changed_edges.pop_front();
            }
            self.recent_changed_edges.push_back(edge);
            self.current_edge_had_change = true;
        }
        self.last_change = Some(LastChange {
            edge,
            node,
            slot,
            before,
            after,
        });
    }
}

#[derive(Clone, Copy, Debug)]
enum SlotDirection {
    DescendingMeet,
    AscendingJoin,
    WriteOnce,
}

trait SolverSlot: Clone + PartialEq {
    const DIRECTION: SlotDirection;
    const SLOT_NAME: &'static str;

    fn merge(&self, incoming: Self) -> Self;
    fn permits(&self, next: &Self) -> bool;
    fn snapshot(&self) -> SlotSnapshot;
}

impl SolverSlot for DomainFlags {
    const DIRECTION: SlotDirection = SlotDirection::DescendingMeet;
    const SLOT_NAME: &'static str = "domain";

    fn merge(&self, incoming: Self) -> Self {
        *self & incoming
    }

    fn permits(&self, next: &Self) -> bool {
        self.contains(*next)
    }

    fn snapshot(&self) -> SlotSnapshot {
        SlotSnapshot::Domain(self.bits())
    }
}

impl SolverSlot for RepFlags {
    const DIRECTION: SlotDirection = SlotDirection::DescendingMeet;
    const SLOT_NAME: &'static str = "representation";

    fn merge(&self, incoming: Self) -> Self {
        *self & incoming
    }

    fn permits(&self, next: &Self) -> bool {
        self.contains(*next)
    }

    fn snapshot(&self) -> SlotSnapshot {
        SlotSnapshot::Representation(self.bits())
    }
}

impl SolverSlot for Option<Fidelity> {
    const DIRECTION: SlotDirection = SlotDirection::AscendingJoin;
    const SLOT_NAME: &'static str = "fidelity";

    fn merge(&self, incoming: Self) -> Self {
        match (self.clone(), incoming) {
            (Some(existing), Some(incoming)) => Some(existing.compose(incoming)),
            (None, incoming) => incoming,
            (existing, None) => existing,
        }
    }

    fn permits(&self, next: &Self) -> bool {
        self.merge(next.clone()) == *next
    }

    fn snapshot(&self) -> SlotSnapshot {
        let fidelity = match self {
            None => FidelitySnapshot::None,
            Some(Fidelity::Lossless) => FidelitySnapshot::Lossless,
            Some(Fidelity::Structural { lost }) => FidelitySnapshot::Structural {
                loss_count: lost.len(),
                loss_fingerprint: fidelity_loss_fingerprint(lost.as_set()),
            },
            Some(Fidelity::NativeCapsule) => FidelitySnapshot::NativeCapsule,
            Some(Fidelity::Unsupported) => FidelitySnapshot::Unsupported,
        };
        SlotSnapshot::Fidelity(fidelity)
    }
}

impl SolverSlot for Option<FidelityAssessmentV2> {
    const DIRECTION: SlotDirection = SlotDirection::AscendingJoin;
    const SLOT_NAME: &'static str = "fidelity-assessment-v2";

    fn merge(&self, incoming: Self) -> Self {
        match (self.clone(), incoming) {
            (Some(existing), Some(incoming)) => Some(existing.then(incoming)),
            (None, incoming) => incoming,
            (existing, None) => existing,
        }
    }

    fn permits(&self, next: &Self) -> bool {
        self.merge(next.clone()) == *next
    }

    fn snapshot(&self) -> SlotSnapshot {
        let (definite_count, possible_count, possible_fingerprint) = match self {
            None | Some(FidelityAssessmentV2::Lossless) => (0, 0, 0),
            Some(FidelityAssessmentV2::Structural { definite, possible }) => (
                definite.as_ref().map_or(0, |losses| losses.len()),
                possible.len(),
                fidelity_loss_fingerprint(possible.as_set()),
            ),
            Some(FidelityAssessmentV2::NativeCapsule) => (0, usize::MAX - 1, 0),
            Some(FidelityAssessmentV2::Unsupported) => (0, usize::MAX, 0),
        };
        SlotSnapshot::FidelityAssessment {
            definite_count,
            possible_count,
            possible_fingerprint,
        }
    }
}

impl SolverSlot for Option<OValue> {
    const DIRECTION: SlotDirection = SlotDirection::WriteOnce;
    const SLOT_NAME: &'static str = "value";

    fn merge(&self, incoming: Self) -> Self {
        debug_assert!(
            self.is_none() || incoming.is_none() || self == &incoming,
            "conflicting materialized value bypassed checked write-once update"
        );
        self.clone().or(incoming)
    }

    fn permits(&self, next: &Self) -> bool {
        self.is_none() || self == next
    }

    fn snapshot(&self) -> SlotSnapshot {
        SlotSnapshot::ValueMaterialized(self.is_some())
    }
}

fn update_slot<T: SolverSlot>(
    slot: &mut T,
    incoming: T,
    trace: &mut SolveTrace,
    node: NodeId,
) -> bool {
    let next = slot.merge(incoming);
    if *slot == next {
        return false;
    }
    debug_assert!(
        slot.permits(&next),
        "solver update violated {:?} slot direction",
        T::DIRECTION
    );
    trace.record(node, T::SLOT_NAME, slot.snapshot(), next.snapshot());
    *slot = next;
    true
}

fn fidelity_loss_fingerprint(lost: &BTreeSet<AnnotationKind>) -> u64 {
    let encoded = serde_json::to_vec(lost)
        .expect("serializing an AnnotationKind set into memory cannot fail");
    let digest = Sha256::digest(encoded);
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("a SHA-256 digest always has an eight-byte prefix"),
    )
}

fn propagate(
    graph: &mut HGraph,
    eid: EdgeId,
    phase: SolvePhase,
    trace: &mut SolveTrace,
) -> Result<bool, SolveError> {
    let Some(edge) = graph.edge(eid).cloned() else {
        return Ok(false);
    };

    if phase == SolvePhase::Fidelity {
        return match &edge.kind {
            OpKind::BackendCrossing { from_lang, to_lang } => {
                let fidelity = input_value_nodes(graph, &edge)
                    .next()
                    .map(|node| fidelity_assessment_for(node, from_lang, to_lang))
                    .unwrap_or(FidelityAssessmentV2::Unsupported);
                Ok(apply_fidelity_to_outputs(graph, &edge, fidelity, trace))
            }
            OpKind::DataFlow => Ok(propagate_dataflow_fidelity(graph, &edge, trace)),
            _ => Ok(false),
        };
    }

    let changed = match &edge.kind {
        OpKind::Additive | OpKind::Multiplicative => {
            let intersection = edge
                .ports
                .iter()
                .filter_map(|p| value_node(graph, p.node))
                .fold(DomainFlags::NUMERIC, |acc, n| {
                    acc & n.domain & DomainFlags::NUMERIC
                });
            apply_domain_to_all(graph, &edge, intersection, trace)
        }
        OpKind::Bitwise => apply_domain_to_all(
            graph,
            &edge,
            DomainFlags::INTEGER | DomainFlags::BITFIELD,
            trace,
        ),
        OpKind::Ordered => {
            let mut changed = apply_domain_to_inputs(
                graph,
                &edge,
                DomainFlags::NUMERIC | DomainFlags::BOOL,
                trace,
            );
            changed |= apply_domain_to_outputs(graph, &edge, DomainFlags::BOOL, trace);
            changed |= apply_rep_to_outputs(graph, &edge, RepFlags::BOOL, trace);
            changed
        }
        OpKind::Bounded { value } => {
            let mut changed = apply_domain_to_outputs(graph, &edge, DomainFlags::INTEGER, trace);
            changed |= apply_rep_to_outputs(graph, &edge, min_rep_for_bigint(value), trace);
            changed |= materialize_bounded_outputs(graph, &edge, value, trace)?;
            changed
        }
        OpKind::AbiFixed { dom, rep } => {
            let mut changed = false;
            for port in &edge.ports {
                if let Some(node) = value_node_mut(graph, port.node) {
                    changed |= update_slot(&mut node.domain, *dom, trace, port.node);
                    changed |= update_slot(&mut node.rep, *rep, trace, port.node);
                }
            }
            changed
        }
        OpKind::FieldAccess { .. } => {
            apply_domain_to_inputs(graph, &edge, DomainFlags::STRUCT, trace)
        }
        OpKind::Dereferenceable => apply_domain_to_all(graph, &edge, DomainFlags::POINTER, trace),
        OpKind::BackendCrossing { .. } => false,
        OpKind::DataFlow => propagate_dataflow_type_value(graph, &edge, trace)?,
        OpKind::StructuralBarrier
        | OpKind::Sequence
        | OpKind::ActorSerial { .. }
        | OpKind::Batch
        | OpKind::All
        | OpKind::Any
        | OpKind::Race
        | OpKind::Request { .. }
        | OpKind::Schedule { .. }
        | OpKind::CacheMemo { .. }
        | OpKind::X86 { .. }
        | OpKind::OcoreOp { .. } => false,
    };
    Ok(changed)
}

pub fn min_rep_for_bigint(value: &BigInt) -> RepFlags {
    match value.to_i64() {
        Some(n) if n >= i8::MIN as i64 && n <= i8::MAX as i64 => RepFlags::I8,
        Some(n) if n >= i16::MIN as i64 && n <= i16::MAX as i64 => RepFlags::I16,
        Some(n) if n >= i32::MIN as i64 && n <= i32::MAX as i64 => RepFlags::I32,
        Some(_) => RepFlags::I64,
        None => RepFlags::BIG,
    }
}

fn propagate_dataflow_type_value(
    graph: &mut HGraph,
    edge: &HEdge,
    trace: &mut SolveTrace,
) -> Result<bool, SolveError> {
    let Some(input) = input_value_nodes(graph, edge).next().cloned() else {
        return Ok(false);
    };
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(output) = value_node_mut(graph, nid) {
            changed |= update_slot(&mut output.domain, input.domain, trace, nid);
            changed |= update_slot(&mut output.rep, input.rep, trace, nid);
            changed |= write_value_once(output, edge.id, nid, input.value.clone(), trace)?;
        }
    }
    Ok(changed)
}

fn propagate_dataflow_fidelity(graph: &mut HGraph, edge: &HEdge, trace: &mut SolveTrace) -> bool {
    let Some(incoming) = input_value_nodes(graph, edge).next().and_then(|input| {
        input.fidelity_assessment.clone().or_else(|| {
            input
                .fidelity
                .clone()
                .map(FidelityAssessmentV2::from_concrete)
        })
    }) else {
        return false;
    };
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|port| matches!(port.role, PortRole::Output | PortRole::InOut))
        .map(|port| port.node)
        .collect::<Vec<_>>()
    {
        if let Some(output) = value_node_mut(graph, nid) {
            changed |= apply_fidelity_to_node(output, incoming.clone(), trace, nid);
        }
    }
    changed
}

fn write_value_once(
    node: &mut HNode,
    edge: EdgeId,
    node_id: NodeId,
    incoming: Option<OValue>,
    trace: &mut SolveTrace,
) -> Result<bool, SolveError> {
    let Some(incoming) = incoming else {
        return Ok(false);
    };
    if let Some(existing) = &node.value {
        if existing != &incoming {
            return Err(SolveError::ConflictingMaterializedValue {
                edge,
                node: node_id,
                existing: Box::new(existing.clone()),
                incoming: Box::new(incoming),
            });
        }
        return Ok(false);
    }
    Ok(update_slot(&mut node.value, Some(incoming), trace, node_id))
}

fn materialize_bounded_outputs(
    graph: &mut HGraph,
    edge: &HEdge,
    value: &BigInt,
    trace: &mut SolveTrace,
) -> Result<bool, SolveError> {
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(node) = value_node_mut(graph, nid) {
            let new_value = OValue::big_int(value.clone());
            changed |= write_value_once(node, edge.id, nid, Some(new_value), trace)?;
        }
    }
    Ok(changed)
}

pub fn fidelity_for(node: &HNode, from_lang: &str, to_lang: &str) -> Fidelity {
    if !node.is_value() {
        return Fidelity::Unsupported;
    }
    // Native values are process-bound: two evaluators with the same canonical
    // language name are not interchangeable processes, so the Native check
    // must precede the same-language shortcut.
    if matches!(node.value, Some(OValue::Native { .. })) {
        return Fidelity::NativeCapsule;
    }

    let registry = BackendRegistry::global();
    let Some(to_spec) = registry.get(to_lang) else {
        return Fidelity::Unsupported;
    };
    let same_backend = registry
        .get(from_lang)
        .is_some_and(|from_spec| from_spec.name == to_spec.name);
    if same_backend {
        return Fidelity::Lossless;
    }
    if let Some(value) = &node.value {
        return fidelity_for_value_with_capabilities(value, &to_spec.value_capabilities);
    }
    fidelity_for_abstract(node, &to_spec.value_capabilities)
}

pub fn fidelity_assessment_for(
    node: &HNode,
    from_lang: &str,
    to_lang: &str,
) -> FidelityAssessmentV2 {
    if !node.is_value() {
        return FidelityAssessmentV2::Unsupported;
    }
    if matches!(node.value, Some(OValue::Native { .. })) {
        return FidelityAssessmentV2::NativeCapsule;
    }

    let registry = BackendRegistry::global();
    let Some(to_spec) = registry.get(to_lang) else {
        return FidelityAssessmentV2::Unsupported;
    };
    let same_backend = registry
        .get(from_lang)
        .is_some_and(|from_spec| from_spec.name == to_spec.name);
    if same_backend {
        return FidelityAssessmentV2::Lossless;
    }
    if let Some(value) = &node.value {
        return FidelityAssessmentV2::from_concrete(fidelity_for_value_with_capabilities(
            value,
            &to_spec.value_capabilities,
        ));
    }
    fidelity_assessment_for_abstract(node, &to_spec.value_capabilities)
}

pub fn fidelity_for_value(value: &OValue, to_lang: &str) -> Fidelity {
    let Some(spec) = BackendRegistry::global().get(to_lang) else {
        return Fidelity::Unsupported;
    };
    fidelity_for_value_with_capabilities(value, &spec.value_capabilities)
}

/// Return the bounded V1 morphism assessment beside the compatibility solver
/// result. This is deliberately shadow-only: callers can inspect divergences,
/// but it does not alter graph facts, evidence, admission, or dispatch.
pub fn backend_morphism_shadow_assessment_for_value(
    value: &OValue,
    to_lang: &str,
) -> Option<BackendMorphismAssessmentV1> {
    shadow_assess_backend_morphism_v1(to_lang, value)
}

pub(super) fn fidelity_for_value_with_capabilities(
    value: &OValue,
    capabilities: &BackendValueCapabilities,
) -> Fidelity {
    match value {
        OValue::Native { .. } => Fidelity::NativeCapsule,
        OValue::Number { v } => fidelity_for_number(v, capabilities),
        OValue::Graph { .. } => Fidelity::structural([AnnotationKind::Identity]),
        OValue::Capability { .. } => Fidelity::structural([AnnotationKind::Capability]),
        _ => Fidelity::Lossless,
    }
}

fn fidelity_for_number(number: &ONumber, capabilities: &BackendValueCapabilities) -> Fidelity {
    let mut lost = BTreeSet::new();

    if let ONumber::Int { v } = number {
        match integer_exceeds_capability(v, &capabilities.integer_exactness) {
            Some(true) => {
                lost.insert(AnnotationKind::NumericPrecision);
            }
            Some(false) => {}
            None => return Fidelity::Unsupported,
        }
    }

    match capabilities.rich_numbers {
        RichNumberPreservation::Preserved => {}
        RichNumberPreservation::Collapsed => {
            lost.insert(AnnotationKind::NumericExactness);
            lost.insert(AnnotationKind::TypeTag);
        }
        RichNumberPreservation::Unknown => return Fidelity::Unsupported,
    }

    Fidelity::structural(lost)
}

fn integer_exceeds_capability(value: &BigInt, exactness: &IntegerExactness) -> Option<bool> {
    integer_range_exceeds_capability(value, value, exactness)
}

fn integer_range_exceeds_capability(
    minimum: &BigInt,
    maximum: &BigInt,
    exactness: &IntegerExactness,
) -> Option<bool> {
    match exactness {
        IntegerExactness::Unknown => None,
        IntegerExactness::Arbitrary => Some(false),
        IntegerExactness::ExactMagnitudeBits(bits) => {
            let upper = BigInt::from(1_u8) << usize::from(*bits);
            let lower = -&upper;
            Some(minimum < &lower || maximum > &upper)
        }
        IntegerExactness::TwosComplementBits(bits) => {
            let magnitude = BigInt::from(1_u8) << usize::from(*bits);
            let lower = -&magnitude;
            let upper = &magnitude - 1_u8;
            Some(minimum < &lower || maximum > &upper)
        }
        IntegerExactness::ExactRange { min, max } => {
            if min > max {
                None
            } else {
                Some(minimum < min || maximum > max)
            }
        }
    }
}

pub(super) fn fidelity_for_abstract(
    node: &HNode,
    capabilities: &BackendValueCapabilities,
) -> Fidelity {
    if node.domain.is_empty() || node.rep.is_empty() {
        return Fidelity::Unsupported;
    }

    let numeric_reps = RepFlags::I8
        | RepFlags::I16
        | RepFlags::I32
        | RepFlags::I64
        | RepFlags::I128
        | RepFlags::BIG
        | RepFlags::F32
        | RepFlags::F64;
    let numeric_domain = node.domain & DomainFlags::NUMERIC;
    let numeric_rep = node.rep & numeric_reps;
    if !numeric_domain.is_empty() || !numeric_rep.is_empty() {
        if numeric_domain.is_empty()
            || numeric_rep.is_empty()
            || !node.domain.difference(DomainFlags::NUMERIC).is_empty()
            || !node.rep.difference(numeric_reps).is_empty()
        {
            return Fidelity::Unsupported;
        }

        let mut lost = BTreeSet::new();
        match abstract_integer_exceeds_capability(numeric_rep, &capabilities.integer_exactness) {
            Some(true) => {
                lost.insert(AnnotationKind::NumericPrecision);
            }
            Some(false) => {}
            None => return Fidelity::Unsupported,
        }
        match capabilities.rich_numbers {
            RichNumberPreservation::Preserved => {}
            RichNumberPreservation::Collapsed => {
                lost.insert(AnnotationKind::NumericExactness);
                lost.insert(AnnotationKind::TypeTag);
            }
            RichNumberPreservation::Unknown => return Fidelity::Unsupported,
        }
        return Fidelity::structural(lost);
    }

    let lossless_domains = DomainFlags::STRING | DomainFlags::BOOL;
    let lossless_reps = RepFlags::STR | RepFlags::BOOL;
    if node.domain.difference(lossless_domains).is_empty()
        && node.rep.difference(lossless_reps).is_empty()
    {
        return Fidelity::Lossless;
    }

    Fidelity::Unsupported
}

pub(super) fn fidelity_assessment_for_abstract(
    node: &HNode,
    capabilities: &BackendValueCapabilities,
) -> FidelityAssessmentV2 {
    match fidelity_for_abstract(node, capabilities) {
        Fidelity::Structural { lost } => {
            let possible = lost.as_set().iter().cloned().collect::<BTreeSet<_>>();
            // An abstract integer representation generally includes both
            // exactly representable and out-of-range values. Precision is
            // therefore a may-loss until a concrete value or a narrower range
            // proves otherwise. Rich-number kind collapse applies to every
            // numeric value and remains definite.
            let definite = possible
                .iter()
                .filter(|kind| **kind != AnnotationKind::NumericPrecision)
                .cloned()
                .collect::<BTreeSet<_>>();
            FidelityAssessmentV2::structural(definite, possible)
                .expect("abstract fidelity transfer preserves subset bounds")
        }
        other => FidelityAssessmentV2::from_concrete(other),
    }
}

fn abstract_integer_exceeds_capability(
    reps: RepFlags,
    exactness: &IntegerExactness,
) -> Option<bool> {
    let integer_reps = reps
        & (RepFlags::I8
            | RepFlags::I16
            | RepFlags::I32
            | RepFlags::I64
            | RepFlags::I128
            | RepFlags::BIG);
    if integer_reps.is_empty() {
        return Some(false);
    }
    if integer_reps.contains(RepFlags::BIG) {
        return match exactness {
            IntegerExactness::Unknown => None,
            IntegerExactness::Arbitrary => Some(false),
            IntegerExactness::ExactMagnitudeBits(_)
            | IntegerExactness::TwosComplementBits(_)
            | IntegerExactness::ExactRange { .. } => Some(true),
        };
    }

    let magnitude_bits = if integer_reps.contains(RepFlags::I128) {
        127_u16
    } else if integer_reps.contains(RepFlags::I64) {
        63
    } else if integer_reps.contains(RepFlags::I32) {
        31
    } else if integer_reps.contains(RepFlags::I16) {
        15
    } else {
        7
    };
    let magnitude = BigInt::from(1_u8) << usize::from(magnitude_bits);
    let minimum = -&magnitude;
    let maximum = &magnitude - 1_u8;
    integer_range_exceeds_capability(&minimum, &maximum, exactness)
}

fn apply_domain_to_all(
    graph: &mut HGraph,
    edge: &HEdge,
    mask: DomainFlags,
    trace: &mut SolveTrace,
) -> bool {
    let mut changed = false;
    for port in &edge.ports {
        if let Some(node) = value_node_mut(graph, port.node) {
            changed |= update_slot(&mut node.domain, mask, trace, port.node);
        }
    }
    changed
}

fn apply_domain_to_inputs(
    graph: &mut HGraph,
    edge: &HEdge,
    mask: DomainFlags,
    trace: &mut SolveTrace,
) -> bool {
    apply_domain_to_roles(graph, edge, mask, trace, |role| {
        matches!(role, PortRole::Input | PortRole::InOut)
    })
}

fn apply_domain_to_outputs(
    graph: &mut HGraph,
    edge: &HEdge,
    mask: DomainFlags,
    trace: &mut SolveTrace,
) -> bool {
    apply_domain_to_roles(graph, edge, mask, trace, |role| {
        matches!(role, PortRole::Output | PortRole::InOut)
    })
}

fn apply_domain_to_roles(
    graph: &mut HGraph,
    edge: &HEdge,
    mask: DomainFlags,
    trace: &mut SolveTrace,
    keep: impl Fn(PortRole) -> bool,
) -> bool {
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| keep(p.role))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(node) = value_node_mut(graph, nid) {
            changed |= update_slot(&mut node.domain, mask, trace, nid);
        }
    }
    changed
}

fn apply_rep_to_outputs(
    graph: &mut HGraph,
    edge: &HEdge,
    mask: RepFlags,
    trace: &mut SolveTrace,
) -> bool {
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(node) = value_node_mut(graph, nid) {
            changed |= update_slot(&mut node.rep, mask, trace, nid);
        }
    }
    changed
}

fn apply_fidelity_to_outputs(
    graph: &mut HGraph,
    edge: &HEdge,
    fidelity: FidelityAssessmentV2,
    trace: &mut SolveTrace,
) -> bool {
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(node) = value_node_mut(graph, nid) {
            changed |= apply_fidelity_to_node(node, fidelity.clone(), trace, nid);
        }
    }
    changed
}

fn apply_fidelity_to_node(
    node: &mut HNode,
    incoming: FidelityAssessmentV2,
    trace: &mut SolveTrace,
    node_id: NodeId,
) -> bool {
    let mut changed = update_slot(
        &mut node.fidelity_assessment,
        Some(incoming),
        trace,
        node_id,
    );
    let projected = node
        .fidelity_assessment
        .as_ref()
        .map(FidelityAssessmentV2::possible_fidelity);
    changed |= update_slot(&mut node.fidelity, projected, trace, node_id);
    changed
}

fn input_value_nodes<'a>(
    graph: &'a HGraph,
    edge: &'a HEdge,
) -> impl Iterator<Item = &'a HNode> + 'a {
    edge.ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Input | PortRole::InOut))
        .filter_map(|p| value_node(graph, p.node))
}

fn value_node(graph: &HGraph, id: NodeId) -> Option<&HNode> {
    graph
        .node(id)
        .filter(|node| matches!(&node.kind, HNodeKind::Value))
}

fn value_node_mut(graph: &mut HGraph, id: NodeId) -> Option<&mut HNode> {
    graph
        .node_mut(id)
        .filter(|node| matches!(&node.kind, HNodeKind::Value))
}
