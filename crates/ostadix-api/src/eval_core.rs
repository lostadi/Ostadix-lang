//! Evaluator-independent graph execution vocabulary.
//!
//! This module owns the pure frame, trace, policy-projection, fingerprint, and
//! splice-rendering contracts shared by the evaluator and graph executor. Live
//! evaluator state and authority remain behind `GraphEvaluationHost`, so the
//! executor depends on this narrow seam rather than on `eval`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use num_traits::ToPrimitive;

use crate::backend_catalog::{BackendInterface, SpliceRenderer};
use crate::capability::BackendSandboxPolicy;
use crate::evidence::AdmittedExecution;
use crate::execution_contract::Policy;
use crate::ir::{ExecutionPlan, InvokeMode, OIr, PlanEdgeKind, PlanNodeId, PlanNodeKind};
use crate::value::{fingerprint_preview, DecimalSpecial, FloatFormat, ONumber, OValue, SeqKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTrace {
    pub events: Vec<TraceEvent>,
}

impl ExecutionTrace {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }
}

impl Default for ExecutionTrace {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceEvent {
    NodeReady(PlanNodeId),
    NodeStarted(PlanNodeId),
    NodeFinished {
        id: PlanNodeId,
        value_type: String,
        fingerprint: Option<String>,
    },
    NodeFailed {
        id: PlanNodeId,
        message: String,
    },
    /// The operation executed speculatively, but strict fail-stop settlement
    /// withheld its result after an earlier semantic failure or infrastructure
    /// abort.
    NodeDiscarded {
        id: PlanNodeId,
        reason: String,
    },
}

/// Materialized values and policy context for one graph evaluation.
pub(crate) struct GraphEvalFrame {
    pub(crate) values: Vec<Option<OValue>>,
    pub(crate) base_scope: HashMap<String, OValue>,
    pub(crate) node_policy: Vec<Policy>,
    pub(crate) trace: ExecutionTrace,
}

impl GraphEvalFrame {
    pub(crate) fn value(&self, id: PlanNodeId) -> Result<&OValue> {
        self.values
            .get(id.0)
            .and_then(Option::as_ref)
            .ok_or_else(|| anyhow::anyhow!("plan node {} has not produced a value", id.0))
    }

    pub(crate) fn set_value(&mut self, id: PlanNodeId, value: OValue) -> Result<()> {
        let slot = self
            .values
            .get_mut(id.0)
            .ok_or_else(|| anyhow::anyhow!("plan node {} is out of bounds", id.0))?;
        *slot = Some(value);
        Ok(())
    }

    pub(crate) fn scope_from_data_edges(
        &self,
        node_id: PlanNodeId,
        plan: &ExecutionPlan,
    ) -> Result<HashMap<String, OValue>> {
        let mut scope = self.base_scope.clone();
        for source in data_predecessors(plan, node_id) {
            if let PlanNodeKind::Store { name } = &plan.nodes[source.0].kind {
                scope.insert(name.clone(), self.value(source)?.clone());
            }
        }
        Ok(scope)
    }

