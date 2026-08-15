// ─────────────────────────────────────────────────────────────────────────────
// ir.rs — the executable Ostadix-lang intermediate representation.
//
// This module is the stable seam between four concerns that were previously
// fused inside parser.rs / eval.rs / olangc.rs:
//
//   1. Syntax            — ONode, produced by the parser.
//   2. Execution plan    — OIr / OIrProgram, a lowered, backend-neutral form
//                          of the program (this module).
//   3. Runtime values    — OValue, produced by the evaluator.
//   4. Backend metadata  — re-exported compatibility facade over
//                          registry::bundle for existing callers.
//
// Non-goals (deliberately out of scope for this layer):
//   - no native codegen from OIR
//   - no optimizer, no SSA, no LLVM, no VM
//
// ONode is syntax only. Every hosted execution lowers to OIR, builds and
// validates an ExecutionPlan, and interprets OIR. Backend execution mode,
// purity, and splice rendering are frozen into each Exec instruction during
// lowering so analysis and runtime dispatch cannot silently diverge.
// ─────────────────────────────────────────────────────────────────────────────

use crate::environment::EnvironmentRefV2;
use crate::parser::ONode;
use crate::value::GroupMode;
use std::collections::{BTreeSet, HashMap};

pub use crate::registry::bundle::{
    BackendAdapterKind, BackendInterface, BackendRegistry, BackendSpec, BackendValueCapabilities,
    ExecutionMode, IntegerExactness, RichNumberPreservation, RuntimeRequirementPrecision,
    RuntimeRequirementSpec, SpliceRenderer, BACKEND_CATALOG_CURRENT_SCHEMA,
    BACKEND_CATALOG_SCHEMA_V1, BACKEND_CATALOG_SCHEMA_V3, BACKEND_CATALOG_SCHEMA_V4,
};

// ═════════════════════════════════════════════════════════════════════════════
// OIr — the lowered instruction forms
// ═════════════════════════════════════════════════════════════════════════════

/// Evaluation policy carried by an Invoke instruction. Special-form behavior
/// is fixed during lowering instead of being rediscovered from a string by the
/// evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvokeMode {
    Eager,
    Lazy,
    Autonomous,
    Group(GroupMode),
}

impl InvokeMode {
    pub(crate) fn for_name(name: &str) -> Self {
        match name {
            "lazy" => Self::Lazy,
            "autonomous" => Self::Autonomous,
            "batch" => Self::Group(GroupMode::Batch),
            "all" => Self::Group(GroupMode::All),
            "any" => Self::Group(GroupMode::Any),
            "race" => Self::Group(GroupMode::Race),
            _ => Self::Eager,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Eager => "eager",
            Self::Lazy => "lazy",
            Self::Autonomous => "autonomous",
            Self::Group(GroupMode::Batch) => "group:batch",
            Self::Group(GroupMode::All) => "group:all",
            Self::Group(GroupMode::Any) => "group:any",
            Self::Group(GroupMode::Race) => "group:race",
        }
    }
}

/// One executable OIR instruction. The tree shape preserves lexical and
/// structural evaluation regions while `ExecutionPlan` makes dependencies
/// and legal scheduling order explicit.
// Keep this public AST's direct variant ownership stable. Boxing only the
// largest variant would churn every constructor and pattern for a size hint.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum OIr {
    /// Verbatim text destined for a backend splice buffer.
    Text(String),

    /// Read a variable from scope (`$name`).
    Load(String),

    /// Bind the result of `expr` to `name` in scope (`let name = expr`).
    Store { name: String, expr: Box<OIr> },

    /// Invoke a built-in O-level function (`instantiate(...)`, `now(...)`, …).
    Invoke {
        fn_name: String,
        mode: InvokeMode,
        args: Vec<OIr>,
    },

    /// Execute a typed-expression block on backend `lang`.
    Exec {
        lang: String,
        env_id: u32,
        attr: Option<String>,
        backend: BackendInterface,
        body: Vec<OIr>,
    },
}

/// A whole lowered program: the IR form of a parsed `.O` document.
#[derive(Debug, Clone, PartialEq)]
pub struct OIrProgram {
    pub nodes: Vec<OIr>,
}

impl OIrProgram {
    /// Lower a parsed ONode forest into an OIrProgram.
    pub fn lower(nodes: &[ONode]) -> Self {
        Self {
            nodes: nodes.iter().map(lower_node).collect(),
        }
    }

    /// Human-readable dump used by `olangc --target ir`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("; OIrProgram\n");
        for node in &self.nodes {
            dump_node(node, 0, &mut out);
        }
        out.push('\n');
        out.push_str(&self.plan().to_text());
        out
    }

    /// Build the canonical execution plan for this program.
    ///
    /// The plan is a dependency graph over OIR nodes:
    ///   - structural edges capture child → parent evaluation dependencies
    ///   - sequence edges preserve left-to-right source order
    ///   - data edges connect `load $x` to the latest dominating `store $x`
    ///
    /// This is the planning surface used by the evaluator. It is also the
    /// designated home for scheduling, batching, purity-aware reordering, and
    /// future code generation.
    pub fn plan(&self) -> ExecutionPlan {
        let mut builder = PlanBuilder::new();
        let mut scope_stack = vec![std::collections::HashMap::new()];
        let mut previous_sibling = None;
        let mut roots = Vec::new();

        for node in &self.nodes {
            let id = builder.add_node(node, &mut scope_stack, None, previous_sibling);
            roots.push(id);
            previous_sibling = Some(id);
        }

        builder.finish(roots)
    }

    /// Return the executable OIR nodes in the same preorder used by
    /// `ExecutionPlan` node allocation.
    ///
    /// Quoted bodies are deliberately skipped: `quote^` owns its body as syntax,
    /// so nested expressions inside it are not executable plan nodes.
    pub fn flatten_for_plan(&self) -> Vec<&OIr> {
        flatten_nodes_for_plan(&self.nodes)
    }

    /// Build the value-node/operation-edge hypergraph for this program from
    /// the canonical execution plan.
    pub fn hgraph(&self) -> crate::hgraph::HGraph {
        let plan = self.plan();
        self.hgraph_for_plan(&plan)
            .expect("freshly-built OIR execution plan should project to HGraph")
    }

    /// Project an already-validated execution plan into the hypergraph
    /// substrate. This keeps dependency ownership in `ExecutionPlan`; HGraph
    /// is a scheduling/type/fidelity projection over that plan.
    pub fn hgraph_for_plan(&self, plan: &ExecutionPlan) -> Result<crate::hgraph::HGraph, String> {
        crate::hgraph::from_oir::build_program_with_plan(self, plan)
    }
}

fn flatten_nodes_for_plan(nodes: &[OIr]) -> Vec<&OIr> {
    let mut out = Vec::new();
    for node in nodes {
        flatten_node_for_plan(node, &mut out);
    }
    out
}

fn flatten_node_for_plan<'a>(node: &'a OIr, out: &mut Vec<&'a OIr>) {
    out.push(node);
    match node {
        OIr::Text(_) | OIr::Load(_) => {}
        OIr::Store { expr, .. } => flatten_node_for_plan(expr, out),
        OIr::Invoke { args, .. } => {
            for arg in args {
                flatten_node_for_plan(arg, out);
            }
        }
        OIr::Exec { body, .. } if is_quote_exec(node) => {
            let _ = body;
        }
        OIr::Exec { body, .. } => {
            for child in body {
                flatten_node_for_plan(child, out);
            }
        }
    }
}

fn is_quote_exec(node: &OIr) -> bool {
    matches!(
        node,
        OIr::Exec { backend, .. }
            if backend.execution == ExecutionMode::InlineAst && backend.canonical == "quote"
    )
}

