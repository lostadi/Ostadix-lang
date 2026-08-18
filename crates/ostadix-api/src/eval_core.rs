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

use crate::backend_catalog::{BackendInterface, SpliceRenderer};
use crate::capability::BackendSandboxPolicy;
use crate::evidence::AdmittedExecution;
use crate::execution_contract::Policy;
use crate::ir::{ExecutionPlan, InvokeMode, OIr, PlanEdgeKind, PlanNodeId, PlanNodeKind};
use crate::value::{DecimalSpecial, FloatFormat, FloatSpecial, ONumber, OValue, SeqKind};

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
    /// The payload is retained, but one or more O-level type tags are erased.
    Structural,
    /// The renderer intentionally produces a human-facing presentation.
    Presentation,
    /// Only an identifying marker or summary survives.
    Opaque,
}

fn container_fidelity<'a>(
    renderer: SpliceRenderer,
    children: impl IntoIterator<Item = &'a OValue>,
    base: RenderFidelity,
) -> RenderFidelity {
    children
        .into_iter()
        .map(|child| render_fidelity(renderer, child))
        .fold(base, |current, child| match (current, child) {
            (RenderFidelity::Opaque, _) | (_, RenderFidelity::Opaque) => RenderFidelity::Opaque,
            (RenderFidelity::Presentation, _) | (_, RenderFidelity::Presentation) => {
                RenderFidelity::Presentation
            }
            (RenderFidelity::Structural, _) | (_, RenderFidelity::Structural) => {
                RenderFidelity::Structural
            }
            _ => RenderFidelity::Typed,
        })
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

pub fn render_fidelity(renderer: SpliceRenderer, val: &OValue) -> RenderFidelity {
    use OValue::*;
    use RenderFidelity::*;

    match renderer {
        SpliceRenderer::Python => match val {
            Null
            | Bool { .. }
            | Number { .. }
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
            Text { .. } | Char { .. } | Bytes { .. } | Symbol { .. } | Keyword { .. } => Structural,
            List { v } => container_fidelity(renderer, v, Typed),
            Map { v } => container_fidelity(renderer, v.values(), Typed),
            Seq { items, .. } | Set { items, .. } => container_fidelity(renderer, items, Typed),
            Object { fields } => container_fidelity(renderer, fields.values(), Typed),
            EntriesMap { entries } => entry_container_fidelity(renderer, entries, Structural),
            Blob { .. } => Structural,
        },
        SpliceRenderer::Nix => match val {
            Null | Bool { .. } | Number { .. } | Text { .. } | NixExpr { .. } => Typed,
            List { v } => container_fidelity(renderer, v, Typed),
            Map { v } => container_fidelity(renderer, v.values(), Typed),
            Seq { items, .. } | Set { items, .. } => container_fidelity(renderer, items, Typed),
            Object { fields } => container_fidelity(renderer, fields.values(), Typed),
            EntriesMap { entries } => entry_container_fidelity(renderer, entries, Structural),
            Char { .. }
            | Bytes { .. }
            | Symbol { .. }
            | Keyword { .. }
            | Graph { .. }
            | Native { .. } => Structural,
            Html { .. }
            | StorePath { .. }
            | Blob { .. }
            | Derivation { .. }
            | System { .. }
            | Expr { .. } => Structural,
            Scope { .. }
            | Request { .. }
            | Capability { .. }
            | Snapshot { .. }
            | Thunk { .. }
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
            | Thunk { .. }
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
            | Bytes { .. }
            | Symbol { .. }
            | Keyword { .. } => Structural,
            List { v } => container_fidelity(renderer, v, Structural),
            Map { v } => container_fidelity(renderer, v.values(), Structural),
            Seq { items, .. } | Set { items, .. } => {
                container_fidelity(renderer, items, Structural)
            }
            Object { fields } => container_fidelity(renderer, fields.values(), Structural),
            EntriesMap { entries } => entry_container_fidelity(renderer, entries, Structural),
            Html { .. }
            | StorePath { .. }
            | Expr { .. }
            | Scope { .. }
            | Blob { .. }
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
            | Error { .. } => Opaque,
        },
    }
}

fn sorted_map_entries(values: &HashMap<String, OValue>) -> Vec<(&String, &OValue)> {
    let mut entries = values.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| *key);
    entries
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
        OValue::Text { v } => serde_json::to_string(&v.utf8).unwrap_or_else(|_| "\"".to_string()),
        OValue::Char { scalar } => {
            serde_json::to_string(&scalar.to_string()).unwrap_or_else(|_| "\"".to_string())
        }
        OValue::Html { v } => serde_json::to_string(v).unwrap_or_else(|_| "\"".to_string()),
        OValue::StorePath { path } => {
            serde_json::to_string(path).unwrap_or_else(|_| "\"".to_string())
        }
        OValue::List { v } => {
            let items = v.iter().map(render_nix).collect::<Vec<_>>().join(" ");
            format!("[ {} ]", items)
        }
        OValue::Map { v } => {
            let items = sorted_map_entries(v)
                .into_iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_string());
                    format!("{} = {};", key, render_nix(val))
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
            let items = fields
                .iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_string());
                    format!("{} = {};", key, render_nix(val))
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
        OValue::Symbol { .. } | OValue::Keyword { .. } => {
            serde_json::to_string(&val.splice_repr()).unwrap_or_else(|_| "\"\"".to_string())
        }
        OValue::Scope { bindings } => {
            format!("\"<scope bindings={}>\"", bindings.len())
        }
        OValue::Blob { v, .. } => serde_json::to_string(v).unwrap_or_else(|_| "\"".to_string()),
        OValue::Bytes { .. } | OValue::Graph { .. } | OValue::Native { .. } => {
            serde_json::to_string(&val.splice_repr()).unwrap_or_else(|_| "\"\"".to_string())
        }
        // An ONixExpr spliced into a Nix context is its already-assembled body —
        // it is a valid Nix expression that can be parenthesised inline.
        OValue::NixExpr { body, .. } => format!("({})", body),
        // A Derivation in a Nix context is its .drv path literal.
        OValue::Derivation { drv_path, .. } => {
            serde_json::to_string(drv_path).unwrap_or_else(|_| "\"".to_string())
        }
        // A Request rendered into Nix source is almost certainly a user error —
        // the user spliced a control value into source text. We embed the
        // splice marker; STEP3 can elevate this to a hard error or auto-resolve.
        OValue::Request { fingerprint, .. } => {
            // STEP-3.5: in a Nix context, an unforced Request is almost
            // always a user error. We emit a string marker that nix eval
            // will reject loudly. {lazy} Eval requests should have been
            // auto-forced before reaching here; {defer} should have errored;
            // Instantiate/Realise have no sensible Nix-context splice form.
            format!("\"<request fp={}>\"", &fingerprint[..8])
        }
        // A Thunk in a Nix context renders as its body, parenthesised. Same
        // treatment as NixExpr — if the lang matches Nix syntax, this is
        // safe; otherwise the user composed two different languages and
        // gets predictable Nix parse errors.
        OValue::Thunk { body, .. } => format!("({})", body),

        // A Group is a control/topology value with no Nix splice form — render
        // a string marker that nix eval will reject loudly, same treatment as
        // an unforced Request. Force the group with `now(...)` before splicing.
        OValue::Group {
            mode, fingerprint, ..
        } => {
            format!("\"<group:{} fp={}>\"", mode.name(), &fingerprint[..8])
        }

        // A System in a Nix context renders as its profile path as a string
        // literal. Useful for Nix expressions that want to inspect or compare
        // against the live profile location.
        OValue::System { profile_path } => {
            serde_json::to_string(profile_path).unwrap_or_else(|_| "\"\"".to_string())
        }

        OValue::Capability { kind, identity, .. } => {
            serde_json::to_string(&format!("<capability:{} {}>", kind.name(), identity))
                .unwrap_or_else(|_| "\"\"".to_string())
        }

        OValue::Snapshot { kind, identity, .. } => {
            serde_json::to_string(&format!("<snapshot:{} {}>", kind.name(), identity))
                .unwrap_or_else(|_| "\"\"".to_string())
        }

        // An Expr in Nix context renders its quoted source as a Nix string
        // literal. Rarely useful — the user almost always wants O.eval first.
        OValue::Expr { src } => serde_json::to_string(src).unwrap_or_else(|_| "\"\"".to_string()),

        // An error outcome in a Nix context renders as a string marker that
        // nix eval will reject loudly — errors should not reach Nix source.
        OValue::Error { msg } => format!("\"<error: {}>\"", msg.replace('"', "\\\"")),
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
            let items = items
                .iter()
                .map(render_python)
                .collect::<Vec<_>>()
                .join(", ");
            format!("set([{}])", items)
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
        ONumber::Int { v } => v.to_string(),
        ONumber::BinaryFloat {
            format: FloatFormat::F32,
            bits,
        } if bits.len() == 4 => {
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(bits);
            render_float_literal(f32::from_bits(u32::from_be_bytes(raw)) as f64)
        }
        ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits,
        } if bits.len() == 8 => {
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(bits);
            render_float_literal(f64::from_bits(u64::from_be_bytes(raw)))
        }
        other => serde_json::to_string(&render_number_fallback(other))
            .unwrap_or_else(|_| "\"\"".to_string()),
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
        ONumber::Int { v } => v.to_string(),
        ONumber::Rational { num, den } => {
            format!("__import__('fractions').Fraction({}, {})", num, den)
        }
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
            format!(
                "__import__('decimal').Decimal({})",
                py_string_literal(&literal)
            )
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
        ONumber::BinaryFloat { format, bits } => {
            let unpack = match format {
                FloatFormat::F32 => ">f",
                FloatFormat::F64 => ">d",
            };
            let bytes = bits
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("__import__('struct').unpack('{unpack}', bytes([{bytes}]))[0]")
        }
        ONumber::BigFloat {
            mantissa,
            exp2,
            precision: _,
            special,
        } => {
            if let Some(special) = special {
                let literal = match special {
                    FloatSpecial::Nan => "NaN",
                    FloatSpecial::PosInf => "Infinity",
                    FloatSpecial::NegInf => "-Infinity",
                    FloatSpecial::PosZero => "0",
                    FloatSpecial::NegZero => "-0",
                };
                return format!(
                    "__import__('decimal').Decimal({})",
                    py_string_literal(literal)
                );
            }

            let mantissa_expr = format!(
                "__import__('decimal').Decimal({})",
                py_string_literal(&mantissa.to_string())
            );
            if *exp2 == 0 {
                mantissa_expr
            } else {
                format!("({mantissa_expr} * (__import__('decimal').Decimal(2) ** {exp2}))")
            }
        }
        ONumber::Complex { re, im } => {
            format!(
                "complex({}, {})",
                render_python_number(re),
                render_python_number(im)
            )
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
                html_escape(&fingerprint[..8]),
            )
        }
        OValue::Thunk {
            body, fingerprint, ..
        } => {
            format!(
                "<code class=\"o-thunk\" data-fp=\"{}\">{}</code>",
                html_escape(&fingerprint[..8]),
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
                html_escape(&fingerprint[..8]),
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
            format!("\\texttt{{<request fp={}>}}", &fingerprint[..8])
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
                &fingerprint[..8]
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
            format!("`<request fp={}>`", &fingerprint[..8])
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
                &fingerprint[..8]
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

    use super::{ExecutionTrace, RenderFidelity, TraceEvent};

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
