use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::value::{AnnotationKind, Fidelity, ONumber, OValue};

use super::{
    graph::{EdgeId, HEdge, HGraph, HNode, NodeId, PortRole},
    kinds::{DomainFlags, OpKind, RepFlags},
};

pub fn solve_types(graph: &mut HGraph) {
    let mut changed = true;
    while changed {
        changed = false;
        for eid in graph.edge_ids() {
            changed |= propagate(graph, eid);
        }
    }
}

fn propagate(graph: &mut HGraph, eid: EdgeId) -> bool {
    let Some(edge) = graph.edge(eid).cloned() else {
        return false;
    };

    match &edge.kind {
        OpKind::Additive | OpKind::Multiplicative => {
            let intersection = edge
                .ports
                .iter()
                .filter_map(|p| graph.node(p.node))
                .fold(DomainFlags::NUMERIC, |acc, n| {
                    acc & n.domain & DomainFlags::NUMERIC
                });
            apply_domain_to_all(graph, &edge, intersection)
        }
        OpKind::Bitwise => {
            apply_domain_to_all(graph, &edge, DomainFlags::INTEGER | DomainFlags::BITFIELD)
        }
        OpKind::Ordered => {
            let mut changed =
                apply_domain_to_inputs(graph, &edge, DomainFlags::NUMERIC | DomainFlags::BOOL);
            changed |= apply_domain_to_outputs(graph, &edge, DomainFlags::BOOL);
            changed |= apply_rep_to_outputs(graph, &edge, RepFlags::BOOL);
            changed
        }
        OpKind::Bounded { value } => {
            let mut changed = apply_domain_to_outputs(graph, &edge, DomainFlags::INTEGER);
            changed |= apply_rep_to_outputs(graph, &edge, min_rep_for_bigint(value));
            changed |= materialize_bounded_outputs(graph, &edge, value);
            changed
        }
        OpKind::AbiFixed { dom, rep } => {
            let mut changed = false;
            for port in &edge.ports {
                if let Some(node) = graph.node_mut(port.node) {
                    let new_dom = node.domain & *dom;
                    let new_rep = node.rep & *rep;
                    if new_dom != node.domain || new_rep != node.rep {
                        node.domain = new_dom;
                        node.rep = new_rep;
                        changed = true;
                    }
                }
            }
            changed
        }
        OpKind::FieldAccess { .. } => apply_domain_to_inputs(graph, &edge, DomainFlags::STRUCT),
        OpKind::Dereferenceable => apply_domain_to_all(graph, &edge, DomainFlags::POINTER),
        OpKind::BackendCrossing { from_lang, to_lang } => {
            let fidelity = input_nodes(&edge)
                .next()
                .and_then(|id| graph.node(id))
                .map(|node| fidelity_for(node, from_lang, to_lang))
                .unwrap_or(Fidelity::Unsupported);
            apply_fidelity_to_outputs(graph, &edge, fidelity)
        }
        OpKind::DataFlow => propagate_dataflow(graph, &edge),
        OpKind::StructuralBarrier
        | OpKind::Sequence
        | OpKind::ActorSerial { .. }
        | OpKind::Batch
        | OpKind::All
        | OpKind::Any
        | OpKind::Race
        | OpKind::X86 { .. }
        | OpKind::OcoreOp { .. } => false,
    }
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

fn propagate_dataflow(graph: &mut HGraph, edge: &HEdge) -> bool {
    let Some(input) = input_nodes(edge)
        .next()
        .and_then(|id| graph.node(id).cloned())
    else {
        return false;
    };
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(output) = graph.node_mut(nid) {
            if output.domain != input.domain {
                output.domain = input.domain;
                changed = true;
            }
            if output.rep != input.rep {
                output.rep = input.rep;
                changed = true;
            }
            if output.fidelity != input.fidelity {
                output.fidelity = input.fidelity.clone();
                changed = true;
            }
            if output.value.is_none() && input.value.is_some() {
                output.value = input.value.clone();
                changed = true;
            }
        }
    }
    changed
}

fn materialize_bounded_outputs(graph: &mut HGraph, edge: &HEdge, value: &BigInt) -> bool {
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(node) = graph.node_mut(nid) {
            let should_write = matches!(
                node.value,
                None | Some(OValue::Int { .. }) | Some(OValue::Number { .. })
            );
            if should_write {
                let new_value = OValue::big_int(value.clone());
                if node.value.as_ref() != Some(&new_value) {
                    node.value = Some(new_value);
                    changed = true;
                }
            }
        }
    }
    changed
}