    pub(crate) fn exec_scope(
        &self,
        node_id: PlanNodeId,
        plan: &ExecutionPlan,
    ) -> Result<HashMap<String, OValue>> {
        let mut scope = self.scope_from_data_edges(node_id, plan)?;
        for child in structural_children(plan, node_id) {
            if let PlanNodeKind::Store { name } = &plan.nodes[child.0].kind {
                scope.insert(name.clone(), self.value(child)?.clone());
            }
        }
        Ok(scope)
    }
}

/// The evaluator authority and state operations required by graph execution.
///
/// This trait is deliberately crate-private and has no `Send` bound: the
/// process registry and live actor state stay on the coordinator thread.
pub(crate) trait GraphEvaluationHost {
    fn verify_admitted_runtime_context(&self, admitted: &AdmittedExecution<'_>) -> Result<()>;

    fn local_worker_parallelism_override(&self) -> Option<usize>;

    fn shim_path(&self, language: &str) -> PathBuf;

    fn authorize_autonomous_ephemeral_shim(
        &self,
        backend: &BackendInterface,
        authority_scope: &HashMap<String, OValue>,
    ) -> Result<BackendSandboxPolicy>;

    fn set_policy(&mut self, policy: Policy) -> Policy;

    fn eval_source_with_scope_until(
        &mut self,
        src: &str,
        caller_scope: &HashMap<String, OValue>,
        deadline: Instant,
    ) -> Result<OValue>;

    fn execute_ready_plan_node(
        &mut self,
        node_id: PlanNodeId,
        node: &OIr,
        plan: &ExecutionPlan,
        frame: &mut GraphEvalFrame,
    ) -> Result<OValue>;

    fn install_execution_trace(&mut self, trace: ExecutionTrace);

    fn flush_autonomous_buffer(&mut self) -> Result<()>;

    fn resolve_after_flush(&mut self, value: OValue) -> Result<OValue>;
}

pub(crate) fn trace_fingerprint(value: &OValue) -> Option<String> {
    match value {
        OValue::NixExpr { fingerprint, .. }
        | OValue::Request { fingerprint, .. }
        | OValue::Thunk { fingerprint, .. }
        | OValue::Group { fingerprint, .. } => Some(fingerprint.clone()),
        _ => None,
    }
}

pub(crate) fn data_predecessors(plan: &ExecutionPlan, node_id: PlanNodeId) -> Vec<PlanNodeId> {
    let mut sources = plan
        .edges
        .iter()
        .filter_map(|edge| {
            (edge.kind == PlanEdgeKind::Data && edge.to == node_id).then_some(edge.from)
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|id| id.0);
    sources
}

fn structural_children(plan: &ExecutionPlan, parent: PlanNodeId) -> Vec<PlanNodeId> {
    let mut children = plan
        .edges
        .iter()
        .filter_map(|edge| {
            (edge.kind == PlanEdgeKind::Structural && edge.to == parent).then_some(edge.from)
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|id| id.0);
    children
}

pub(crate) fn derive_policy_contexts(
    plan: &ExecutionPlan,
    flat: &[&OIr],
    base_policy: Policy,
) -> Result<Vec<Policy>> {
    if flat.len() != plan.nodes.len() {
        bail!(
            "OIR flatten produced {} nodes but plan has {} nodes",
            flat.len(),
            plan.nodes.len()
        );
    }

    let mut policies = vec![base_policy; plan.nodes.len()];
    for plan_node in &plan.nodes {
        let id = plan_node.id;
        let parent_policy = policies[id.0];
        let child_policy = match flat[id.0] {
            OIr::Invoke {
                mode: InvokeMode::Lazy,
                ..
            } => Policy::Lazy,
            OIr::Invoke {
                mode: InvokeMode::Autonomous,
                ..
            } => Policy::Autonomous,
            OIr::Invoke {
                mode: InvokeMode::Group(_),
                ..
            } => match parent_policy {
                Policy::Autonomous => Policy::Autonomous,
                Policy::Lazy => Policy::Lazy,
                Policy::Eager => Policy::Lazy,
            },
            _ => parent_policy,
        };

        for child in structural_children(plan, id) {
            policies[child.0] = child_policy;
        }
    }

    Ok(policies)
}

/// Render using the strategy frozen into executable OIR. Keeping this as a
/// value-level function lets tests exercise renderers directly while runtime
/// execution never has to rediscover backend policy from a language string.
pub(crate) fn render_with(renderer: SpliceRenderer, val: &OValue) -> String {
    match renderer {
        SpliceRenderer::Python => render_python(val),
        SpliceRenderer::Html => render_html(val),
        SpliceRenderer::Latex => render_latex(val),
        SpliceRenderer::Markdown => render_markdown(val),
        SpliceRenderer::Nix => render_nix(val),
        SpliceRenderer::Default => val.splice_repr(),
    }
}

/// How much OValue information survives a source-splice rendering.
///
/// This classification is deliberately separate from OValue's wire lifting.
/// Wire lifting preserves the tagged OValue. A splice renderer projects that
/// value into a consumer language and may erase tags or retain only a readable
/// marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFidelity {
    /// The consumer syntax retains the value and its O-level type.
    Typed,
    /// Portable payload or structure survives, but O-level tags or auxiliary
    /// metadata may be erased.
    Structural,
    /// The renderer intentionally produces a human-facing presentation.
    Presentation,
    /// Only an identifying marker or summary survives.
    Opaque,
}

/// Select the weaker classification while folding children of one renderer.
///
/// This is deliberately not a public cross-renderer order. `Structural` is
/// reachable only for source-oriented renderers while `Presentation` is
/// reachable only for human-facing renderers, so their relative branch below
/// is defensive rather than a claimed semantic comparison.
const fn weaker_for_container(left: RenderFidelity, right: RenderFidelity) -> RenderFidelity {
    match (left, right) {
        (RenderFidelity::Opaque, _) | (_, RenderFidelity::Opaque) => RenderFidelity::Opaque,
        (RenderFidelity::Presentation, _) | (_, RenderFidelity::Presentation) => {
            RenderFidelity::Presentation
        }
        (RenderFidelity::Structural, _) | (_, RenderFidelity::Structural) => {
            RenderFidelity::Structural
        }
        _ => RenderFidelity::Typed,
    }
}

fn container_fidelity<'a>(
    renderer: SpliceRenderer,
    children: impl IntoIterator<Item = &'a OValue>,
    base: RenderFidelity,
) -> RenderFidelity {
    children
        .into_iter()
        .map(|child| render_fidelity(renderer, child))
        .fold(base, weaker_for_container)
}

fn entry_container_fidelity(
    renderer: SpliceRenderer,
    entries: &[(OValue, OValue)],
    base: RenderFidelity,
) -> RenderFidelity {
    container_fidelity(
        renderer,
        entries.iter().flat_map(|(key, value)| [key, value]),
        base,
    )
}

/// Recompute the descriptive source projection for one value and renderer
/// under that backend's standard shim prelude.
///
/// Recursive containers inherit their weakest child classification within the
/// same renderer. This is neither an admission check nor a conversion to the
/// value-crossing [`crate::value::Fidelity`] domains.
pub fn render_fidelity(renderer: SpliceRenderer, val: &OValue) -> RenderFidelity {
    use OValue::*;
    use RenderFidelity::*;

    match renderer {
        SpliceRenderer::Python => match val {
            Null
            | Bool { .. }
            | Html { .. }
            | StorePath { .. }
            | Expr { .. }
            | Scope { .. }
            | NixExpr { .. }
            | Derivation { .. }
            | Request { .. }
            | System { .. }
            | Capability { .. }
            | Snapshot { .. }
            | Thunk { .. }
            | Group { .. }
            | Graph { .. }
            | Native { .. }
            | Error { .. } => Typed,
            Number {
                v: ONumber::Int { .. },
            } => Typed,
            Number {
                v:
                    ONumber::Decimal {
                        coeff,
                        exp10,
                        special,
                    },
            } if python_decimal_round_trips_exactly(coeff, *exp10, *special) => Typed,
            Number {
                v:
                    ONumber::BinaryFloat {
                        format: FloatFormat::F64,
                        bits,
                    },
            } if bits.len() == 8 => Typed,
            Number { .. } => Structural,
            Text { .. } | Char { .. } | Bytes { .. } | Symbol { .. } | Keyword { .. } => Structural,
            List { v } => container_fidelity(renderer, v, Typed),
            Map { v } => container_fidelity(renderer, v.values(), Typed),
            Seq {
                kind: SeqKind::Tuple,
                items,
            } => container_fidelity(renderer, items, Typed),
            Seq { items, .. } | Set { items, .. } => {
                container_fidelity(renderer, items, Structural)
            }
            Object { fields } => container_fidelity(renderer, fields.values(), Structural),
            EntriesMap { entries } => entry_container_fidelity(renderer, entries, Structural),
            Blob { .. } => Structural,
        },
        SpliceRenderer::Nix => match val {
            Null | Bool { .. } => Typed,
            Number {
                v: ONumber::Int { v },
            } if v.to_i64().is_some() => Typed,
            Text { v } if v.encoding.as_deref() == Some("utf-8") && !v.utf8.contains('\0') => Typed,
            Number { .. } | Text { .. } => Structural,
            List { v } => container_fidelity(renderer, v, Typed),
            Map { v } => container_fidelity(
                renderer,
                v.values(),
                if v.keys().all(|key| !key.contains('\0')) {
                    Typed
                } else {
                    Structural
                },
            ),
            Seq { items, .. } | Set { items, .. } => {
                container_fidelity(renderer, items, Structural)
            }
            Object { fields } => container_fidelity(renderer, fields.values(), Structural),
            EntriesMap { entries } => entry_container_fidelity(renderer, entries, Structural),
            Char { .. } | Symbol { .. } | Keyword { .. } => Structural,
            Bytes { v } if v.media_type.is_some() => Structural,
            Bytes { .. } | Graph { .. } | Native { .. } => Opaque,
            Html { .. }
            | StorePath { .. }
            | Blob { .. }
            | NixExpr { .. }
            | Thunk { .. }
            | Derivation { .. }
            | System { .. }
            | Expr { .. } => Structural,
            Scope { .. }
            | Request { .. }
            | Capability { .. }
            | Snapshot { .. }
            | Group { .. }
            | Error { .. } => Opaque,
        },
        SpliceRenderer::Html | SpliceRenderer::Latex | SpliceRenderer::Markdown => match val {
            Null
            | Bool { .. }
            | Number { .. }
            | Text { .. }
            | Char { .. }
            | Html { .. }
            | StorePath { .. }
            | Blob { .. }
            | Bytes { .. }
            | Symbol { .. }
            | Keyword { .. }
            | NixExpr { .. }
            | Derivation { .. }
            | System { .. }
            | Thunk { .. }
            | Expr { .. }
            | Error { .. } => Presentation,
            List { v } => container_fidelity(renderer, v, Presentation),
            Map { v } => container_fidelity(renderer, v.values(), Presentation),
            Seq { items, .. } | Set { items, .. } => {
                container_fidelity(renderer, items, Presentation)
            }
            Object { fields } => container_fidelity(renderer, fields.values(), Presentation),
            EntriesMap { entries } => entry_container_fidelity(renderer, entries, Presentation),
            Scope { .. }
            | Request { .. }
            | Capability { .. }
            | Snapshot { .. }
            | Group { .. }
            | Graph { .. }
            | Native { .. } => Opaque,
        },
        SpliceRenderer::Default => match val {
            Null
            | Bool { .. }
            | Number { .. }
            | Text { .. }
            | Char { .. }
            | Symbol { .. }
            | Keyword { .. } => Structural,
            Bytes { v } if v.media_type.is_some() => Structural,
            Bytes { .. } => Opaque,
            List { v } => container_fidelity(renderer, v, Structural),
            Map { v } => container_fidelity(renderer, v.values(), Structural),
            Seq { items, .. } | Set { items, .. } => {
                container_fidelity(renderer, items, Structural)
            }
            Object { fields } => container_fidelity(renderer, fields.values(), Structural),
            EntriesMap { entries } => entry_container_fidelity(renderer, entries, Structural),
            Html { .. }
            | StorePath { .. }
            | Blob { .. }
            | NixExpr { .. }
            | Derivation { .. }
            | Thunk { .. }
            | Expr { .. }
            | System { .. } => Structural,
            Scope { .. }
            | Request { .. }
            | Capability { .. }
            | Snapshot { .. }
            | Group { .. }
            | Graph { .. }
            | Native { .. }
            | Error { .. } => Opaque,
        },
    }
}

fn sorted_map_entries(values: &HashMap<String, OValue>) -> Vec<(&String, &OValue)> {
    let mut entries = values.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| *key);
    entries
}

fn python_decimal_round_trips_exactly(
    coeff: &num_bigint::BigInt,
    exp10: i64,
    special: Option<DecimalSpecial>,
) -> bool {
    const PORTABLE_MAX_EMAX: i64 = 425_000_000;
    const PORTABLE_MIN_ETINY: i64 = -849_999_999;
    const PORTABLE_MAX_PREC: usize = 425_000_000;
    let coeff_is_zero = coeff == &num_bigint::BigInt::from(0);
    match special {
        None => {
            let coefficient_digits = coeff.to_str_radix(10).trim_start_matches('-').len();
            let adjusted_exponent = i64::try_from(coefficient_digits)
                .ok()
                .and_then(|digits| exp10.checked_add(digits - 1));
            !coeff_is_zero
                && coefficient_digits <= PORTABLE_MAX_PREC
                && exp10 >= PORTABLE_MIN_ETINY
                && adjusted_exponent.is_some_and(|adjusted| adjusted <= PORTABLE_MAX_EMAX)
        }
        Some(_) => coeff_is_zero && exp10 == 0,
    }
}

fn render_python_bigint(value: &num_bigint::BigInt) -> String {
    // CPython permits the decimal conversion ceiling to be configured as low
    // as 640 digits. Power-of-two bases are exempt, so larger integers use an
    // exact hexadecimal constructor instead of an invalid decimal literal.
    const PORTABLE_DECIMAL_DIGITS: usize = 640;
    let decimal = value.to_string();
    if decimal.trim_start_matches('-').len() <= PORTABLE_DECIMAL_DIGITS {
        decimal
    } else {
        let hex = value.to_str_radix(16);
        match hex.strip_prefix('-') {
            Some(magnitude) => format!("-0x{magnitude}"),
            None => format!("0x{hex}"),
        }
    }
}

fn python_render_is_hashable(value: &OValue) -> bool {
    match value {
        OValue::List { .. }
        | OValue::Map { .. }
        | OValue::Object { .. }
        | OValue::EntriesMap { .. }
        | OValue::Set { .. }
        | OValue::Blob { .. } => false,
        OValue::Seq {
            kind: SeqKind::Tuple,
            items,
        } => items.iter().all(python_render_is_hashable),
        OValue::Seq { .. } => false,
        _ => true,
    }
}

fn render_nix_string(value: &str) -> String {
    if value.contains('\0') {
        // Nix strings cannot contain NUL. A tagged Unicode-scalar sequence is
        // syntactically valid, reconstructible, and domain-separated from
        // every ordinary string (including the old JSON-spelling fallback).
        let codepoints = value
            .chars()
            .map(|ch| u32::from(ch).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        return format!(
            "{{ __ostadix_string_encoding = \"unicode-codepoints-v1\"; codepoints = [ {codepoints} ]; }}"
        );
    }

    let mut rendered = String::with_capacity(value.len() + 2);
    rendered.push('"');
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => rendered.push_str("\\\""),
            '\\' => rendered.push_str("\\\\"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            '$' if chars.peek() == Some(&'{') => rendered.push_str("\\$"),
            _ => rendered.push(ch),
        }
    }
    rendered.push('"');
    rendered
}

fn render_nix_attr_name(value: &str) -> String {
    debug_assert!(!value.contains('\0'));
    render_nix_string(value)
}

fn render_nix_keyed_entries(entries: &[(&String, &OValue)]) -> String {
    let items = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "{{ key = {}; value = {}; }}",
                render_nix_string(key),
                render_nix(value)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("[ {items} ]")
}

// ═════════════════════════════════════════════════════════════════════════════
// Language-specific renderers
// ═════════════════════════════════════════════════════════════════════════════

// ── Python ───────────────────────────────────────────────────────────────────

fn render_nix(val: &OValue) -> String {
    match val {
        OValue::Null => "null".to_string(),
        OValue::Bool { v } => {
            if *v {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        OValue::Number { v } => render_nix_number(v),
        OValue::Text { v } => render_nix_string(&v.utf8),
        OValue::Char { scalar } => render_nix_string(&scalar.to_string()),
        OValue::Html { v } => render_nix_string(v),
        OValue::StorePath { path } => render_nix_string(path),
        OValue::List { v } => {
            let items = v.iter().map(render_nix).collect::<Vec<_>>().join(" ");
            format!("[ {} ]", items)
        }
        OValue::Map { v } => {
            let entries = sorted_map_entries(v);
            if entries.iter().any(|(key, _)| key.contains('\0')) {
                return render_nix_keyed_entries(&entries);
            }
            let items = entries
                .into_iter()
                .map(|(k, val)| {
                    let key = render_nix_attr_name(k);
                    format!("{key} = {};", render_nix(val))
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{{ {} }}", items)
        }
        OValue::Seq { items, .. } | OValue::Set { items, .. } => {
            let items = items.iter().map(render_nix).collect::<Vec<_>>().join(" ");
            format!("[ {} ]", items)
        }
        OValue::Object { fields } => {
            let entries = fields.iter().collect::<Vec<_>>();
            if entries.iter().any(|(key, _)| key.contains('\0')) {
                return render_nix_keyed_entries(&entries);
            }
            let items = entries
                .iter()
                .map(|(k, val)| {
                    let key = render_nix_attr_name(k);
                    format!("{key} = {};", render_nix(val))
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{{ {} }}", items)
        }
        OValue::EntriesMap { entries } => {
            let items = entries
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{{ key = {}; value = {}; }}",
                        render_nix(key),
                        render_nix(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("[ {} ]", items)
        }
        OValue::Symbol { .. } | OValue::Keyword { .. } => render_nix_string(&val.splice_repr()),
        OValue::Scope { bindings } => {
            format!("\"<scope bindings={}>\"", bindings.len())
        }
        OValue::Blob { v, .. } => render_nix_string(v),
        OValue::Bytes { .. } | OValue::Graph { .. } | OValue::Native { .. } => {
            render_nix_string(&val.splice_repr())
        }
        // An ONixExpr spliced into a Nix context is its already-assembled body.
        // Internal producers expect valid Nix; the public/deserialized carrier
        // intentionally retains that backend parse obligation. Empty public
        // bodies still fall back to an inert, syntactically valid string.
        OValue::NixExpr { body, .. } if !body.trim().is_empty() => format!("({body})"),
        OValue::NixExpr { body, .. } => render_nix_string(body),
        // A Derivation in a Nix context is its .drv path literal.
        OValue::Derivation { drv_path, .. } => render_nix_string(drv_path),
        // A Request rendered into Nix source is almost certainly a user error:
        // the user spliced a control value into source text. Preserve that fact
        // as an inert marker; STEP3 can elevate it or auto-resolve it earlier.
        OValue::Request { fingerprint, .. } => {
            // STEP-3.5: in a Nix context, an unforced Request is almost
            // always a user error. We emit an inert string marker. {lazy} Eval
            // requests should have been
            // auto-forced before reaching here; {defer} should have errored;
            // Instantiate/Realise have no sensible Nix-context splice form.
            render_nix_string(&format!(
                "<request fp={}>",
                fingerprint_preview(fingerprint)
            ))
        }
        // A Thunk does not carry its source language (that lives on its
        // wrapping Request), so an unforced cross-language thunk is preserved
        // as inert source text rather than guessed to be a Nix expression.
        OValue::Thunk { body, .. } => render_nix_string(body),

        // A Group is a control/topology value with no Nix splice form. Render
        // an inert marker, as for an unforced Request. Force the group with
        // `now(...)` before splicing.
        OValue::Group {
            mode, fingerprint, ..
        } => render_nix_string(&format!(
            "<group:{} fp={}>",
            mode.name(),
            fingerprint_preview(fingerprint)
        )),

        // A System in a Nix context renders as its profile path as a string
        // literal. Useful for Nix expressions that want to inspect or compare
        // against the live profile location.
        OValue::System { profile_path } => render_nix_string(profile_path),

        OValue::Capability { kind, identity, .. } => {
            render_nix_string(&format!("<capability:{} {}>", kind.name(), identity))
        }

        OValue::Snapshot { kind, identity, .. } => {
            render_nix_string(&format!("<snapshot:{} {}>", kind.name(), identity))
        }

        // An Expr in Nix context renders its quoted source as a Nix string
        // literal. Rarely useful — the user almost always wants O.eval first.
        OValue::Expr { src } => render_nix_string(src),

        // Preserve an error outcome as an inert string marker; errors normally
        // should not reach Nix source.
        OValue::Error { msg } => render_nix_string(&format!("<error: {msg}>")),
    }
}

fn render_python(val: &OValue) -> String {
    match val {
        OValue::Null => "None".to_string(),

        OValue::Bool { v } => {
            if *v {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }

        OValue::Number { v } => render_python_number(v),

        OValue::Text { v } => serde_json::to_string(&v.utf8).unwrap_or_else(|_| "''".to_string()),
        OValue::Char { scalar } => {
            serde_json::to_string(&scalar.to_string()).unwrap_or_else(|_| "''".to_string())
        }

        OValue::Html { v } => {
            let lit = serde_json::to_string(v).unwrap_or_else(|_| "''".to_string());
            format!("OHtml({})", lit)
        }

        OValue::StorePath { path } => {
            let lit = serde_json::to_string(path).unwrap_or_else(|_| "''".to_string());
            format!("OStorePath({})", lit)
        }

        OValue::List { v } => {
            let items = v.iter().map(render_python).collect::<Vec<_>>().join(", ");

            format!("[{}]", items)
        }
        OValue::Seq {
            kind: SeqKind::Tuple,
            items,
        } => {
            let singleton = items.len() == 1;
            let rendered = items
                .iter()
                .map(render_python)
                .collect::<Vec<_>>()
                .join(", ");
            if singleton {
                format!("({},)", rendered)
            } else {
                format!("({})", rendered)
            }
        }
        OValue::Seq { items, .. } => {
            let items = items
                .iter()
                .map(render_python)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{}]", items)
        }

        OValue::Map { v } => {
            let items = sorted_map_entries(v)
                .into_iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| "''".to_string());
                    format!("{}: {}", key, render_python(val))
                })
                .collect::<Vec<_>>()
                .join(", ");

            format!("{{{}}}", items)
        }
        OValue::Object { fields } => {
            let items = fields
                .iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| "''".to_string());
                    format!("{}: {}", key, render_python(val))
                })
                .collect::<Vec<_>>()
                .join(", ");

            format!("{{{}}}", items)
        }
        OValue::EntriesMap { entries } => {
            let items = entries
                .iter()
                .map(|(key, value)| format!("({}, {})", render_python(key), render_python(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{}]", items)
        }
        OValue::Set { items, .. } => {
            let all_hashable = items.iter().all(python_render_is_hashable);
            let rendered = items
                .iter()
                .map(render_python)
                .collect::<Vec<_>>()
                .join(", ");
            if all_hashable {
                format!("set([{rendered}])")
            } else {
                format!("[{rendered}]")
            }
        }
        OValue::Symbol { .. } | OValue::Keyword { .. } => {
            serde_json::to_string(&val.splice_repr()).unwrap_or_else(|_| "''".to_string())
        }

        OValue::Scope { bindings } => {
            let wire = serde_json::to_string(&OValue::Scope {
                bindings: bindings.clone(),
            })
            .expect("OValue::Scope must serialize");
            let encoded = serde_json::to_string(&wire).expect("scope JSON string must serialize");
            format!("OScopeValue.from_wire_json({encoded})")
        }

        OValue::Blob { v, mime } => {
            let mime_lit = serde_json::to_string(mime).unwrap_or_else(|_| "''".to_string());
            let data_lit = serde_json::to_string(v).unwrap_or_else(|_| "''".to_string());

            format!("{{'mime': {}, 'base64': {}}}", mime_lit, data_lit)
        }
        OValue::Bytes { v } => {
            let items = v
                .bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("bytes([{}])", items)
        }

        OValue::NixExpr { .. }
        | OValue::Derivation { .. }
        | OValue::Request { .. }
        | OValue::Thunk { .. }
        | OValue::System { .. }
        | OValue::Capability { .. }
        | OValue::Snapshot { .. }
        | OValue::Graph { .. }
        | OValue::Native { .. }
        | OValue::Group { .. }
        | OValue::Error { .. } => render_python_opaque(val),

        // An Expr value in Python is available as an OExprValue object (set up
        // by the Python shim's oval_to_py). Splicing it into source text as a
        // Python repr would lose the type, so we render it as an OExprValue
        // constructor that the shim recognises. The shim ensures OExprValue is
        // always in scope when handling exec bindings.
        OValue::Expr { src } => {
            let src_lit = serde_json::to_string(src).unwrap_or_else(|_| "''".to_string());
            format!("OExprValue({})", src_lit)
        }
    }
}

fn render_python_opaque(val: &OValue) -> String {
    let wire = serde_json::to_string(val).expect("OValue must serialize for Python rendering");
    let encoded = serde_json::to_string(&wire).expect("OValue JSON string must serialize");
    format!("OOpaqueValue.from_wire_json({encoded})")
}

fn render_nix_number(value: &ONumber) -> String {
    match value {
        ONumber::Int { v } if v.to_i64() == Some(i64::MIN) => {
            "(-9223372036854775807 - 1)".to_string()
        }
        ONumber::Int { v } if v.to_i64().is_some() => v.to_string(),
        ONumber::Int { v } => render_nix_string(&v.to_string()),
        ONumber::BinaryFloat {
            format: FloatFormat::F32,
            bits,
        } if bits.len() == 4 => {
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(bits);
            let decoded = f32::from_bits(u32::from_be_bytes(raw)) as f64;
            if decoded.is_finite() {
                render_float_literal(decoded)
            } else {
                render_nix_string(&render_number_fallback(value))
            }
        }
        ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits,
        } if bits.len() == 8 => {
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(bits);
            let decoded = f64::from_bits(u64::from_be_bytes(raw));
            if decoded.is_finite() {
                render_float_literal(decoded)
            } else {
                render_nix_string(&render_number_fallback(value))
            }
        }
        other => render_nix_string(&render_number_fallback(other)),
    }
}

fn render_number_fallback(value: &ONumber) -> String {
    match value {
        ONumber::Int { v } => v.to_string(),
        ONumber::Rational { num, den } => format!("{num}/{den}"),
        ONumber::Decimal {
            coeff,
            exp10,
            special,
        } => match special {
            Some(special) => format!("{special:?}").to_lowercase(),
            None => format!("{coeff}e{exp10}"),
        },
        ONumber::BinaryFloat { format, bits } => {
            format!("{format:?}:{}", hex::encode(bits))
        }
        ONumber::BigFloat {
            mantissa,
            exp2,
            precision,
            special,
        } => match special {
            Some(special) => format!("{special:?}").to_lowercase(),
            None => format!("{mantissa}p{exp2}@{precision:?}"),
        },
        ONumber::Complex { re, im } => {
            format!(
                "{}+{}i",
                render_number_fallback(re),
                render_number_fallback(im)
            )
        }
    }
}

fn render_float_literal(value: f64) -> String {
    let rendered = value.to_string();
    if value.is_finite()
        && !rendered.contains('.')
        && !rendered.contains('e')
        && !rendered.contains('E')
    {
        format!("{rendered}.0")
    } else {
        rendered
    }
}

fn render_python_number(value: &ONumber) -> String {
    match value {
        ONumber::Int { v } => render_python_bigint(v),
        ONumber::Rational { num, den } if den != &num_bigint::BigInt::from(0) => {
            format!(
                "__import__('fractions').Fraction({}, {})",
                render_python_bigint(num),
                render_python_bigint(den)
            )
        }
        ONumber::Rational { .. } => py_string_literal(&render_number_fallback(value)),
        ONumber::Decimal {
            coeff,
            exp10,
            special,
        } => {
            let literal = match special {
                Some(DecimalSpecial::Nan) => "NaN".to_string(),
                Some(DecimalSpecial::PosInf) => "Infinity".to_string(),
                Some(DecimalSpecial::NegInf) => "-Infinity".to_string(),
                Some(DecimalSpecial::PosZero) => "0".to_string(),
                Some(DecimalSpecial::NegZero) => "-0".to_string(),
                None => format!("{coeff}e{exp10}"),
            };
            if special.is_none() && !python_decimal_round_trips_exactly(coeff, *exp10, *special) {
                py_string_literal(&render_number_fallback(value))
            } else {
                format!(
                    "__import__('decimal').Decimal({})",
                    py_string_literal(&literal)
                )
            }
        }
        ONumber::BinaryFloat {
            format: FloatFormat::F32,
            bits,
        } if bits.len() == 4 => {
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(bits);
            let value = f32::from_bits(u32::from_be_bytes(raw)) as f64;
            if value.is_finite() {
                return render_float_literal(value);
            }
            let bytes = bits
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("__import__('struct').unpack('>f', bytes([{bytes}]))[0]")
        }
        ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits,
        } if bits.len() == 8 => {
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(bits);
            let value = f64::from_bits(u64::from_be_bytes(raw));
            if value.is_finite() {
                return render_float_literal(value);
            }
            let bytes = bits
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("__import__('struct').unpack('>d', bytes([{bytes}]))[0]")
        }
        ONumber::BinaryFloat { .. } => py_string_literal(&render_number_fallback(value)),
        ONumber::BigFloat { .. } | ONumber::Complex { .. } => {
            py_string_literal(&render_number_fallback(value))
        }
    }
}

fn py_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "''".to_string())
}

