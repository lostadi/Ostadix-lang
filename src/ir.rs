// ─────────────────────────────────────────────────────────────────────────────
// ir.rs — the O-lang intermediate representation and backend interface layer.
//
// This module is a thin, stable seam between four concerns that were
// previously fused inside parser.rs / eval.rs / olangc.rs:
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
//   - no behavior changes — lowering is a 1:1 structural mapping of ONode,
//     and the registry reproduces the exact metadata the evaluator used
//     to hard-code.
//
// The evaluator continues to walk ONode directly; OIr is currently a
// debug/inspection surface (`olangc --target ir`) and the designated place
// where future execution planning will live.
// ─────────────────────────────────────────────────────────────────────────────

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::parser::ONode;

// ═════════════════════════════════════════════════════════════════════════════
// OIr — the lowered instruction forms
// ═════════════════════════════════════════════════════════════════════════════

/// One lowered IR node. Structurally mirrors `ONode` 1:1 today — the value of
/// the layer is the *seam*, not a transformation. Future passes (constant
/// splice folding, purity-driven reordering, request batching) operate here
/// without touching parser or evaluator.
#[derive(Debug, Clone, PartialEq)]
pub enum OIr {
    /// Verbatim text destined for a backend splice buffer.
    Text(String),

    /// Read a variable from scope (`$name`).
    Load(String),

    /// Bind the result of `expr` to `name` in scope (`let name = expr`).
    Store { name: String, expr: Box<OIr> },

    /// Invoke a built-in O-level function (`instantiate(...)`, `now(...)`, …).
    Invoke { fn_name: String, args: Vec<OIr> },

    /// Execute a typed-expression block on backend `lang`.
    Exec {
        lang:   String,
        env_id: u32,
        attr:   Option<String>,
        body:   Vec<OIr>,
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
        Self { nodes: nodes.iter().map(lower_node).collect() }
    }