/// ONode → OIr lowering. Purely structural; never fails.
pub fn lower_node(node: &ONode) -> OIr {
    match node {
        ONode::RawText(s) => OIr::Text(s.clone()),
        ONode::VarRef(name) => OIr::Load(name.clone()),
        ONode::LetBinding { name, expr } => OIr::Store {
            name: name.clone(),
            expr: Box::new(lower_node(expr)),
        },
        ONode::Call { fn_name, args } => OIr::Invoke {
            fn_name: fn_name.clone(),
            mode: InvokeMode::for_name(fn_name),
            args: args.iter().map(lower_node).collect(),
        },
        ONode::TypedExpr {
            lang,
            env_id,
            attr,
            body,
        } => OIr::Exec {
            lang: lang.clone(),
            env_id: *env_id,
            attr: attr.clone(),
            backend: BackendRegistry::global().interface_for(lang),
            body: body.iter().map(lower_node).collect(),
        },
    }
}

/// Reconstruct executable OIR as parseable O source. This is used by the
/// `quote` instruction, so quotation no longer reaches back into ONode.
pub fn reconstruct_source(nodes: &[OIr]) -> String {
    let mut out = String::new();
    for node in nodes {
        reconstruct_node(node, &mut out);
    }
    out
}

fn reconstruct_node(node: &OIr, out: &mut String) {
    match node {
        OIr::Text(text) => out.push_str(text),
        OIr::Load(name) => {
            out.push('$');
            out.push_str(name);
        }
        OIr::Store { name, expr } => {
            out.push_str("let ");
            out.push_str(name);
            out.push_str(" = ");
            reconstruct_node(expr, out);
        }
        OIr::Invoke { fn_name, args, .. } => {
            out.push_str(fn_name);
            out.push('(');
            for (index, arg) in args.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                reconstruct_node(arg, out);
            }
            out.push(')');
        }
        OIr::Exec {
            lang,
            env_id,
            attr,
            body,
            ..
        } => {
            out.push_str(lang);
            let environment = EnvironmentRefV2::from_encoded(*env_id);
            if let Some(marker) = environment.source_marker() {
                out.push_str(&marker);
            }
            if let Some(attr) = attr {
                out.push('{');
                out.push_str(attr);
                out.push('}');
            }
            out.push_str("^(");
            for child in body {
                reconstruct_node(child, out);
            }
            out.push_str(")_");
            out.push_str(lang);
            if let Some(marker) = environment.source_marker() {
                out.push_str(&marker);
            }
            if let Some(attr) = attr {
                out.push('{');
                out.push_str(attr);
                out.push('}');
            }
        }
    }
}