// ── HTML ─────────────────────────────────────────────────────────────────────

fn render_html(val: &OValue) -> String {
    match val {
        OValue::Null => String::new(),

        OValue::Bool { v } => html_escape(&v.to_string()),
        OValue::Number { .. } => html_escape(&val.splice_repr()),

        // Plain strings are untrusted text — escape them. Trusted raw HTML
        // must arrive as OValue::Html (the "trusted HTML fragment" type per
        // SPEC.md), e.g. produced by an inner html^(...)_html block.
        OValue::Text { v } => html_escape(&v.utf8),
        OValue::Char { scalar } => html_escape(&scalar.to_string()),
        OValue::Html { v } => v.clone(),

        OValue::StorePath { path } => {
            format!("<code class=\"o-store-path\">{}</code>", html_escape(path))
        }

        OValue::List { v } => {
            let items = v
                .iter()
                .map(|item| format!("<li>{}</li>", render_html(item)))
                .collect::<Vec<_>>()
                .join("");
            format!("<ul>{}</ul>", items)
        }
        OValue::Seq { items, .. } | OValue::Set { items, .. } => {
            let items = items
                .iter()
                .map(|item| format!("<li>{}</li>", render_html(item)))
                .collect::<Vec<_>>()
                .join("");
            format!("<ul>{}</ul>", items)
        }

        OValue::Map { v } => sorted_map_entries(v)
            .into_iter()
            .map(|(k, val)| {
                format!(
                    "<div data-o-key=\"{}\">{}</div>",
                    html_escape(k),
                    render_html(val)
                )
            })
            .collect::<Vec<_>>()
            .join(""),
        OValue::Object { fields } => fields
            .iter()
            .map(|(k, val)| {
                format!(
                    "<div data-o-key=\"{}\">{}</div>",
                    html_escape(k),
                    render_html(val)
                )
            })
            .collect::<Vec<_>>()
            .join(""),
        OValue::EntriesMap { entries } => entries
            .iter()
            .map(|(key, value)| {
                format!(
                    "<div data-o-entry><span>{}</span>{}</div>",
                    render_html(key),
                    render_html(value)
                )
            })
            .collect::<Vec<_>>()
            .join(""),
        OValue::Symbol { .. } | OValue::Keyword { .. } => html_escape(&val.splice_repr()),

        OValue::Scope { bindings } => {
            format!(
                "<code class=\"o-scope\" data-bindings=\"{}\">&lt;scope&gt;</code>",
                bindings.len()
            )
        }

        OValue::Blob { v, mime } => render_html_blob(v, mime),
        OValue::Bytes { v } => render_html_blob(
            &B64.encode(&v.bytes),
            v.media_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
        ),
        OValue::Graph { root, nodes } => {
            format!(
                "<code class=\"o-graph\" data-root=\"{}\" data-nodes=\"{}\">&lt;graph&gt;</code>",
                root,
                nodes.len(),
            )
        }
        OValue::Native { v } => {
            format!(
                "<code class=\"o-native\" data-lang=\"{}\" data-codec=\"{}\">{}</code>",
                html_escape(&v.lang),
                html_escape(&v.codec),
                html_escape(&v.type_name),
            )
        }

        OValue::NixExpr {
            body, fingerprint, ..
        } => {
            format!(
                "<code class=\"o-nix-expr\" data-fp=\"{}\">{}</code>",
                html_escape(fingerprint),
                html_escape(body),
            )
        }

        OValue::Derivation {
            drv_path, outputs, ..
        } => {
            format!(
                "<code class=\"o-derivation\" data-outputs=\"{}\">{}</code>",
                html_escape(&outputs.join(",")),
                html_escape(drv_path),
            )
        }

        OValue::Request { fingerprint, .. } => {
            format!(
                "<code class=\"o-request\" data-fp=\"{}\">&lt;request&gt;</code>",
                html_escape(fingerprint_preview(fingerprint)),
            )
        }
        OValue::Thunk {
            body, fingerprint, ..
        } => {
            format!(
                "<code class=\"o-thunk\" data-fp=\"{}\">{}</code>",
                html_escape(fingerprint_preview(fingerprint)),
                html_escape(body),
            )
        }
        OValue::System { profile_path } => {
            format!(
                "<code class=\"o-system\">{}</code>",
                html_escape(profile_path),
            )
        }
        OValue::Capability {
            kind,
            identity,
            metadata,
        } => {
            format!(
                "<code class=\"o-capability\" data-kind=\"{}\" data-meta=\"{}\">{}</code>",
                html_escape(kind.name()),
                metadata.len(),
                html_escape(identity),
            )
        }
        OValue::Snapshot {
            kind,
            identity,
            state,
        } => {
            format!(
                "<code class=\"o-snapshot\" data-kind=\"{}\" data-fields=\"{}\">{}</code>",
                html_escape(kind.name()),
                state.len(),
                html_escape(identity),
            )
        }

        OValue::Group {
            mode,
            members,
            fingerprint,
        } => {
            format!(
                "<code class=\"o-group\" data-mode=\"{}\" data-fp=\"{}\">&lt;group n={}&gt;</code>",
                html_escape(mode.name()),
                html_escape(fingerprint_preview(fingerprint)),
                members.len(),
            )
        }

        OValue::Expr { src } => {
            // Render an OExpr as a <code> block showing the quoted source.
            // Users should O.eval() it rather than splice it into HTML, but
            // we provide a readable fallback so debugging is easier.
            format!("<code class=\"o-expr\">{}</code>", html_escape(src),)
        }

        // An error outcome in HTML renders as a styled error span.
        OValue::Error { msg } => {
            format!(
                "<span class=\"o-error\" role=\"alert\">{}</span>",
                html_escape(msg),
            )
        }
    }
}