    /// Human-readable dump used by `olangc --target ir`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("; OIrProgram\n");
        for node in &self.nodes {
            dump_node(node, 0, &mut out);
        }
        out
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
            args:    args.iter().map(lower_node).collect(),
        },
        ONode::TypedExpr { lang, env_id, attr, body } => OIr::Exec {
            lang:   lang.clone(),
            env_id: *env_id,
            attr:   attr.clone(),
            body:   body.iter().map(lower_node).collect(),
        },
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
        OIr::Invoke { fn_name, args } => {
            out.push_str(&format!("{indent}invoke {fn_name}/{}\n", args.len()));
            for arg in args {
                dump_node(arg, depth + 1, out);
            }
        }
        OIr::Exec { lang, env_id, attr, body } => {
            let attr_s = attr.as_deref()
                .map(|a| format!(" {{{a}}}"))
                .unwrap_or_default();
            let env_s = if *env_id == 0 {
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
}

impl BackendSpec {
    const fn new(
        name: &'static str,
        aliases: &'static [&'static str],
        pure: bool,
        renderer: SpliceRenderer,
    ) -> Self {
        Self { name, aliases, pure, renderer }
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
    BackendSpec::new("O",     &[], false, SpliceRenderer::Default),
    BackendSpec::new("quote", &[], false, SpliceRenderer::Default),

    // Nix family — deterministic by design.
    BackendSpec::new("nix",        &[], true, SpliceRenderer::Nix),
    // nix_expr is already lazy by construction; {lazy}/{defer} are rejected
    // anyway. It splices via the default representation (its body is
    // assembled before any Nix evaluation happens).
    BackendSpec::new("nix_expr",   &[], true, SpliceRenderer::Default),
    BackendSpec::new("nix_store",  &[], true, SpliceRenderer::Nix),
    BackendSpec::new("nixos_test", &[], true, SpliceRenderer::Nix),

    // Pure templating.
    BackendSpec::new("html",     &[],     true, SpliceRenderer::Html),
    BackendSpec::new("markdown", &["md"], true, SpliceRenderer::Markdown),
    BackendSpec::new("latex",    &["tex"], true, SpliceRenderer::Latex),
    BackendSpec::new("text",     &[],     true, SpliceRenderer::Default),

    // Declarative / pure-by-default languages.
    BackendSpec::new("sql",         &[], true, SpliceRenderer::Default),
    BackendSpec::new("haskell",     &[], true, SpliceRenderer::Default),
    BackendSpec::new("ocaml",       &[], true, SpliceRenderer::Default),
    BackendSpec::new("webassembly", &[], true, SpliceRenderer::Default),

    // General-purpose, impure backends.
    BackendSpec::new("python",      &["py"], false, SpliceRenderer::Python),
    BackendSpec::new("bash",        &[], false, SpliceRenderer::Default),
    BackendSpec::new("shell",       &[], false, SpliceRenderer::Default),
    BackendSpec::new("rust",        &[], false, SpliceRenderer::Default),
    BackendSpec::new("racket",      &[], false, SpliceRenderer::Default),
    BackendSpec::new("csharp",      &[], false, SpliceRenderer::Default),
    BackendSpec::new("cpp",         &[], false, SpliceRenderer::Default),
    BackendSpec::new("lisp",        &[], false, SpliceRenderer::Default),
    BackendSpec::new("common_lisp", &[], false, SpliceRenderer::Default),
    BackendSpec::new("ruby",        &[], false, SpliceRenderer::Default),
    BackendSpec::new("matlab",      &[], false, SpliceRenderer::Default),
    BackendSpec::new("mathematica", &[], false, SpliceRenderer::Default),
    BackendSpec::new("java",        &[], false, SpliceRenderer::Default),
    BackendSpec::new("javascript",  &[], false, SpliceRenderer::Default),
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
    const DEFAULT_SPEC: BackendSpec =
        BackendSpec::new("", &[], false, SpliceRenderer::Default);

    /// The process-wide registry over the static spec table.
    pub fn global() -> &'static BackendRegistry {
        static REGISTRY: OnceLock<BackendRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| BackendRegistry { specs: BACKEND_SPECS })
    }

    /// Look up a backend by canonical name or alias.
    pub fn get(&self, lang: &str) -> Option<&BackendSpec> {
        self.specs.iter().find(|s| s.matches(lang))
    }

    /// Whether `{lazy}` may cache results from this backend.
    /// Unknown backends are conservatively impure.
    pub fn is_pure(&self, lang: &str) -> bool {
        self.get(lang).map_or(false, |s| s.pure)
    }

    /// Which splice-rendering strategy `render_child` should use for `lang`.
    /// Unknown backends use the conservative default representation.
    pub fn renderer_for(&self, lang: &str) -> SpliceRenderer {
        self.get(lang).map_or(Self::DEFAULT_SPEC.renderer, |s| s.renderer)
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
            lang:   lang.to_string(),
            env_id: 0,
            attr:   None,
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
        let nodes = vec![typed("html", vec![
            ONode::RawText("<p>".into()),
            typed("python", vec![ONode::RawText("2 + 2".into())]),
            ONode::VarRef("x".into()),
            ONode::RawText("</p>".into()),
        ])];
        let prog = OIrProgram::lower(&nodes);
        assert_eq!(prog.nodes, vec![OIr::Exec {
            lang:   "html".into(),
            env_id: 0,
            attr:   None,
            body:   vec![
                OIr::Text("<p>".into()),
                OIr::Exec {
                    lang:   "python".into(),
                    env_id: 0,
                    attr:   None,
                    body:   vec![OIr::Text("2 + 2".into())],
                },
                OIr::Load("x".into()),
                OIr::Text("</p>".into()),
            ],
        }]);
    }

    #[test]
    fn lower_let_and_call() {
        let nodes = vec![ONode::LetBinding {
            name: "drv".into(),
            expr: Box::new(ONode::Call {
                fn_name: "instantiate".into(),
                args:    vec![ONode::VarRef("expr".into())],
            }),
        }];
        let prog = OIrProgram::lower(&nodes);
        assert_eq!(prog.nodes, vec![OIr::Store {
            name: "drv".into(),
            expr: Box::new(OIr::Invoke {
                fn_name: "instantiate".into(),
                args:    vec![OIr::Load("expr".into())],
            }),
        }]);
    }

    #[test]
    fn ir_dump_is_stable() {
        let nodes = vec![typed("python", vec![ONode::RawText("1 + 1".into())])];
        let prog = OIrProgram::lower(&nodes);
        assert_eq!(prog.to_text(), "; OIrProgram\nexec python\n  text \"1 + 1\"\n");
    }

    #[test]
    fn registry_purity_matches_legacy_table() {
        let reg = BackendRegistry::global();
        for lang in [
            "nix", "nix_expr", "nix_store", "nixos_test", "html", "markdown",
            "latex", "text", "sql", "haskell", "ocaml", "webassembly",
        ] {
            assert!(reg.is_pure(lang), "{lang} should be pure");
        }
        for lang in [
            "python", "shell", "bash", "rust", "racket", "java",
            "javascript", "ruby", "O", "quote", "cobol",
        ] {
            assert!(!reg.is_pure(lang), "{lang} should be impure");
        }
    }

    #[test]
    fn registry_renderers_match_legacy_dispatch() {
        let reg = BackendRegistry::global();
        assert_eq!(reg.renderer_for("python"), SpliceRenderer::Python);
        assert_eq!(reg.renderer_for("py"),     SpliceRenderer::Python);
        assert_eq!(reg.renderer_for("html"),   SpliceRenderer::Html);
        assert_eq!(reg.renderer_for("latex"),  SpliceRenderer::Latex);
        assert_eq!(reg.renderer_for("tex"),    SpliceRenderer::Latex);
        assert_eq!(reg.renderer_for("markdown"), SpliceRenderer::Markdown);
        assert_eq!(reg.renderer_for("md"),       SpliceRenderer::Markdown);
        assert_eq!(reg.renderer_for("nix"),        SpliceRenderer::Nix);
        assert_eq!(reg.renderer_for("nix_store"),  SpliceRenderer::Nix);
        assert_eq!(reg.renderer_for("nixos_test"), SpliceRenderer::Nix);
        // nix_expr splices via the default representation (legacy behavior).
        assert_eq!(reg.renderer_for("nix_expr"), SpliceRenderer::Default);
        assert_eq!(reg.renderer_for("cobol"),    SpliceRenderer::Default);
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
}
