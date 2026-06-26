// ─────────────────────────────────────────────────────────────────────────────
// ir.rs — the executable O-lang intermediate representation.
//
// This module is the stable seam between four concerns that were previously
// fused inside parser.rs / eval.rs / olangc.rs:
//
//   1. Syntax            — ONode, produced by the parser.
//   2. Execution plan    — OIr / OIrProgram, a lowered, backend-neutral form
//                          of the program (this module).
//   3. Runtime values    — OValue, produced by the evaluator.
//   4. Backend metadata  — BackendSpec / BackendRegistry: purity, splice
//                          rendering strategy, and shim path resolution.
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

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::parser::ONode;
use crate::value::{BackendAuthority, GroupMode};

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
    fn for_name(name: &str) -> Self {
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

    fn label(self) -> &'static str {
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

    /// Build the value-node/operation-edge hypergraph for this program.
    ///
    /// The current evaluator still interprets OIR, but this keeps the new
    /// hypergraph substrate in lockstep with the same lowered program that
    /// runtime execution uses.
    pub fn hgraph(&self) -> crate::hgraph::HGraph {
        crate::hgraph::from_oir::build_program(self)
    }
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
            if *env_id != u32::MAX {
                out.push('[');
                out.push_str(&env_id.to_string());
                out.push(']');
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
            if *env_id != u32::MAX {
                out.push('[');
                out.push_str(&env_id.to_string());
                out.push(']');
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
            let env_s = if *env_id == u32::MAX {
                String::new()
            } else {
                format!(" [env {env_id}]")
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEdge {
    pub from: PlanNodeId,
    pub to: PlanNodeId,
    pub kind: PlanEdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    InlineAst,
    InlineValue,
    Shim,
}

impl ExecutionMode {
    fn label(self) -> &'static str {
        match self {
            ExecutionMode::InlineAst => "inline_ast",
            ExecutionMode::InlineValue => "inline_value",
            ExecutionMode::Shim => "shim",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInterface {
    pub canonical: String,
    pub pure: bool,
    pub renderer: SpliceRenderer,
    pub execution: ExecutionMode,
    /// Authority required by the backend adapter itself, before any
    /// additional rights declared by a source block.
    pub required_authorities: Vec<BackendAuthority>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanNodeKind {
    Text,
    Load {
        name: String,
    },
    Store {
        name: String,
    },
    Invoke {
        fn_name: String,
        mode: InvokeMode,
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
    fn describe(&self) -> String {
        match self {
            PlanNodeKind::Text => "text".to_string(),
            PlanNodeKind::Load { name } => format!("load ${name}"),
            PlanNodeKind::Store { name } => format!("store ${name}"),
            PlanNodeKind::Invoke {
                fn_name,
                mode,
                arg_count,
            } => {
                format!("invoke {fn_name}/{arg_count} [{}]", mode.label())
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
                let env = if *env_id == u32::MAX {
                    "ephemeral".to_string()
                } else {
                    env_id.to_string()
                };
                let required = backend
                    .required_authorities
                    .iter()
                    .map(|authority| authority.name())
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "exec {} [env {}]{} backend={} pure={} renderer={:?} execution={} required=[{}]",
                    lang,
                    env,
                    attr_s,
                    backend.canonical,
                    backend.pure,
                    backend.renderer,
                    backend.execution.label(),
                    required
                )
            }
        }
    }
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
        self.edges.push(PlanEdge { from, to, kind });
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
            OIr::Exec { body, .. } => {
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
            } => PlanNodeKind::Invoke {
                fn_name: fn_name.clone(),
                mode: *mode,
                arg_count: args.len(),
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

// ═════════════════════════════════════════════════════════════════════════════
// Backend interface — BackendSpec / BackendRegistry
// ═════════════════════════════════════════════════════════════════════════════

/// How an OValue is rendered into a backend's splice buffer. The actual
/// renderer functions live in eval.rs (they need OValue); the registry only
/// records which strategy a backend uses, so the dispatch decision is
/// centralized here while the value-level code stays with the evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpliceRenderer {
    /// Python literals (`None`, `True`, `[1, 2]`, …).
    Python,
    /// Embeddable HTML markup (blobs become data-URI `<img>` tags).
    Html,
    /// LaTeX-safe text.
    Latex,
    /// Markdown-safe text.
    Markdown,
    /// Syntactically valid Nix expressions.
    Nix,
    /// `OValue::splice_repr()` — the conservative cross-language form.
    Default,
}

/// Static metadata for one backend: the single source of truth for purity,
/// splice rendering strategy, and shim filename hints.
#[derive(Debug, Clone, Copy)]
pub struct BackendSpec {
    /// Canonical backend name as it appears in a language tag.
    pub name: &'static str,
    /// Alternate tag spellings accepted by splice rendering (`py`, `md`, …).
    pub aliases: &'static [&'static str],
    /// Whether `{lazy}` may cache results from this backend.
    ///
    /// "Pure" means: same body + same deps + same env ⇒ same output. No
    /// hidden IO, no clock, no random, no mutable global state. The flag is
    /// conservative — a backend is marked pure only when we're confident.
    /// `{defer}` works on any backend (it never caches), so it's the
    /// impure-backend escape hatch.
    pub pure: bool,
    /// Which splice-rendering strategy `render_child` should use.
    pub renderer: SpliceRenderer,
    /// How the evaluator dispatches this backend.
    pub execution: ExecutionMode,
    /// Rights needed to implement this backend. For example,
    /// the Bash adapter must start `bash`, while Python evaluation itself does
    /// not require a child process.
    pub required_authorities: &'static [BackendAuthority],
}

impl BackendSpec {
    const fn new(
        name: &'static str,
        aliases: &'static [&'static str],
        pure: bool,
        renderer: SpliceRenderer,
        execution: ExecutionMode,
    ) -> Self {
        Self {
            name,
            aliases,
            pure,
            renderer,
            execution,
            required_authorities: &[],
        }
    }

    const fn with_authority(
        name: &'static str,
        aliases: &'static [&'static str],
        pure: bool,
        renderer: SpliceRenderer,
        execution: ExecutionMode,
        required_authorities: &'static [BackendAuthority],
    ) -> Self {
        Self {
            name,
            aliases,
            pure,
            renderer,
            execution,
            required_authorities,
        }
    }

    fn matches(&self, lang: &str) -> bool {
        self.name == lang || self.aliases.contains(&lang)
    }
}

/// Centralized backend metadata table. Purity values reproduce the former
/// PURE_BACKENDS list in eval.rs; renderer values reproduce the former
/// render_child match arms. Backends not listed here fall back to
/// `BackendRegistry::DEFAULT_SPEC` (impure, default renderer).
const BACKEND_SPECS: &[BackendSpec] = &[
    // Sequencing / host languages.
    BackendSpec::new(
        "O",
        &["o"],
        false,
        SpliceRenderer::Default,
        ExecutionMode::InlineAst,
    ),
    BackendSpec::new(
        "quote",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::InlineAst,
    ),
    // Nix family — deterministic by design.
    BackendSpec::with_authority(
        "nix",
        &[],
        true,
        SpliceRenderer::Nix,
        ExecutionMode::Shim,
        &[
            BackendAuthority::FileRead,
            BackendAuthority::FileWrite,
            BackendAuthority::Network,
            BackendAuthority::Process,
        ],
    ),
    // nix_expr is already lazy by construction; {lazy}/{defer} are rejected
    // anyway. It splices via the default representation (its body is
    // assembled before any Nix evaluation happens).
    BackendSpec::new(
        "nix_expr",
        &[],
        true,
        SpliceRenderer::Default,
        ExecutionMode::InlineValue,
    ),
    BackendSpec::with_authority(
        "nix_store",
        &[],
        true,
        SpliceRenderer::Nix,
        ExecutionMode::Shim,
        &[
            BackendAuthority::FileRead,
            BackendAuthority::FileWrite,
            BackendAuthority::Network,
            BackendAuthority::Process,
        ],
    ),
    BackendSpec::with_authority(
        "nixos_test",
        &[],
        true,
        SpliceRenderer::Nix,
        ExecutionMode::Shim,
        &[
            BackendAuthority::FileRead,
            BackendAuthority::FileWrite,
            BackendAuthority::Network,
            BackendAuthority::Process,
        ],
    ),
    // Pure templating.
    BackendSpec::new(
        "html",
        &[],
        true,
        SpliceRenderer::Html,
        ExecutionMode::InlineValue,
    ),
    BackendSpec::new(
        "markdown",
        &["md"],
        true,
        SpliceRenderer::Markdown,
        ExecutionMode::InlineValue,
    ),
    BackendSpec::new(
        "latex",
        &["tex"],
        true,
        SpliceRenderer::Latex,
        ExecutionMode::InlineValue,
    ),
    BackendSpec::new(
        "text",
        &["plain"],
        true,
        SpliceRenderer::Default,
        ExecutionMode::InlineValue,
    ),
    // Declarative / pure-by-default languages.
    BackendSpec::new(
        "sql",
        &[],
        true,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
    ),
    BackendSpec::with_authority(
        "haskell",
        &[],
        true,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "ocaml",
        &[],
        true,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "webassembly",
        &[],
        true,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    // General-purpose, impure backends.
    BackendSpec::new(
        "python",
        &["py"],
        false,
        SpliceRenderer::Python,
        ExecutionMode::Shim,
    ),
    BackendSpec::with_authority(
        "bash",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "shell",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "rust",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "racket",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "csharp",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "cpp",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "lisp",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "common_lisp",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "ruby",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "matlab",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "mathematica",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "java",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
    BackendSpec::with_authority(
        "javascript",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[BackendAuthority::FileWrite, BackendAuthority::Process],
    ),
];

/// Lookup table over `BackendSpec`s plus the centralized shim path
/// resolution rule. Today the table is static; `BackendRegistry` is the
/// place where dynamically registered backends would plug in later.
#[derive(Debug)]
pub struct BackendRegistry {
    specs: &'static [BackendSpec],
}

impl BackendRegistry {
    /// Fallback metadata for backends with no entry in the table:
    /// impure, conservative cross-language splice representation.
    const DEFAULT_SPEC: BackendSpec = BackendSpec::with_authority(
        "",
        &[],
        false,
        SpliceRenderer::Default,
        ExecutionMode::Shim,
        &[
            BackendAuthority::FileRead,
            BackendAuthority::FileWrite,
            BackendAuthority::Network,
            BackendAuthority::Process,
        ],
    );

    /// The process-wide registry over the static spec table.
    pub fn global() -> &'static BackendRegistry {
        static REGISTRY: OnceLock<BackendRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| BackendRegistry {
            specs: BACKEND_SPECS,
        })
    }

    /// Look up a backend by canonical name or alias.
    pub fn get(&self, lang: &str) -> Option<&BackendSpec> {
        self.specs.iter().find(|s| s.matches(lang))
    }

    /// Resolve a language tag (canonical name or alias) to its canonical
    /// name. Unknown tags are returned unchanged.
    pub fn canonical<'a>(&self, lang: &'a str) -> &'a str {
        self.get(lang).map_or(lang, |s| s.name)
    }

    /// Whether `{lazy}` may cache results from this backend.
    /// Unknown backends are conservatively impure.
    pub fn is_pure(&self, lang: &str) -> bool {
        self.get(lang).is_some_and(|s| s.pure)
    }

    /// Which splice-rendering strategy `render_child` should use for `lang`.
    /// Unknown backends use the conservative default representation.
    pub fn renderer_for(&self, lang: &str) -> SpliceRenderer {
        self.get(lang)
            .map_or(Self::DEFAULT_SPEC.renderer, |s| s.renderer)
    }

    /// Typed backend interface metadata used by planning and dispatch policy.
    pub fn interface_for(&self, lang: &str) -> BackendInterface {
        let canonical = self.canonical(lang).to_string();
        let spec = self.get(lang).copied().unwrap_or(Self::DEFAULT_SPEC);
        BackendInterface {
            canonical,
            pure: spec.pure,
            renderer: spec.renderer,
            execution: spec.execution,
            required_authorities: spec.required_authorities.to_vec(),
        }
    }

    /// Centralized shim path resolution.
    ///
    /// Probes, in order: `<dir>/<lang>_shim.py`, `<dir>/<lang>_shim`,
    /// `<dir>/<lang>.py`, `<dir>/<lang>`. If none exists on disk, falls back
    /// to `<dir>/<lang>_shim.py` so the eventual spawn error names the
    /// conventional path.
    pub fn resolve_shim_path(&self, shim_dir: &Path, lang: &str) -> PathBuf {
        let candidates = [
            shim_dir.join(format!("{lang}_shim.py")),
            shim_dir.join(format!("{lang}_shim")),
            shim_dir.join(format!("{lang}.py")),
            shim_dir.join(lang),
        ];
        candidates
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| shim_dir.join(format!("{lang}_shim.py")))
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ir_dump_is_stable() {
        let nodes = vec![typed("python", vec![ONode::RawText("1 + 1".into())])];
        let prog = OIrProgram::lower(&nodes);
        assert_eq!(
            prog.to_text(),
            concat!(
                "; OIrProgram\n",
                "exec python [env 0]\n",
                "  text \"1 + 1\"\n",
                "\n",
                "; ExecutionPlan\n",
                "roots [0]\n",
                "node 0 exec python [env 0] backend=python pure=false renderer=Python execution=shim required=[]\n",
                "node 1 text\n",
                "edge 1 -> 0 structural\n",
            )
        );
    }

    #[test]
    fn registry_purity_matches_legacy_table() {
        let reg = BackendRegistry::global();
        for lang in [
            "nix",
            "nix_expr",
            "nix_store",
            "nixos_test",
            "html",
            "markdown",
            "latex",
            "text",
            "sql",
            "haskell",
            "ocaml",
            "webassembly",
        ] {
            assert!(reg.is_pure(lang), "{lang} should be pure");
        }
        for lang in [
            "python",
            "shell",
            "bash",
            "rust",
            "racket",
            "java",
            "javascript",
            "ruby",
            "O",
            "quote",
            "cobol",
        ] {
            assert!(!reg.is_pure(lang), "{lang} should be impure");
        }
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
                    PlanNodeKind::Invoke { fn_name, .. } if fn_name == "scope"
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