fn dump_node(node: &OIr, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match node {
        OIr::Text(s) => {
            out.push_str(&format!("{indent}text {s:?}\n"));
        }
        OIr::Load(name) => {
            out.push_str(&format!("{indent}load ${name}\n"));
        }
        OIr::Store { name, expr } => {
            out.push_str(&format!("{indent}store ${name} =\n"));
            dump_node(expr, depth + 1, out);
        }
        OIr::Invoke {
            fn_name,
            mode,
            args,
        } => {
            out.push_str(&format!(
                "{indent}invoke {fn_name}/{} [{}]\n",
                args.len(),
                mode.label()
            ));
            for arg in args {
                dump_node(arg, depth + 1, out);
            }
        }
        OIr::Exec {
            lang,
            env_id,
            attr,
            body,
            ..
        } => {
            let attr_s = attr
                .as_deref()
                .map(|a| format!(" {{{a}}}"))
                .unwrap_or_default();
            let env_s = match EnvironmentRefV2::from_encoded(*env_id) {
                EnvironmentRefV2::Ephemeral => String::new(),
                EnvironmentRefV2::LinkerIsolated => " [env *]".to_string(),
                EnvironmentRefV2::Persistent(id) => format!(" [env {id}]"),
            };
            out.push_str(&format!("{indent}exec {lang}{env_s}{attr_s}\n"));
            for child in body {
                dump_node(child, depth + 1, out);
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// ExecutionPlan — canonical dependency graph over OIR
// ═════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanNodeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEdgeKind {
    Structural,
    Sequence,
    Data,
}

impl PlanEdgeKind {
    fn label(self) -> &'static str {
        match self {
            PlanEdgeKind::Structural => "structural",
            PlanEdgeKind::Sequence => "sequence",
            PlanEdgeKind::Data => "data",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanNodeClass {
    Pure,
    Effect,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    Memoize,
    Bypass,
}

impl CachePolicy {
    pub fn cacheable(self) -> bool {
        match self {
            Self::Memoize => true,
            Self::Bypass => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanRequestKind {
    Instantiate,
    Realise,
    DryActivate,
    Activate,
}

impl PlanRequestKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Instantiate => "instantiate",
            Self::Realise => "realise",
            Self::DryActivate => "dry_activate",
            Self::Activate => "activate",
        }
    }

    pub fn cache_policy(self) -> CachePolicy {
        match self {
            Self::Instantiate | Self::Realise => CachePolicy::Memoize,
            Self::DryActivate | Self::Activate => CachePolicy::Bypass,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanScheduleKind {
    Force,
    Lazy,
    Autonomous,
}

impl PlanScheduleKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Force => "force",
            Self::Lazy => "lazy",
            Self::Autonomous => "autonomous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEdge {
    pub from: PlanNodeId,
    pub to: PlanNodeId,
    pub kind: PlanEdgeKind,
}

// `PlanNodeKind` is a public execution-plan vocabulary; preserve its direct
// variant ownership instead of changing that API solely to equalize sizes.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanNodeKind {
    Text,
    Load {
        name: String,
    },
    Store {
        name: String,
    },
    Call {
        fn_name: String,
        mode: InvokeMode,
        arg_count: usize,
    },
    Request {
        fn_name: String,
        kind: PlanRequestKind,
        arg_count: usize,
    },
    Group {
        mode: GroupMode,
        member_count: usize,
    },
    Schedule {
        fn_name: String,
        kind: PlanScheduleKind,
        arg_count: usize,
    },
    Exec {
        lang: String,
        env_id: u32,
        attr: Option<String>,
        backend: BackendInterface,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanNode {
    pub id: PlanNodeId,
    pub kind: PlanNodeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub roots: Vec<PlanNodeId>,
    pub nodes: Vec<PlanNode>,
    pub edges: Vec<PlanEdge>,
}

impl ExecutionPlan {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("; ExecutionPlan\n");
        if !self.roots.is_empty() {
            let roots = self
                .roots
                .iter()
                .map(|id| id.0.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("roots [{roots}]\n"));
        }
        for node in &self.nodes {
            out.push_str(&format!("node {} {}\n", node.id.0, node.kind.describe()));
        }
        for edge in &self.edges {
            out.push_str(&format!(
                "edge {} -> {} {}\n",
                edge.from.0,
                edge.to.0,
                edge.kind.label()
            ));
        }
        out
    }

    /// Validate plan identity, edge bounds, acyclicity, and root coverage.
    /// Runtime execution calls this before evaluating any instruction.
    pub fn validate(&self, root_count: usize) -> Result<(), String> {
        if self.roots.len() != root_count {
            return Err(format!(
                "execution plan has {} roots for {root_count} OIR instructions",
                self.roots.len()
            ));
        }
        for (index, node) in self.nodes.iter().enumerate() {
            if node.id != PlanNodeId(index) {
                return Err(format!(
                    "execution plan node identity mismatch at {index}: got {}",
                    node.id.0
                ));
            }
        }
        let mut roots = BTreeSet::new();
        for root in &self.roots {
            if root.0 >= self.nodes.len() {
                return Err(format!("execution plan root {} is out of bounds", root.0));
            }
            if !roots.insert(root.0) {
                return Err(format!("execution plan root {} is duplicated", root.0));
            }
        }
        for edge in &self.edges {
            if edge.from.0 >= self.nodes.len() || edge.to.0 >= self.nodes.len() {
                return Err(format!(
                    "execution plan edge {} -> {} is out of bounds",
                    edge.from.0, edge.to.0
                ));
            }
        }
        self.topological_order()?;
        self.root_schedule()?;
        Ok(())
    }

    /// Stable topological order over every planned instruction. Lower node
    /// identifiers win ties so source order remains deterministic whenever
    /// the dependency graph permits more than one schedule.
    pub fn topological_order(&self) -> Result<Vec<PlanNodeId>, String> {
        let mut indegree = vec![0usize; self.nodes.len()];
        let mut successors = vec![Vec::new(); self.nodes.len()];
        for edge in &self.edges {
            indegree[edge.to.0] += 1;
            successors[edge.from.0].push(edge.to.0);
        }

        let mut ready: BTreeSet<usize> = indegree
            .iter()
            .enumerate()
            .filter_map(|(id, degree)| (*degree == 0).then_some(id))
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(id) = ready.iter().next().copied() {
            ready.remove(&id);
            order.push(PlanNodeId(id));
            for successor in &successors[id] {
                indegree[*successor] -= 1;
                if indegree[*successor] == 0 {
                    ready.insert(*successor);
                }
            }
        }
        if order.len() != self.nodes.len() {
            return Err("execution plan dependency graph contains a cycle".to_string());
        }
        Ok(order)
    }

    /// Return top-level OIR indices in their executable dependency order.
    /// The evaluator uses this schedule instead of walking parser nodes.
    pub fn root_schedule(&self) -> Result<Vec<usize>, String> {
        let positions: HashMap<PlanNodeId, usize> = self
            .roots
            .iter()
            .copied()
            .enumerate()
            .map(|(position, id)| (id, position))
            .collect();
        let schedule: Vec<usize> = self
            .topological_order()?
            .into_iter()
            .filter_map(|id| positions.get(&id).copied())
            .collect();
        if schedule.len() != self.roots.len() {
            return Err("execution plan did not schedule every root".to_string());
        }
        Ok(schedule)
    }

    /// Return the direct structural children of `parent` in executable plan
    /// order. Recursive OIR evaluation uses this for every Store, Invoke, and
    /// Exec region rather than assuming vector order independently of the
    /// dependency graph.
    pub fn child_schedule(&self, parent: PlanNodeId) -> Result<Vec<PlanNodeId>, String> {
        if parent.0 >= self.nodes.len() {
            return Err(format!(
                "execution plan parent {} is out of bounds",
                parent.0
            ));
        }
        let children: BTreeSet<PlanNodeId> = self
            .edges
            .iter()
            .filter_map(|edge| {
                (edge.kind == PlanEdgeKind::Structural && edge.to == parent).then_some(edge.from)
            })
            .collect();
        Ok(self
            .topological_order()?
            .into_iter()
            .filter(|id| children.contains(id))
            .collect())
    }
}

impl PlanNodeKind {
    pub fn class(&self) -> PlanNodeClass {
        match self {
            Self::Text | Self::Load { .. } | Self::Store { .. } | Self::Call { .. } => {
                PlanNodeClass::Pure
            }
            Self::Exec { .. } if self.eval_cache_policy().is_some() => PlanNodeClass::Control,
            Self::Exec { .. } => PlanNodeClass::Effect,
            Self::Request { .. } | Self::Group { .. } | Self::Schedule { .. } => {
                PlanNodeClass::Control
            }
        }
    }

    pub fn eval_cache_policy(&self) -> Option<CachePolicy> {
        match self {
            Self::Exec { attr, .. } => parse_eval_cache_policy(attr.as_deref()),
            Self::Request { kind, .. } => Some(kind.cache_policy()),
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            PlanNodeKind::Text => "text".to_string(),
            PlanNodeKind::Load { name } => format!("load ${name}"),
            PlanNodeKind::Store { name } => format!("store ${name}"),
            PlanNodeKind::Call {
                fn_name,
                mode,
                arg_count,
            } => {
                format!("call {fn_name}/{arg_count} [{}]", mode.label())
            }
            PlanNodeKind::Request {
                fn_name,
                kind,
                arg_count,
            } => {
                format!("request {fn_name}/{arg_count} [{}]", kind.label())
            }
            PlanNodeKind::Group { mode, member_count } => {
                format!("group {}/{}", mode.name(), member_count)
            }
            PlanNodeKind::Schedule {
                fn_name,
                kind,
                arg_count,
            } => {
                format!("schedule {fn_name}/{arg_count} [{}]", kind.label())
            }
            PlanNodeKind::Exec {
                lang,
                env_id,
                attr,
                backend,
            } => {
                let attr_s = attr
                    .as_deref()
                    .map(|a| format!(" {{{a}}}"))
                    .unwrap_or_default();
                let env = match EnvironmentRefV2::from_encoded(*env_id) {
                    EnvironmentRefV2::Ephemeral => "ephemeral".to_string(),
                    EnvironmentRefV2::LinkerIsolated => "*".to_string(),
                    EnvironmentRefV2::Persistent(id) => id.to_string(),
                };
                let required = backend
                    .required_authorities
                    .iter()
                    .map(|authority| authority.name())
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "exec {} [env {}]{} backend={} spec={} pure={} renderer={:?} execution={} required=[{}]",
                    lang,
                    env,
                    attr_s,
                    backend.canonical,
                    backend.specification_sha256.as_deref().unwrap_or("unknown"),
                    backend.pure,
                    backend.renderer,
                    backend.execution.label(),
                    required
                )
            }
        }
    }
}

fn parse_eval_cache_policy(attr: Option<&str>) -> Option<CachePolicy> {
    let mut policy = None;
    for entry in attr.into_iter().flat_map(|attr| attr.split(',')) {
        match entry.trim() {
            "lazy" => policy = Some(CachePolicy::Memoize),
            "defer" => policy = Some(CachePolicy::Bypass),
            _ => {}
        }
    }
    policy
}

struct PlanBuilder {
    nodes: Vec<PlanNode>,
    edges: Vec<PlanEdge>,
}

impl PlanBuilder {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    fn finish(self, roots: Vec<PlanNodeId>) -> ExecutionPlan {
        ExecutionPlan {
            roots,
            nodes: self.nodes,
            edges: self.edges,
        }
    }

    fn add_edge(&mut self, from: PlanNodeId, to: PlanNodeId, kind: PlanEdgeKind) {
        if !self
            .edges
            .iter()
            .any(|edge| edge.from == from && edge.to == to && edge.kind == kind)
        {
            self.edges.push(PlanEdge { from, to, kind });
        }
    }

    fn add_node(
        &mut self,
        node: &OIr,
        scope_stack: &mut Vec<std::collections::HashMap<String, PlanNodeId>>,
        parent: Option<PlanNodeId>,
        previous_sibling: Option<PlanNodeId>,
    ) -> PlanNodeId {
        let id = PlanNodeId(self.nodes.len());
        let kind = self.plan_kind(node);
        self.nodes.push(PlanNode { id, kind });

        if let Some(parent_id) = parent {
            self.add_edge(id, parent_id, PlanEdgeKind::Structural);
        }
        if let Some(prev) = previous_sibling {
            self.add_edge(prev, id, PlanEdgeKind::Sequence);
        }

        match node {
            OIr::Text(_) => {}
            OIr::Load(name) => {
                if let Some(source) = scope_stack.iter().rev().find_map(|scope| scope.get(name)) {
                    self.add_edge(*source, id, PlanEdgeKind::Data);
                }
            }
            OIr::Store { name, expr } => {
                scope_stack.push(std::collections::HashMap::new());
                self.add_node(expr, scope_stack, Some(id), None);
                scope_stack.pop();
                scope_stack
                    .last_mut()
                    .expect("scope stack always has a root scope")
                    .insert(name.clone(), id);
            }
            OIr::Invoke { fn_name, args, .. } => {
                // scope() reads every currently visible lexical binding even
                // though it has no syntactic arguments. Record those implicit
                // reads as data dependencies so the plan describes the same
                // semantics the evaluator executes. Inner bindings shadow
                // outer bindings with the same name.
                if fn_name == "scope" {
                    let mut seen = std::collections::HashSet::new();
                    let mut sources = Vec::new();
                    for lexical_scope in scope_stack.iter().rev() {
                        for (name, source) in lexical_scope {
                            if seen.insert(name.clone()) {
                                sources.push(*source);
                            }
                        }
                    }
                    sources.sort_by_key(|source| source.0);
                    for source in sources {
                        self.add_edge(source, id, PlanEdgeKind::Data);
                    }
                }
                scope_stack.push(std::collections::HashMap::new());
                let mut prev = None;
                for arg in args {
                    prev = Some(self.add_node(arg, scope_stack, Some(id), prev));
                }
                scope_stack.pop();
            }
            OIr::Exec {
                attr,
                backend,
                body,
                ..
            } => {
                // Every shim receives the complete visible O scope as native
                // bindings. Keep those dependencies even for an ephemeral
                // process: fresh interpreter state does not erase lexical
                // dataflow.
                if backend.execution == ExecutionMode::Shim {
                    for source in visible_scope_sources(scope_stack) {
                        self.add_edge(source, id, PlanEdgeKind::Data);
                    }
                }
                if let Some(binding) = attr_capability_binding(attr.as_deref()) {
                    if let Some(source) = scope_stack
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(binding.as_str()))
                    {
                        self.add_edge(*source, id, PlanEdgeKind::Data);
                    }
                }
                if backend.execution == ExecutionMode::InlineAst && backend.canonical == "quote" {
                    return id;
                }
                scope_stack.push(std::collections::HashMap::new());
                let mut prev = None;
                for child in body {
                    prev = Some(self.add_node(child, scope_stack, Some(id), prev));
                }
                scope_stack.pop();
            }
        }

        id
    }

    fn plan_kind(&self, node: &OIr) -> PlanNodeKind {
        match node {
            OIr::Text(_) => PlanNodeKind::Text,
            OIr::Load(name) => PlanNodeKind::Load { name: name.clone() },
            OIr::Store { name, .. } => PlanNodeKind::Store { name: name.clone() },
            OIr::Invoke {
                fn_name,
                mode,
                args,
            } => match mode {
                InvokeMode::Group(mode) => PlanNodeKind::Group {
                    mode: *mode,
                    member_count: args.len(),
                },
                InvokeMode::Lazy => PlanNodeKind::Schedule {
                    fn_name: fn_name.clone(),
                    kind: PlanScheduleKind::Lazy,
                    arg_count: args.len(),
                },
                InvokeMode::Autonomous => PlanNodeKind::Schedule {
                    fn_name: fn_name.clone(),
                    kind: PlanScheduleKind::Autonomous,
                    arg_count: args.len(),
                },
                InvokeMode::Eager => match fn_name.as_str() {
                    "instantiate" => PlanNodeKind::Request {
                        fn_name: fn_name.clone(),
                        kind: PlanRequestKind::Instantiate,
                        arg_count: args.len(),
                    },
                    "realise" => PlanNodeKind::Request {
                        fn_name: fn_name.clone(),
                        kind: PlanRequestKind::Realise,
                        arg_count: args.len(),
                    },
                    "dry_activate" => PlanNodeKind::Request {
                        fn_name: fn_name.clone(),
                        kind: PlanRequestKind::DryActivate,
                        arg_count: args.len(),
                    },
                    "activate" => PlanNodeKind::Request {
                        fn_name: fn_name.clone(),
                        kind: PlanRequestKind::Activate,
                        arg_count: args.len(),
                    },
                    "now" => PlanNodeKind::Schedule {
                        fn_name: fn_name.clone(),
                        kind: PlanScheduleKind::Force,
                        arg_count: args.len(),
                    },
                    _ => PlanNodeKind::Call {
                        fn_name: fn_name.clone(),
                        mode: *mode,
                        arg_count: args.len(),
                    },
                },
            },
            OIr::Exec {
                lang,
                env_id,
                attr,
                backend,
                ..
            } => PlanNodeKind::Exec {
                lang: lang.clone(),
                env_id: *env_id,
                attr: attr.clone(),
                backend: backend.clone(),
            },
        }
    }
}

fn visible_scope_sources(
    scope_stack: &[std::collections::HashMap<String, PlanNodeId>],
) -> Vec<PlanNodeId> {
    let mut seen = std::collections::HashSet::new();
    let mut sources = Vec::new();
    for lexical_scope in scope_stack.iter().rev() {
        for (name, source) in lexical_scope {
            if seen.insert(name.clone()) {
                sources.push(*source);
            }
        }
    }
    sources.sort_by_key(|source| source.0);
    sources
}

fn attr_capability_binding(attr: Option<&str>) -> Option<String> {
    attr.into_iter()
        .flat_map(|attr| attr.split(','))
        .map(str::trim)
        .find_map(|entry| {
            entry
                .strip_prefix("cap=")
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use crate::registry::bundle::{
        catalog_hash_field, finish_catalog_hash, hash_backend_spec_v3, hash_backend_spec_v4,
        integer_exactness,
    };
    use crate::value::BackendAuthority;
    use num_bigint::BigInt;
    use sha2::{Digest, Sha256};
    use std::path::Path;

    fn typed(lang: &str, body: Vec<ONode>) -> ONode {
        ONode::TypedExpr {
            lang: lang.to_string(),
            env_id: 0,
            attr: None,
            body,
        }
    }

    #[test]
    fn lower_raw_text() {
        let prog = OIrProgram::lower(&[ONode::RawText("hi".into())]);
        assert_eq!(prog.nodes, vec![OIr::Text("hi".into())]);
    }

    #[test]
    fn lower_nested_typed_expr() {
        let nodes = vec![typed(
            "html",
            vec![
                ONode::RawText("<p>".into()),
                typed("python", vec![ONode::RawText("2 + 2".into())]),
                ONode::VarRef("x".into()),
                ONode::RawText("</p>".into()),
            ],
        )];
        let prog = OIrProgram::lower(&nodes);
        assert_eq!(
            prog.nodes,
            vec![OIr::Exec {
                lang: "html".into(),
                env_id: 0,
                attr: None,
                backend: BackendRegistry::global().interface_for("html"),
                body: vec![
                    OIr::Text("<p>".into()),
                    OIr::Exec {
                        lang: "python".into(),
                        env_id: 0,
                        attr: None,
                        backend: BackendRegistry::global().interface_for("python"),
                        body: vec![OIr::Text("2 + 2".into())],
                    },
                    OIr::Load("x".into()),
                    OIr::Text("</p>".into()),
                ],
            }]
        );
    }

    #[test]
    fn lower_let_and_call() {
        let nodes = vec![ONode::LetBinding {
            name: "drv".into(),
            expr: Box::new(ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef("expr".into())],
            }),
        }];
        let prog = OIrProgram::lower(&nodes);
        assert_eq!(
            prog.nodes,
            vec![OIr::Store {
                name: "drv".into(),
                expr: Box::new(OIr::Invoke {
                    fn_name: "instantiate".into(),
                    mode: InvokeMode::Eager,
                    args: vec![OIr::Load("expr".into())],
                }),
            }]
        );
    }

    #[test]
    fn lowering_types_policy_changing_invocations() {
        for (name, expected) in [
            ("lazy", InvokeMode::Lazy),
            ("autonomous", InvokeMode::Autonomous),
            ("batch", InvokeMode::Group(GroupMode::Batch)),
            ("all", InvokeMode::Group(GroupMode::All)),
            ("any", InvokeMode::Group(GroupMode::Any)),
            ("race", InvokeMode::Group(GroupMode::Race)),
            ("now", InvokeMode::Eager),
        ] {
            let program = OIrProgram::lower(&[ONode::Call {
                fn_name: name.into(),
                args: vec![ONode::RawText("x".into())],
            }]);
            assert!(matches!(
                &program.nodes[0],
                OIr::Invoke { mode, .. } if *mode == expected
            ));
        }
    }

    #[test]
    fn source_lowers_typed_group_members_into_direct_exec_arguments() {
        let source = "autonomous(batch(python^(1)_python, python^(2)_python))";
        let backends = BackendRegistry::global().registered_backend_tags();
        let parsed = Parser::new(source, &backends).parse().unwrap();
        let program = OIrProgram::lower(&parsed);
        let OIr::Invoke {
            mode: InvokeMode::Autonomous,
            args: autonomous_args,
            ..
        } = &program.nodes[0]
        else {
            panic!("expected autonomous invocation")
        };
        let OIr::Invoke {
            mode: InvokeMode::Group(GroupMode::Batch),
            args: members,
            ..
        } = &autonomous_args[0]
        else {
            panic!("expected nested batch invocation")
        };
        assert_eq!(members.len(), 2);
        assert!(members.iter().all(|member| matches!(
            member,
            OIr::Exec {
                env_id: u32::MAX,
                backend,
                ..
            } if backend.canonical == "python"
        )));
    }

    #[test]
    fn ir_dump_is_stable() {
        let nodes = vec![typed("python", vec![ONode::RawText("1 + 1".into())])];
        let prog = OIrProgram::lower(&nodes);
        let python_spec = BackendRegistry::global()
            .specification_sha256("python")
            .expect("python specification digest");
        assert_eq!(
            prog.to_text(),
            format!(
                concat!(
                "; OIrProgram\n",
                "exec python [env 0]\n",
                "  text \"1 + 1\"\n",
                "\n",
                "; ExecutionPlan\n",
                "roots [0]\n",
                "node 0 exec python [env 0] backend=python spec={} pure=false renderer=Python execution=shim required=[]\n",
                "node 1 text\n",
                "edge 1 -> 0 structural\n",
                ),
                python_spec
            )
        );
    }

    #[test]
    fn registry_purity_is_conservative() {
        let reg = BackendRegistry::global();
        // The cache-safe set is limited to deterministic inline
        // representation handlers plus nix_expr's deterministic expression
        // *capture* (which never invokes a shim).
        for lang in ["nix_expr", "html", "markdown", "latex", "text"] {
            assert!(reg.is_pure(lang), "{lang} should be cache-safe");
        }
        // Every unrestricted shim-backed backend is impure: the runtime
        // does not enforce a closed deterministic execution environment,
        // so generic `{lazy}` caching would be unsound.
        for lang in [
            "nix",
            "nix_store",
            "nixos_test",
            "haskell",
            "ocaml",
            "webassembly",
            "python",
            "shell",
            "bash",
            "rust",
            "racket",
            "java",
            "javascript",
            "ruby",
            "sql",
            "O",
            "quote",
            "cobol",
        ] {
            assert!(!reg.is_pure(lang), "{lang} should be impure");
        }
    }

    /// The accepted tag set exposed by the registry contains every
    /// canonical backend name and every declared alias, with no duplicate
    /// or missing entries. Binaries derive their parser tag sets from this
    /// method instead of maintaining copies.
    #[test]
    fn registry_tag_set_covers_all_canonical_names_and_aliases() {
        let reg = BackendRegistry::global();
        let names = reg.registered_backend_names();

        // No duplicates.
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "duplicate tags in registry: {names:?}"
        );

        // Every canonical name and every alias is present.
        for spec in reg.canonical_specs() {
            assert!(
                unique.contains(spec.name),
                "missing canonical name {}",
                spec.name
            );
            for alias in spec.aliases {
                assert!(unique.contains(alias), "missing alias {alias}");
            }
        }

        // Every tag maps back to some spec (nothing extra).
        for tag in &names {
            assert!(reg.get(tag).is_some(), "tag {tag} resolves to no spec");
        }

        // Known aliases used by the parser remain accepted.
        for alias in ["py", "md", "tex", "plain", "o"] {
            assert!(unique.contains(alias), "parser alias {alias} must remain");
        }

        // Owned-tag convenience view agrees.
        let owned = reg.registered_backend_tags();
        assert_eq!(owned.len(), names.len());
    }

    #[test]
    fn canonical_catalog_links_every_backend_to_one_runtime_requirement() {
        let registry = BackendRegistry::global();
        let requirements = registry.runtime_requirement_specs();
        let requirement_keys = requirements
            .iter()
            .map(|requirement| requirement.key)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(requirement_keys.len(), requirements.len());

        let mut referenced = std::collections::BTreeSet::new();
        assert_eq!(registry.canonical_specs().len(), 30);
        for spec in registry.canonical_specs() {
            assert!(
                requirement_keys.contains(spec.runtime_requirement_key),
                "backend {} references missing runtime requirement {}",
                spec.name,
                spec.runtime_requirement_key
            );
            referenced.insert(spec.runtime_requirement_key);
            let requirement = registry.runtime_requirements_for(spec.name);
            match spec.adapter {
                BackendAdapterKind::Inline => {
                    assert_ne!(spec.execution, ExecutionMode::Shim, "{}", spec.name);
                    assert!(requirement.builtin, "{}", spec.name);
                    assert!(requirement.alternatives.is_empty(), "{}", spec.name);
                }
                BackendAdapterKind::NativeRust => {
                    if spec.execution == ExecutionMode::Shim {
                        assert!(!requirement.builtin, "{}", spec.name);
                    } else {
                        assert_eq!(spec.name, "nix_expr");
                        assert!(requirement.builtin, "{}", spec.name);
                    }
                }
                BackendAdapterKind::LegacyPythonShim => {
                    assert_eq!(spec.execution, ExecutionMode::Shim, "{}", spec.name);
                    assert!(!requirement.builtin, "{}", spec.name);
                    assert!(
                        requirement
                            .alternatives
                            .iter()
                            .any(|alternative| alternative.contains(&"python3")),
                        "legacy Python adapter {} must declare python3",
                        spec.name
                    );
                }
            }
        }
        assert_eq!(
            referenced, requirement_keys,
            "runtime requirement groups must not be orphaned"
        );
    }

    #[test]
    fn runtime_requirement_alternatives_preserve_or_of_and_semantics() {
        let registry = BackendRegistry::global();
        let rendered = |lang: &str| {
            registry
                .runtime_requirements_for(lang)
                .alternatives
                .iter()
                .map(|alternative| alternative.join("+"))
                .collect::<Vec<_>>()
                .join("|")
        };

        assert_eq!(rendered("java"), "javac+java");
        assert_eq!(rendered("haskell"), "runghc|ghc");
        assert_eq!(rendered("csharp"), "dotnet|mcs+mono");
        assert_eq!(rendered("webassembly"), "wat2wasm+wasmtime|wat2wasm+wasmer");
        assert_eq!(rendered("unregistered_backend"), "python3");
        assert_eq!(
            registry.runtime_requirements_for("webassembly").precision,
            RuntimeRequirementPrecision::ConservativeAllSources
        );
    }

    #[test]
    fn catalog_adapter_projection_distinguishes_execution_implementations() {
        let registry = BackendRegistry::global();
        for lang in ["O", "quote", "html", "markdown", "latex", "text"] {
            assert_eq!(
                registry.adapter_for(lang),
                BackendAdapterKind::Inline,
                "{lang}"
            );
        }
        for lang in ["python", "py", "nixos_test", "ubuntu_vm", "ubuntu"] {
            assert_eq!(
                registry.adapter_for(lang),
                BackendAdapterKind::LegacyPythonShim,
                "{lang}"
            );
        }
        for lang in ["bash", "sql", "java", "webassembly", "common_lisp"] {
            assert_eq!(
                registry.adapter_for(lang),
                BackendAdapterKind::NativeRust,
                "{lang}"
            );
        }
        assert_eq!(
            registry.adapter_for("nix_expr"),
            BackendAdapterKind::NativeRust
        );
        assert_eq!(
            registry.adapter_for("unknown"),
            BackendAdapterKind::LegacyPythonShim
        );
    }

    #[test]
    fn catalog_digests_are_stable_canonical_projections() {
        let registry = BackendRegistry::global();
        assert_eq!(BACKEND_CATALOG_SCHEMA_V3, "ostadix.backend-catalog/v3");
        assert_eq!(BACKEND_CATALOG_SCHEMA_V4, "ostadix.backend-catalog/v4");
        assert_eq!(BACKEND_CATALOG_CURRENT_SCHEMA, BACKEND_CATALOG_SCHEMA_V4);
        assert_eq!(BACKEND_CATALOG_SCHEMA_V1, BACKEND_CATALOG_SCHEMA_V4);
        let catalog = registry.catalog_sha256();
        assert_eq!(catalog.len(), 64);
        assert!(catalog.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(catalog, registry.catalog_sha256());

        let python = registry.specification_sha256("python").unwrap();
        assert_eq!(python, registry.specification_sha256("py").unwrap());
        assert_ne!(python, registry.specification_sha256("bash").unwrap());
        assert_eq!(python.len(), 64);
        assert!(registry.specification_sha256("unknown").is_none());

        assert!(registry.contains_specification_sha256(&python));
        assert!(
            registry.contains_specification_sha256(&registry.specification_sha256("py").unwrap())
        );
        assert!(!registry.contains_specification_sha256(&"0".repeat(64)));

        let legacy_v3 = registry.specification_sha256_v3("python").unwrap();
        assert_ne!(legacy_v3, python);
        assert_eq!(legacy_v3, registry.specification_sha256_v3("py").unwrap());
        assert!(!registry.contains_specification_sha256(&legacy_v3));
    }

    #[test]
    fn catalog_v3_digest_goldens_are_pinned_before_the_v4_rollover() {
        let registry = BackendRegistry::global();
        assert_eq!(
            registry.catalog_sha256_v3(),
            "c2453ff4cb2480e03a4a0b2356439cbc2a0ddcc914ed948fa9fe91eab1ac79ea"
        );
        let expected = [
            (
                "O",
                "d950d5857e1dc57ea4f2a2ae4603c22809aa3fbb0ae72fb983c78c5a9e632594",
            ),
            (
                "quote",
                "a354062b89cdc800361a22973e7b22029ef3a8ba6c89ae517dd060323fe9584d",
            ),
            (
                "nix",
                "af2e5cb7ca31a435f11e4d963fd72f1602bdc34af22f952e0fa2ab79d5073d4e",
            ),
            (
                "nix_expr",
                "6df666a6848122bf99bd6a6fb0a69ca807dec1324decf3e056a94af43ac6fe5c",
            ),
            (
                "nix_store",
                "de30341e99237888ae48d1acaad65d008281dd6d596b27a9b7bac8472888dd66",
            ),
            (
                "nixos_test",
                "3aab03df2cd680d7525038bb7d8f996abceabf88f0fc1b538f0555469863956f",
            ),
            (
                "html",
                "09c84ed2860b7e489ac7b85019bea022776201f777fa46342ffab3f906eff9ab",
            ),
            (
                "markdown",
                "aee4c1a29734ebfbf378b20e4fc19d7fbfb8365fbcbdd5538d337e3239a0c427",
            ),
            (
                "latex",
                "06ed79952fca6fb4e5221a9f4f9f36a15f17aaef747fdf7bae2fa290d497a6ab",
            ),
            (
                "text",
                "636b9d9237b58af152c2fe92896306e491f8cde407d01a1c0857b3a465f8b551",
            ),
            (
                "sql",
                "db4b71ab62c1c528f63b4b3706e7bab8a0e8583800d8a9cce27763f52ae618ec",
            ),
            (
                "haskell",
                "22f9325f9ca3200747b05cdee3656ffd65159fd06bfb55093956247451852f16",
            ),
            (
                "ocaml",
                "37cd484f614d3f8c1d1c3d06d8f52f9ecfe7d1d07b7728f7362536f3b07cf9bb",
            ),
            (
                "webassembly",
                "51cdefb6d3d187f6bd3a3c2ee343c18c462048e9e025d0d8052a9cad2bf4d4aa",
            ),
            (
                "python",
                "dd078f6b0eb48e099cce81b39711fa62313d39c7f8915abd97d9e72bc7678ecc",
            ),
            (
                "ubuntu_vm",
                "e863e96d5b3ce3e2b57a0ec8ee0ceb06038aa19b7929f5852ff2f92e117f44d3",
            ),
            (
                "bash",
                "e89ea6eb57eea53b50ba1c3b7a83a64c79d36160af7bffb0b15f8d03560c10cd",
            ),
            (
                "shell",
                "31652f34b475bdc7956505e39d135af74f80659f0795ca54a0e8f8a76c116cf9",
            ),
            (
                "rust",
                "8744827a7d497396ab645f4abd6f2f3a660da2fb769449d1cfa76ba98b68c2e5",
            ),
            (
                "racket",
                "c4e53bf39f937d0c25282b6f4e6a1ecf7073a68e110910fe0193cb446f2393e5",
            ),
            (
                "csharp",
                "4a34c14b79e3631831c220f61d8a30151b77fd4ccd475f35147aa94ba6d00e5e",
            ),
            (
                "c",
                "d8842139f0f671a062f414069d5653dc880e23e3439e73323868829fbaf7210d",
            ),
            (
                "cpp",
                "9c33364ebeb787d05f0ec4f01cf2f429cdcb6adbed2c01c67595557660dc6b73",
            ),
            (
                "lisp",
                "48df5c9240a148db43ad011dbf92a669138111540338273557e7d86faa8132fd",
            ),
            (
                "common_lisp",
                "7da0053792edf03b618f61408429bdc93fa2d50387f731c4d365d61d1f3b03c7",
            ),
            (
                "ruby",
                "14891c157518c0c8f34622adc00a8c755447ab7a894386341548dbec21fd6513",
            ),
            (
                "matlab",
                "d3917e3c2cd83292ef23795ba0098ccc62112c827a62499f404200c58c4bb8cf",
            ),
            (
                "mathematica",
                "c67b1715316b2b22349046e9541792d9d62c81bf48a9dd08b0d9fd2559557760",
            ),
            (
                "java",
                "b97392e28423d3aa4fd47919f18ca97d056b18344df9cba952f2c8a6c83b5962",
            ),
            (
                "javascript",
                "f98d171a8e35e67baa2d2a0b7d586140a1e37ea2d580844d596bf80ce8dd9bac",
            ),
        ];
        assert_eq!(registry.canonical_specs().len(), expected.len());
        for (name, digest) in expected {
            assert_eq!(
                registry.specification_sha256_v3(name).as_deref(),
                Some(digest)
            );
            assert!(!registry.contains_specification_sha256(digest));
        }
    }

    #[test]
    fn catalog_v4_declares_the_exact_state_support_partition() {
        use crate::placement::{BackendStateSupportV2, SnapshotCompatibilityV2};

        let registry = BackendRegistry::global();
        let mut stateless = Vec::new();
        let mut semantic = Vec::new();
        let mut external = Vec::new();
        for spec in registry.canonical_specs() {
            match &spec.state_support {
                BackendStateSupportV2::Stateless => stateless.push(spec.name),
                BackendStateSupportV2::SemanticSnapshot { .. } => semantic.push(spec.name),
                BackendStateSupportV2::ExternalPinned { .. } => external.push(spec.name),
            }
        }

        assert_eq!(stateless.len(), 27);
        assert_eq!(semantic, ["sql", "python"]);
        assert_eq!(external, ["ubuntu_vm"]);

        let expected_python_codec = crate::placement::SemanticDigestV1::hash_bytes(
            "ostadix/backend-state-codec-name/v2",
            b"ostadix.python-graph/v1",
        );
        assert_eq!(
            registry.state_support_for("py"),
            Some(&BackendStateSupportV2::SemanticSnapshot {
                codec: expected_python_codec,
                compatibility: SnapshotCompatibilityV2::ExactImplementation,
            })
        );
        let expected_sql_codec = crate::placement::SemanticDigestV1::hash_bytes(
            "ostadix/backend-state-codec-name/v2",
            crate::backend::state::SQL_CLI_CODEC_V1.as_bytes(),
        );
        assert_eq!(
            registry.state_support_for("sql"),
            Some(&BackendStateSupportV2::SemanticSnapshot {
                codec: expected_sql_codec,
                compatibility: SnapshotCompatibilityV2::ExactImplementation,
            })
        );
        let expected_ubuntu_manifest = crate::placement::SemanticDigestV1::hash_bytes(
            "ostadix/external-state-manifest-schema-name/v2",
            b"ostadix.multipass-resource/v1",
        );
        assert_eq!(
            registry.state_support_for("ubuntu"),
            Some(&BackendStateSupportV2::ExternalPinned {
                manifest_schema: expected_ubuntu_manifest,
            })
        );
        assert_eq!(registry.state_support_for("unknown"), None);
        assert_eq!(registry.interface_for("unknown").state_support, None);
    }

    #[test]
    fn catalog_v4_hashes_state_support_while_v3_stays_archival() {
        use crate::placement::BackendStateSupportV2;

        let registry = BackendRegistry::global();
        let python = registry.get("python").unwrap();
        let requirement = registry.runtime_requirements_for("python");
        let digest_for =
            |schema: &str,
             spec: &BackendSpec,
             hash_spec: fn(&mut Sha256, &BackendSpec, &RuntimeRequirementSpec)| {
                let mut hash = Sha256::new();
                catalog_hash_field(&mut hash, schema.as_bytes());
                hash_spec(&mut hash, spec, requirement);
                finish_catalog_hash(hash)
            };

        let mut weakened = python.clone();
        weakened.state_support = BackendStateSupportV2::Stateless;
        assert_eq!(
            digest_for(BACKEND_CATALOG_SCHEMA_V3, python, hash_backend_spec_v3),
            digest_for(BACKEND_CATALOG_SCHEMA_V3, &weakened, hash_backend_spec_v3),
            "archival V3 identity predates state support"
        );
        assert_ne!(
            digest_for(BACKEND_CATALOG_SCHEMA_V4, python, hash_backend_spec_v4),
            digest_for(BACKEND_CATALOG_SCHEMA_V4, &weakened, hash_backend_spec_v4),
            "current V4 identity must bind state support"
        );
    }

    #[test]
    fn exact_range_catalog_syntax_hashes_canonical_bigint_bounds() {
        let parsed = integer_exactness!(ExactRange {
            min: "-10",
            max: "20"
        });
        assert_eq!(
            parsed,
            IntegerExactness::ExactRange {
                min: BigInt::from(-10),
                max: BigInt::from(20),
            }
        );

        let registry = BackendRegistry::global();
        let digest_for = |integer_exactness: IntegerExactness| {
            let mut spec = registry.get("javascript").unwrap().clone();
            spec.value_capabilities.integer_exactness = integer_exactness;
            let mut hash = Sha256::new();
            catalog_hash_field(&mut hash, BACKEND_CATALOG_SCHEMA_V3.as_bytes());
            hash_backend_spec_v3(
                &mut hash,
                &spec,
                registry.runtime_requirements_for(spec.name),
            );
            finish_catalog_hash(hash)
        };

        let canonical = IntegerExactness::ExactRange {
            min: BigInt::from(-10),
            max: BigInt::from(20),
        };
        assert_eq!(digest_for(parsed), digest_for(canonical));
        assert_ne!(
            digest_for(IntegerExactness::ExactRange {
                min: BigInt::from(-10),
                max: BigInt::from(20),
            }),
            digest_for(IntegerExactness::ExactRange {
                min: BigInt::from(-10),
                max: BigInt::from(21),
            })
        );
        assert_ne!(
            digest_for(IntegerExactness::ExactMagnitudeBits(63)),
            digest_for(IntegerExactness::TwosComplementBits(63))
        );
    }

    #[test]
    #[should_panic(expected = "must use canonical signed base-10 spelling")]
    fn exact_range_catalog_syntax_rejects_noncanonical_bounds() {
        let _ = integer_exactness!(ExactRange {
            min: "-00010",
            max: "20"
        });
    }

    #[test]
    fn catalog_value_capabilities_follow_canonical_backend_identity() {
        let registry = BackendRegistry::global();
        assert_eq!(
            registry.value_capabilities_for("python"),
            registry.value_capabilities_for("py")
        );
        assert_eq!(
            registry.value_capabilities_for("python"),
            BackendValueCapabilities {
                integer_exactness: IntegerExactness::Arbitrary,
                rich_numbers: RichNumberPreservation::Preserved,
            }
        );
        assert_eq!(
            registry.value_capabilities_for("javascript"),
            BackendValueCapabilities {
                integer_exactness: IntegerExactness::ExactMagnitudeBits(53),
                rich_numbers: RichNumberPreservation::Collapsed,
            }
        );
        assert_eq!(
            registry.value_capabilities_for("java"),
            BackendValueCapabilities {
                integer_exactness: IntegerExactness::TwosComplementBits(63),
                rich_numbers: RichNumberPreservation::Collapsed,
            }
        );
        assert_eq!(
            registry.value_capabilities_for("unregistered"),
            BackendValueCapabilities::UNKNOWN
        );

        let python = registry.interface_for("py");
        assert_eq!(python.canonical, "python");
        assert_eq!(
            python.specification_sha256,
            registry.specification_sha256("python")
        );
        assert_eq!(
            python.value_capabilities,
            registry.value_capabilities_for("python")
        );
    }

    #[test]
    fn registry_renderers_match_legacy_dispatch() {
        let reg = BackendRegistry::global();
        assert_eq!(reg.renderer_for("python"), SpliceRenderer::Python);
        assert_eq!(reg.renderer_for("py"), SpliceRenderer::Python);
        assert_eq!(reg.renderer_for("html"), SpliceRenderer::Html);
        assert_eq!(reg.renderer_for("latex"), SpliceRenderer::Latex);
        assert_eq!(reg.renderer_for("tex"), SpliceRenderer::Latex);
        assert_eq!(reg.renderer_for("markdown"), SpliceRenderer::Markdown);
        assert_eq!(reg.renderer_for("md"), SpliceRenderer::Markdown);
        assert_eq!(reg.renderer_for("nix"), SpliceRenderer::Nix);
        assert_eq!(reg.renderer_for("nix_store"), SpliceRenderer::Nix);
        assert_eq!(reg.renderer_for("nixos_test"), SpliceRenderer::Nix);
        // nix_expr splices via the default representation (legacy behavior).
        assert_eq!(reg.renderer_for("nix_expr"), SpliceRenderer::Default);
        assert_eq!(reg.renderer_for("cobol"), SpliceRenderer::Default);
    }

    #[test]
    fn registry_exposes_adapter_required_authority_in_oir() {
        let reg = BackendRegistry::global();
        assert!(reg.interface_for("python").required_authorities.is_empty());
        assert_eq!(
            reg.interface_for("bash").required_authorities,
            vec![BackendAuthority::Process]
        );
        assert_eq!(
            reg.interface_for("nix").required_authorities,
            BackendAuthority::ALL
        );
        assert_eq!(
            reg.interface_for("unregistered_backend")
                .required_authorities,
            BackendAuthority::ALL,
            "unknown shims must default to the conservative authority envelope"
        );
    }

    #[test]
    fn shim_resolution_falls_back_to_convention() {
        let reg = BackendRegistry::global();
        let dir = Path::new("/nonexistent_shim_dir_for_test");
        assert_eq!(
            reg.resolve_shim_path(dir, "python"),
            dir.join("python_shim.py")
        );
    }

    #[test]
    fn plan_builds_data_and_sequence_edges() {
        let prog = OIrProgram::lower(&[
            ONode::LetBinding {
                name: "x".into(),
                expr: Box::new(ONode::Call {
                    fn_name: "instantiate".into(),
                    args: vec![ONode::VarRef("expr".into())],
                }),
            },
            ONode::TypedExpr {
                lang: "python".into(),
                env_id: 0,
                attr: None,
                body: vec![ONode::VarRef("x".into())],
            },
        ]);

        let plan = prog.plan();
        assert_eq!(plan.roots, vec![PlanNodeId(0), PlanNodeId(3)]);
        assert!(plan.edges.iter().any(|e| {
            e.from == PlanNodeId(0) && e.to == PlanNodeId(3) && e.kind == PlanEdgeKind::Sequence
        }));
        assert!(plan.edges.iter().any(|e| {
            e.from == PlanNodeId(0) && e.to == PlanNodeId(4) && e.kind == PlanEdgeKind::Data
        }));
    }

    #[test]
    fn quote_body_is_syntax_not_executable_plan_nodes() {
        let program = OIrProgram::lower(&[typed(
            "quote",
            vec![typed("python", vec![ONode::RawText("6 * 7".into())])],
        )]);

        let plan = program.plan();
        assert_eq!(program.flatten_for_plan().len(), 1);
        assert_eq!(plan.nodes.len(), 1);
        assert!(plan
            .edges
            .iter()
            .all(|edge| edge.kind != PlanEdgeKind::Structural));
    }

    #[test]
    fn backend_capability_attr_is_a_graph_visible_data_dependency() {
        let program = OIrProgram::lower(&[
            ONode::LetBinding {
                name: "runner".into(),
                expr: Box::new(ONode::RawText("capability placeholder".into())),
            },
            ONode::TypedExpr {
                lang: "python".into(),
                env_id: 0,
                attr: Some("cap=runner,process".into()),
                body: vec![ONode::RawText("__oval_result__ = 1".into())],
            },
        ]);

        let plan = program.plan();
        let runner_store = plan
            .nodes
            .iter()
            .find(|node| matches!(&node.kind, PlanNodeKind::Store { name } if name == "runner"))
            .unwrap()
            .id;
        let python_exec = plan
            .nodes
            .iter()
            .find(|node| {
                matches!(
                    &node.kind,
                    PlanNodeKind::Exec { lang, attr, .. }
                        if lang == "python" && attr.as_deref() == Some("cap=runner,process")
                )
            })
            .unwrap()
            .id;

        assert!(plan.edges.iter().any(|edge| {
            edge.from == runner_store && edge.to == python_exec && edge.kind == PlanEdgeKind::Data
        }));
    }

    #[test]
    fn scope_capture_depends_on_every_visible_store() {
        let program = OIrProgram::lower(&[
            ONode::LetBinding {
                name: "x".into(),
                expr: Box::new(ONode::RawText("one".into())),
            },
            ONode::LetBinding {
                name: "y".into(),
                expr: Box::new(ONode::RawText("two".into())),
            },
            ONode::LetBinding {
                name: "captured".into(),
                expr: Box::new(ONode::Call {
                    fn_name: "scope".into(),
                    args: vec![],
                }),
            },
        ]);
        let plan = program.plan();
        let capture = plan
            .nodes
            .iter()
            .find(|node| {
                matches!(
                    &node.kind,
                    PlanNodeKind::Call { fn_name, .. } if fn_name == "scope"
                )
            })
            .unwrap()
            .id;
        let visible_stores = plan
            .edges
            .iter()
            .filter(|edge| edge.to == capture && edge.kind == PlanEdgeKind::Data)
            .map(|edge| edge.from)
            .collect::<BTreeSet<_>>();

        assert_eq!(
            visible_stores,
            BTreeSet::from([PlanNodeId(0), PlanNodeId(2)])
        );
    }

    #[test]
    fn executable_plan_validates_and_schedules_roots() {
        let program = OIrProgram::lower(&[
            ONode::LetBinding {
                name: "x".into(),
                expr: Box::new(ONode::RawText("value".into())),
            },
            ONode::VarRef("x".into()),
            typed("html", vec![ONode::VarRef("x".into())]),
        ]);
        let plan = program.plan();
        plan.validate(program.nodes.len()).unwrap();
        assert_eq!(plan.root_schedule().unwrap(), vec![0, 1, 2]);
        assert_eq!(
            plan.child_schedule(plan.roots[0]).unwrap(),
            vec![PlanNodeId(1)]
        );
        assert_eq!(
            plan.child_schedule(plan.roots[2]).unwrap(),
            vec![PlanNodeId(4)]
        );
        assert_eq!(plan.topological_order().unwrap().len(), plan.nodes.len());
    }

    #[test]
    fn plan_promotes_request_group_and_schedule_nodes() {
        let program = OIrProgram::lower(&[
            ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::RawText("drv".into())],
            },
            ONode::Call {
                fn_name: "dry_activate".into(),
                args: vec![ONode::RawText("/nix/store/demo-system".into())],
            },
            ONode::Call {
                fn_name: "activate".into(),
                args: vec![ONode::RawText("/nix/store/demo-system".into())],
            },
            ONode::Call {
                fn_name: "batch".into(),
                args: vec![ONode::RawText("a".into()), ONode::RawText("b".into())],
            },
            ONode::Call {
                fn_name: "autonomous".into(),
                args: vec![ONode::RawText("body".into())],
            },
            ONode::Call {
                fn_name: "now".into(),
                args: vec![ONode::RawText("req".into())],
            },
            ONode::TypedExpr {
                lang: "html".into(),
                env_id: 0,
                attr: Some("lazy".into()),
                body: vec![ONode::RawText("<p>x</p>".into())],
            },
        ]);
        let plan = program.plan();

        assert!(plan.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Request {
                    kind: PlanRequestKind::Instantiate,
                    ..
                }
            ) && node.kind.class() == PlanNodeClass::Control
                && node.kind.eval_cache_policy() == Some(CachePolicy::Memoize)
        }));
        assert!(plan.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Request {
                    kind: PlanRequestKind::DryActivate,
                    ..
                }
            ) && node.kind.class() == PlanNodeClass::Control
                && node.kind.eval_cache_policy() == Some(CachePolicy::Bypass)
        }));
        assert!(plan.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Request {
                    kind: PlanRequestKind::Activate,
                    ..
                }
            ) && node.kind.class() == PlanNodeClass::Control
                && node.kind.eval_cache_policy() == Some(CachePolicy::Bypass)
        }));
        assert!(plan.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Group {
                    mode: GroupMode::Batch,
                    member_count: 2,
                }
            ) && node.kind.class() == PlanNodeClass::Control
        }));
        assert!(plan.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Schedule {
                    kind: PlanScheduleKind::Autonomous,
                    ..
                }
            ) && node.kind.class() == PlanNodeClass::Control
        }));
        assert!(plan.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Schedule {
                    kind: PlanScheduleKind::Force,
                    ..
                }
            ) && node.kind.class() == PlanNodeClass::Control
        }));
        assert!(plan.nodes.iter().any(|node| {
            matches!(
                &node.kind,
                PlanNodeKind::Exec {
                    attr: Some(attr),
                    ..
                } if attr == "lazy"
            ) && node.kind.class() == PlanNodeClass::Control
                && node.kind.eval_cache_policy() == Some(CachePolicy::Memoize)
        }));
    }

    #[test]
    fn executable_plan_rejects_dependency_cycles() {
        let mut plan = OIrProgram::lower(&[ONode::RawText("a".into())]).plan();
        plan.edges.push(PlanEdge {
            from: PlanNodeId(0),
            to: PlanNodeId(0),
            kind: PlanEdgeKind::Sequence,
        });
        assert!(plan.validate(1).unwrap_err().contains("cycle"));
    }

    #[test]
    fn executable_oir_reconstructs_quoted_source() {
        let nodes = vec![typed(
            "html",
            vec![
                ONode::RawText("<p>".into()),
                ONode::VarRef("answer".into()),
                ONode::RawText("</p>".into()),
            ],
        )];
        let program = OIrProgram::lower(&nodes);
        assert_eq!(
            reconstruct_source(&program.nodes),
            "html[0]^(<p>$answer</p>)_html[0]"
        );
    }

    #[test]
    fn executable_oir_preserves_linker_isolated_source_marker() {
        let mut node = typed("python", vec![ONode::RawText("1".into())]);
        let ONode::TypedExpr { env_id, .. } = &mut node else {
            unreachable!("typed helper always constructs a typed expression")
        };
        *env_id = crate::environment::LINKER_ISOLATED_ENV_ID;
        let program = OIrProgram::lower(&[node]);

        assert_eq!(
            reconstruct_source(&program.nodes),
            "python[*]^(1)_python[*]"
        );
        let dump = program.to_text();
        assert!(dump.contains("exec python [env *]"), "{dump}");
    }

    #[test]
    fn registry_exposes_typed_backend_interface() {
        let reg = BackendRegistry::global();
        let python = reg.interface_for("py");
        let html = reg.interface_for("html");
        let quote = reg.interface_for("quote");

        assert_eq!(python.canonical, "python");
        assert_eq!(python.execution, ExecutionMode::Shim);
        assert_eq!(html.execution, ExecutionMode::InlineValue);
        assert_eq!(quote.execution, ExecutionMode::InlineAst);
    }
}