fn render_html_blob(b64: &str, mime: &str) -> String {
    if mime.starts_with("image/") {
        // Inline data URI — the standard way to embed binary images in HTML
        // without a separate file.  Matches the Python HtmlBackend exactly.
        return format!("<img src=\"data:{};base64,{}\" />", mime, b64);
    }

    if mime == "text/html" {
        // The blob carries raw HTML bytes.  Decode and embed directly.
        if let Ok(bytes) = B64.decode(b64) {
            if let Ok(text) = std::str::from_utf8(&bytes) {
                return text.to_string();
            }
        }
        return format!("<!-- blob decode error: {} -->", mime);
    }

    if mime.starts_with("text/") {
        // Escaped plain text embedded in HTML.
        if let Ok(bytes) = B64.decode(b64) {
            if let Ok(text) = std::str::from_utf8(&bytes) {
                return html_escape(text);
            }
        }
    }

    // Generic binary: data URI link.
    format!(
        "<a href=\"data:{};base64,{}\">[blob {}, {} bytes (base64)]</a>",
        mime,
        b64,
        mime,
        b64.len() * 3 / 4, // approximate decoded byte count
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── LaTeX ─────────────────────────────────────────────────────────────────────

fn render_latex(val: &OValue) -> String {
    match val {
        OValue::Null => String::new(),
        OValue::Bool { v } => v.to_string(),
        OValue::Number { .. } => val.splice_repr(),
        OValue::Text { v } => v.utf8.clone(),
        OValue::Char { scalar } => scalar.to_string(),
        OValue::Html { v } => v.clone(),
        OValue::StorePath { path } => {
            format!("\\texttt{{{}}}", path.replace("_", "\\_"))
        }
        OValue::List { v } => v.iter().map(render_latex).collect::<Vec<_>>().join(", "),
        OValue::Seq { items, .. } | OValue::Set { items, .. } => items
            .iter()
            .map(render_latex)
            .collect::<Vec<_>>()
            .join(", "),
        OValue::Map { v } => sorted_map_entries(v)
            .into_iter()
            .map(|(k, val)| format!("{}: {}", k, render_latex(val)))
            .collect::<Vec<_>>()
            .join(", "),
        OValue::Object { fields } => fields
            .iter()
            .map(|(k, val)| format!("{}: {}", k, render_latex(val)))
            .collect::<Vec<_>>()
            .join(", "),
        OValue::EntriesMap { entries } => entries
            .iter()
            .map(|(key, value)| format!("{} => {}", render_latex(key), render_latex(value)))
            .collect::<Vec<_>>()
            .join(", "),
        OValue::Symbol { .. } | OValue::Keyword { .. } => val.splice_repr(),
        OValue::Scope { bindings } => {
            format!("\\texttt{{<scope bindings={}>}}", bindings.len())
        }
        OValue::Blob { mime, .. } => format!("\\texttt{{<blob:{}>}}", mime),
        OValue::Bytes { v } => format!("\\texttt{{<bytes:{}>}}", v.bytes.len()),
        OValue::Graph { root, nodes } => {
            format!("\\texttt{{<graph root={} nodes={}>}}", root, nodes.len())
        }
        OValue::Native { v } => {
            format!(
                "\\texttt{{<native:{} {}>}}",
                v.lang.replace("_", "\\_"),
                v.type_name.replace("_", "\\_")
            )
        }
        OValue::NixExpr { body, .. } => format!("\\texttt{{{}}}", body.replace("_", "\\_")),
        OValue::Derivation { drv_path, .. } => {
            format!("\\texttt{{{}}}", drv_path.replace("_", "\\_"))
        }
        OValue::Request { fingerprint, .. } => {
            format!(
                "\\texttt{{<request fp={}>}}",
                fingerprint_preview(fingerprint)
            )
        }
        OValue::Thunk { body, .. } => {
            format!("\\texttt{{{}}}", body.replace("_", "\\_"))
        }
        OValue::System { profile_path } => {
            format!("\\texttt{{{}}}", profile_path.replace("_", "\\_"))
        }
        OValue::Capability { kind, identity, .. } => {
            format!(
                "\\texttt{{<capability:{} {}>}}",
                kind.name(),
                identity.replace("_", "\\_")
            )
        }
        OValue::Snapshot { kind, identity, .. } => {
            format!(
                "\\texttt{{<snapshot:{} {}>}}",
                kind.name(),
                identity.replace("_", "\\_")
            )
        }
        OValue::Group {
            mode,
            members,
            fingerprint,
        } => {
            format!(
                "\\texttt{{<group:{} n={} fp={}>}}",
                mode.name(),
                members.len(),
                fingerprint_preview(fingerprint)
            )
        }
        OValue::Expr { src } => {
            format!("\\texttt{{{}}}", src.replace("_", "\\_"))
        }
        OValue::Error { msg } => {
            format!("\\texttt{{<error: {}>}}", msg.replace("_", "\\_"))
        }
    }
}

// ── Markdown ──────────────────────────────────────────────────────────────────

fn render_markdown(val: &OValue) -> String {
    match val {
        OValue::Null => String::new(),
        OValue::Bool { v } => v.to_string(),
        OValue::Number { .. } => val.splice_repr(),
        OValue::Text { v } => v.utf8.clone(),
        OValue::Char { scalar } => scalar.to_string(),
        OValue::Html { v } => v.clone(),
        OValue::StorePath { path } => format!("`{}`", path),
        OValue::List { v } => v.iter().map(render_markdown).collect::<Vec<_>>().join("\n"),
        OValue::Seq { items, .. } | OValue::Set { items, .. } => items
            .iter()
            .map(render_markdown)
            .collect::<Vec<_>>()
            .join("\n"),
        OValue::Map { v } => sorted_map_entries(v)
            .into_iter()
            .map(|(k, val)| format!("**{}**: {}", k, render_markdown(val)))
            .collect::<Vec<_>>()
            .join("\n"),
        OValue::Object { fields } => fields
            .iter()
            .map(|(k, val)| format!("**{}**: {}", k, render_markdown(val)))
            .collect::<Vec<_>>()
            .join("\n"),
        OValue::EntriesMap { entries } => entries
            .iter()
            .map(|(key, value)| format!("{} => {}", render_markdown(key), render_markdown(value)))
            .collect::<Vec<_>>()
            .join("\n"),
        OValue::Symbol { .. } | OValue::Keyword { .. } => val.splice_repr(),
        OValue::Scope { bindings } => format!("`<scope bindings={}>`", bindings.len()),
        OValue::Blob { mime, .. } => format!("<blob:{}>", mime),
        OValue::Bytes { v } => format!("<bytes:{}>", v.bytes.len()),
        OValue::Graph { root, nodes } => format!("`<graph root={} nodes={}>`", root, nodes.len()),
        OValue::Native { v } => format!("`<native:{} {}>`", v.lang, v.type_name),
        OValue::NixExpr { body, .. } => format!("`{}`", body),
        OValue::Derivation { drv_path, .. } => format!("`{}`", drv_path),
        OValue::Request { fingerprint, .. } => {
            format!("`<request fp={}>`", fingerprint_preview(fingerprint))
        }
        OValue::Thunk { body, .. } => {
            format!("`{}`", body)
        }
        OValue::System { profile_path } => {
            format!("`{}`", profile_path)
        }
        OValue::Capability { kind, identity, .. } => {
            format!("`<capability:{} {}>`", kind.name(), identity)
        }
        OValue::Snapshot { kind, identity, .. } => {
            format!("`<snapshot:{} {}>`", kind.name(), identity)
        }
        OValue::Group {
            mode,
            members,
            fingerprint,
        } => {
            format!(
                "`<group:{} n={} fp={}>`",
                mode.name(),
                members.len(),
                fingerprint_preview(fingerprint)
            )
        }
        OValue::Expr { src } => {
            format!("`{}`", src)
        }
        OValue::Error { msg } => {
            format!("`<error: {}>`", msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use super::{weaker_for_container, ExecutionTrace, RenderFidelity, TraceEvent};

    #[test]
    fn renderer_local_container_combiner_obeys_semilattice_laws() {
        let renderer_local_domains: [&[RenderFidelity]; 4] = [
            &[RenderFidelity::Typed, RenderFidelity::Structural],
            &[
                RenderFidelity::Typed,
                RenderFidelity::Structural,
                RenderFidelity::Opaque,
            ],
            &[RenderFidelity::Presentation, RenderFidelity::Opaque],
            &[RenderFidelity::Structural, RenderFidelity::Opaque],
        ];

        for domain in renderer_local_domains {
            for &a in domain {
                assert_eq!(weaker_for_container(a, a), a);
                for &b in domain {
                    assert_eq!(weaker_for_container(a, b), weaker_for_container(b, a),);
                    for &c in domain {
                        assert_eq!(
                            weaker_for_container(weaker_for_container(a, b), c),
                            weaker_for_container(a, weaker_for_container(b, c)),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn eval_compatibility_exports_are_the_canonical_core_types() {
        assert_eq!(
            TypeId::of::<ExecutionTrace>(),
            TypeId::of::<crate::eval::ExecutionTrace>()
        );
        assert_eq!(
            TypeId::of::<TraceEvent>(),
            TypeId::of::<crate::eval::TraceEvent>()
        );
        assert_eq!(
            TypeId::of::<RenderFidelity>(),
            TypeId::of::<crate::eval::RenderFidelity>()
        );

        let canonical = ExecutionTrace::new();
        let compatibility: crate::eval::ExecutionTrace = canonical;
        let _: ExecutionTrace = compatibility;
    }
}