pub fn fidelity_for(node: &HNode, from_lang: &str, to_lang: &str) -> Fidelity {
    if from_lang == to_lang {
        return Fidelity::Lossless;
    }
    if matches!(node.value, Some(OValue::Native { .. })) {
        return Fidelity::NativeCapsule;
    }
    if let Some(value) = &node.value {
        return fidelity_for_value(value, to_lang);
    }
    if node.rep.contains(RepFlags::BIG) && !backend_supports_bigint(to_lang) {
        return Fidelity::structural([AnnotationKind::NumericPrecision]);
    }
    if node.domain.is_empty() || node.rep.is_empty() {
        return Fidelity::Unsupported;
    }
    Fidelity::Lossless
}

pub fn fidelity_for_value(value: &OValue, to_lang: &str) -> Fidelity {
    match value {
        OValue::Native { .. } => Fidelity::NativeCapsule,
        OValue::Number {
            v: ONumber::Int { v },
        } if min_rep_for_bigint(v) == RepFlags::BIG && !backend_supports_bigint(to_lang) => {
            Fidelity::structural([AnnotationKind::NumericPrecision])
        }
        OValue::Number { .. } if !backend_supports_rich_numbers(to_lang) => {
            Fidelity::structural([AnnotationKind::NumericExactness, AnnotationKind::TypeTag])
        }
        OValue::Graph { .. } => Fidelity::structural([AnnotationKind::Identity]),
        OValue::Capability { .. } => Fidelity::structural([AnnotationKind::Capability]),
        _ => Fidelity::Lossless,
    }
}

fn backend_supports_bigint(lang: &str) -> bool {
    matches!(
        lang,
        "python" | "ruby" | "racket" | "haskell" | "lisp" | "common_lisp" | "mathematica"
    )
}

fn backend_supports_rich_numbers(lang: &str) -> bool {
    matches!(lang, "python" | "racket" | "haskell" | "mathematica")
}

fn apply_domain_to_all(graph: &mut HGraph, edge: &HEdge, mask: DomainFlags) -> bool {
    let mut changed = false;
    for port in &edge.ports {
        if let Some(node) = graph.node_mut(port.node) {
            let new = node.domain & mask;
            if new != node.domain {
                node.domain = new;
                changed = true;
            }
        }
    }
    changed
}

fn apply_domain_to_inputs(graph: &mut HGraph, edge: &HEdge, mask: DomainFlags) -> bool {
    apply_domain_to_roles(graph, edge, mask, |role| {
        matches!(role, PortRole::Input | PortRole::InOut)
    })
}

fn apply_domain_to_outputs(graph: &mut HGraph, edge: &HEdge, mask: DomainFlags) -> bool {
    apply_domain_to_roles(graph, edge, mask, |role| {
        matches!(role, PortRole::Output | PortRole::InOut)
    })
}

fn apply_domain_to_roles(
    graph: &mut HGraph,
    edge: &HEdge,
    mask: DomainFlags,
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
        if let Some(node) = graph.node_mut(nid) {
            let new = node.domain & mask;
            if new != node.domain {
                node.domain = new;
                changed = true;
            }
        }
    }
    changed
}

fn apply_rep_to_outputs(graph: &mut HGraph, edge: &HEdge, mask: RepFlags) -> bool {
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(node) = graph.node_mut(nid) {
            let new = node.rep & mask;
            if new != node.rep {
                node.rep = new;
                changed = true;
            }
        }
    }
    changed
}

fn apply_fidelity_to_outputs(graph: &mut HGraph, edge: &HEdge, fidelity: Fidelity) -> bool {
    let mut changed = false;
    for nid in edge
        .ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Output | PortRole::InOut))
        .map(|p| p.node)
        .collect::<Vec<_>>()
    {
        if let Some(node) = graph.node_mut(nid) {
            let old = node.fidelity.clone();
            let new = match old.clone() {
                Some(existing) => existing.compose(fidelity.clone()),
                None => fidelity.clone(),
            };
            if old.as_ref() != Some(&new) {
                node.fidelity = Some(new);
                changed = true;
            }
        }
    }
    changed
}

fn input_nodes(edge: &HEdge) -> impl Iterator<Item = NodeId> + '_ {
    edge.ports
        .iter()
        .filter(|p| matches!(p.role, PortRole::Input | PortRole::InOut))
        .map(|p| p.node)
}
