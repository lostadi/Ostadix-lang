// ─────────────────────────────────────────────────────────────────────────────
// eval.rs
//
// The O-language evaluator — applicative order, leaves-up.
//
// Evaluation semantics (mirrors o_lang/evaluator.py):
//
//   TypedExpr { lang, env_id, body }:
//     1. Walk body children left-to-right, building a splice buffer:
//          RawText  → append verbatim
//          VarRef   → look up scope, render via render_child, append
//          TypedExpr → evaluate recursively first, render via render_child, append
//     2. Call ProcessRegistry::exec(lang, env_id, buffer, scope, shim)
//     3. For ephemeral envs (env_id == u32::MAX): call cleanup_env (always, even on err)
//
//   Root document (eval_document):
//     Evaluate nodes sequentially; return the last non-null OValue,
//     or ONull if no non-null value was produced.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

use crate::parser::ONode;
use crate::process::ProcessRegistry;
use crate::value::OValue;

// ═════════════════════════════════════════════════════════════════════════════
// Evaluator
// ═════════════════════════════════════════════════════════════════════════════

pub struct Evaluator {
    registry: ProcessRegistry,
    /// Directory containing one backend shim executable per language.
    /// Shim path for a language `lang` is `shim_dir/lang`.
    shim_dir: PathBuf,
}

impl Evaluator {
    pub fn new(shim_dir: PathBuf) -> Self {
        Evaluator {
            registry: ProcessRegistry::new(),
            shim_dir,
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Public API
    // ─────────────────────────────────────────────────────────────────────────

    /// Evaluate a parsed O document.
    ///
    /// Nodes are evaluated sequentially with an empty root scope. The return
    /// value is the last non-null `OValue` produced, or `OValue::Null` if
    /// every node evaluated to null or the document was empty.
    pub fn eval_document(&mut self, nodes: Vec<ONode>) -> Result<OValue> {
        let mut scope = HashMap::new();
        let mut last = OValue::Null;

        for node in nodes {
            match &node {
                ONode::LetBinding { name, expr } => {
                    let value = self.eval_node(expr, &scope)?;
                    scope.insert(name.clone(), value.clone());

                    if !matches!(value, OValue::Null) {
                        last = value;
                    }
                }

                _ => {
                    let value = self.eval_node(&node, &scope)?;

                    if !matches!(value, OValue::Null) {
                        last = value;
                    }
                }
            }
        }

        Ok(last)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Node dispatch
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_node(&mut self, node: &ONode, scope: &HashMap<String, OValue>) -> Result<OValue> {
        match node {
            ONode::LetBinding { expr, .. } => {
                self.eval_node(expr, scope)
            },
            ONode::RawText(text) => Ok(OValue::str_(text.clone())),

            ONode::VarRef(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Undefined variable: ${}", name)),

            ONode::TypedExpr { lang, env_id, body } => {
                self.eval_typed_expr(lang, *env_id, body, scope)
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core evaluation: build splice buffer then dispatch to backend
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_typed_expr(
        &mut self,
        lang:   &str,
        env_id: u32,
        body:   &[ONode],
        scope:  &HashMap<String, OValue>,
    ) -> Result<OValue> {
        // Step 1 — build the fully-spliced source string for the backend.
        let mut buf = String::new();

        for child in body {
            match child {
                ONode::LetBinding { .. } => {
                    bail!("let bindings are only supported at document top level for now");
                },
                ONode::RawText(text) => {
                    buf.push_str(text);
                }

                ONode::VarRef(name) => {
                    let val = scope
                        .get(name)
                        .ok_or_else(|| anyhow::anyhow!("Undefined variable: ${}", name))?;
                    buf.push_str(&self.render_child(lang, val));
                }

                ONode::TypedExpr {
                    lang: child_lang,
                    env_id: child_env_id,
                    body: child_body,
                } => {
                    // Evaluate the nested expression first (leaves-up / applicative order),
                    // then render its value into the parent language's source syntax.
                    let child_val =
                        self.eval_typed_expr(child_lang, *child_env_id, child_body, scope)?;
                    buf.push_str(&self.render_child(lang, &child_val));
                }
            }
        }

        // Step 2 — send the completed splice buffer to the backend.
        let shim = {
            let candidates = [
                self.shim_dir.join(format!("{lang}_shim.py")),
                self.shim_dir.join(format!("{lang}_shim")),
                self.shim_dir.join(format!("{lang}.py")),
                self.shim_dir.join(lang),
            ];

            candidates
                .into_iter()
                .find(|p| p.exists())
                .unwrap_or_else(|| self.shim_dir.join(format!("{lang}_shim.py")))
        };
        if lang == "html" {
            return Ok(OValue::html(buf));
        }

        let result = self.registry.exec(lang, env_id, &buf, scope.clone(), &shim);

        // Step 3 — discard ephemeral envs (env_id == u32::MAX) after every expression,
        // regardless of whether exec succeeded.  This mirrors the Python
        // evaluator's "unbracketed → env is garbage collected after eval".
        if env_id == u32::MAX {
            let _ = self.registry.cleanup_env(lang, u32::MAX);
        }

        result.map_err(|e| {
            let env_label = if env_id == u32::MAX {
                format!("{lang}[*ephemeral*]")
            } else {
                format!("{lang}[{env_id}]")
            };

            anyhow::anyhow!("[{}] {}", env_label, e)
        })
    }

    // ─────────────────────────────────────────────────────────────────────────
    // render_child — language-native splice representation
    //
    // Converts an OValue into a string that is syntactically valid source code
    // in language `lang`.  The result is inserted verbatim into the splice
    // buffer that is sent to the backend as `code`.
    //
    // Language-specific dispatch first; unrecognised languages fall through to
    // OValue::splice_repr(), which produces a conservative representation
    // that is valid in the widest range of languages.
    // ─────────────────────────────────────────────────────────────────────────

    fn render_child(&self, lang: &str, val: &OValue) -> String {
        match lang {
            // ── Python ──────────────────────────────────────────────────────
            // Produce a valid Python literal so the spliced code compiles
            // without the user having to quote things manually.
            "python" | "py" => render_python(val),

            // ── HTML ─────────────────────────────────────────────────────────
            // Produce embeddable HTML markup.  OBlob images become data-URI
            // <img> tags; everything else falls back to splice_repr or
            // direct string embedding.
            "html" => render_html(val),

            // ── LaTeX ────────────────────────────────────────────────────────
            "latex" | "tex" => render_latex(val),

            // ── Markdown ─────────────────────────────────────────────────────
            "markdown" | "md" => render_markdown(val),

            // ── Nix family ───────────────────────────────────────────────────
            // Produce syntactically valid Nix expressions so that O values
            // from prior blocks can be spliced into Nix code via $var.
            "nix" | "nix_store" | "nixos_test" => render_nix(val),

            // ── Default: use the conservative cross-language representation ──
            _ => val.splice_repr(),
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Language-specific renderers
// ═════════════════════════════════════════════════════════════════════════════

// ── Python ───────────────────────────────────────────────────────────────────

fn render_nix(val: &OValue) -> String {
    match val {
        OValue::Null => "null".to_string(),
        OValue::Bool { v } => {
            if *v { "true".to_string() } else { "false".to_string() }
        }
        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => v.to_string(),
        OValue::Str { v } => serde_json::to_string(v).unwrap_or_else(|_| "\"".to_string()),
        OValue::Html { v } => serde_json::to_string(v).unwrap_or_else(|_| "\"".to_string()),
        OValue::StorePath { path } => serde_json::to_string(path).unwrap_or_else(|_| "\"".to_string()),
        OValue::List { v } => {
            let items = v.iter().map(render_nix).collect::<Vec<_>>().join(" ");
            format!("[ {} ]", items)
        }
        OValue::Map { v } => {
            let items = v.iter()
                .map(|(k, val)| format!("{} = {};", k, render_nix(val)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{{ {} }}", items)
        }
        OValue::Blob { v, .. } => serde_json::to_string(v).unwrap_or_else(|_| "\"".to_string()),
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

        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => {
            let s = v.to_string();
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s
            } else {
                format!("{}.0", s)
            }
        }

        OValue::Str { v } => {
            serde_json::to_string(v).unwrap_or_else(|_| "''".to_string())
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
            let items = v
                .iter()
                .map(render_python)
                .collect::<Vec<_>>()
                .join(", ");

            format!("[{}]", items)
        }

        OValue::Map { v } => {
            let items = v
                .iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| "''".to_string());
                    format!("{}: {}", key, render_python(val))
                })
                .collect::<Vec<_>>()
                .join(", ");

            format!("{{{}}}", items)
        }

        OValue::Blob { v, mime } => {
            let mime_lit = serde_json::to_string(mime).unwrap_or_else(|_| "''".to_string());
            let data_lit = serde_json::to_string(v).unwrap_or_else(|_| "''".to_string());

            format!("{{'mime': {}, 'base64': {}}}", mime_lit, data_lit)
        }
    }
}

// ── HTML ─────────────────────────────────────────────────────────────────────

fn render_html(val: &OValue) -> String {
    match val {
        OValue::Null => String::new(),

        OValue::Bool { v } => html_escape(&v.to_string()),
        OValue::Int { v } => html_escape(&v.to_string()),
        OValue::Float { v } => html_escape(&v.to_string()),

        OValue::Str { v } => v.clone(),
        OValue::Html { v } => v.clone(),

        OValue::StorePath { path } => {
            format!(
                "<code class=\"o-store-path\">{}</code>",
                html_escape(path)
            )
        }

        OValue::List { v } => {
            let items = v
                .iter()
                .map(|item| format!("<li>{}</li>", render_html(item)))
                .collect::<Vec<_>>()
                .join("");
            format!("<ul>{}</ul>", items)
        }

        OValue::Map { v } => {
            v.iter()
                .map(|(k, val)| {
                    format!(
                        "<div data-o-key=\"{}\">{}</div>",
                        html_escape(k),
                        render_html(val)
                    )
                })
                .collect::<Vec<_>>()
                .join("")
        }

        OValue::Blob { v, mime } => render_html_blob(v, mime),
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
        b64.len() * 3 / 4,  // approximate decoded byte count
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
        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => v.to_string(),
        OValue::Str { v } => v.clone(),
        OValue::Html { v } => v.clone(),
        OValue::StorePath { path } => {
            format!("\\texttt{{{}}}", path.replace("_", "\\_"))
        }
        OValue::List { v } => {
            v.iter()
                .map(render_latex)
                .collect::<Vec<_>>()
                .join(", ")
        }
        OValue::Map { v } => {
            v.iter()
                .map(|(k, val)| format!("{}: {}", k, render_latex(val)))
                .collect::<Vec<_>>()
                .join(", ")
        }
        OValue::Blob { mime, .. } => format!("\\texttt{{<blob:{}>}}", mime),
    }
}

// ── Markdown ──────────────────────────────────────────────────────────────────

fn render_markdown(val: &OValue) -> String {
    match val {
        OValue::Null => String::new(),
        OValue::Bool { v } => v.to_string(),
        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => v.to_string(),
        OValue::Str { v } => v.clone(),
        OValue::Html { v } => v.clone(),
        OValue::StorePath { path } => format!("`{}`", path),
        OValue::List { v } => {
            v.iter()
                .map(render_markdown)
                .collect::<Vec<_>>()
                .join("\n")
        }
        OValue::Map { v } => {
            v.iter()
                .map(|(k, val)| format!("**{}**: {}", k, render_markdown(val)))
                .collect::<Vec<_>>()
                .join("\n")
        }
        OValue::Blob { mime, .. } => format!("<blob:{}>", mime),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── render_child: Python ──────────────────────────────────────────────────

    #[test]
    fn python_null_renders_as_none() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("python", &OValue::Null), "None");
    }

    #[test]
    fn python_bool_true_renders_as_title_case() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("python", &OValue::bool_(true)),  "True");
        assert_eq!(e.render_child("python", &OValue::bool_(false)), "False");
    }

    #[test]
    fn python_str_is_repr_quoted() {
        let e = Evaluator::new("/tmp".into());
        let s = e.render_child("python", &OValue::str_("hello world"));
        assert_eq!(s, "\"hello world\"");
    }

    #[test]
    fn python_str_with_internal_quotes_is_escaped() {
        let e = Evaluator::new("/tmp".into());
        let s = e.render_child("python", &OValue::str_("say \"hi\""));
        // Rust {:?} on &str escapes interior double-quotes with backslash
        assert!(s.starts_with('"') && s.ends_with('"'));
        assert!(s.contains("\\\""));
    }

    #[test]
    fn python_float_always_has_decimal() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("python", &OValue::float(3.0)), "3.0");
        assert_eq!(e.render_child("python", &OValue::float(3.5)), "3.5");
    }

    #[test]
    fn python_list_renders_as_list_literal() {
        let e = Evaluator::new("/tmp".into());
        let v = OValue::list(vec![OValue::int(1), OValue::int(2), OValue::int(3)]);
        assert_eq!(e.render_child("python", &v), "[1, 2, 3]");
    }

    // ── render_child: HTML ────────────────────────────────────────────────────

    #[test]
    fn html_null_is_empty_string() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("html", &OValue::Null), "");
    }

    #[test]
    fn html_blob_image_png_becomes_img_data_uri() {
        let e = Evaluator::new("/tmp".into());
        let png = OValue::blob(b"\x89PNG", "image/png");
        let result = e.render_child("html", &png);
        assert!(result.starts_with("<img src=\"data:image/png;base64,"));
        assert!(result.ends_with("\" />"));
    }

    #[test]
    fn html_list_becomes_ul() {
        let e = Evaluator::new("/tmp".into());
        let v = OValue::list(vec![OValue::str_("a"), OValue::str_("b")]);
        let result = e.render_child("html", &v);
        assert!(result.starts_with("<ul>"));
        assert!(result.contains("<li>a</li>"));
        assert!(result.contains("<li>b</li>"));
        assert!(result.ends_with("</ul>"));
    }

    #[test]
    fn html_str_is_passed_through_unescaped() {
        let e = Evaluator::new("/tmp".into());
        let result = e.render_child("html", &OValue::str_("<b>bold</b>"));
        assert_eq!(result, "<b>bold</b>");
    }

    // ── render_child: default fallback ───────────────────────────────────────

    #[test]
    fn unknown_lang_falls_back_to_splice_repr() {
        let e = Evaluator::new("/tmp".into());
        let v = OValue::int(42);
        assert_eq!(e.render_child("cobol", &v), v.splice_repr());
    }

    // ── render_child: nix ────────────────────────────────────────────────────

    #[test]
    fn nix_null_renders_as_null() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("nix", &OValue::Null), "null");
    }

    #[test]
    fn nix_bool_renders_correctly() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("nix", &OValue::bool_(true)),  "true");
        assert_eq!(e.render_child("nix", &OValue::bool_(false)), "false");
    }

    #[test]
    fn nix_int_renders_as_integer() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("nix", &OValue::int(42)),  "42");
        assert_eq!(e.render_child("nix", &OValue::int(-1)), "-1");
    }

    #[test]
    fn nix_str_renders_as_double_quoted() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("nix", &OValue::str_("hello")), "\"hello\"");
    }

    #[test]
    fn nix_list_renders_with_space_delimiters() {
        let e = Evaluator::new("/tmp".into());
        let v = OValue::list(vec![OValue::int(1), OValue::int(2)]);
        assert_eq!(e.render_child("nix", &v), "[ 1 2 ]");
    }

    #[test]
    fn nix_store_path_uses_nix_renderer() {
        let e = Evaluator::new("/tmp".into());
        let v = OValue::store_path("/nix/store/abc-hello");
        // nix and nix_store both dispatch to render_nix
        let nix_out   = e.render_child("nix",       &v);
        let store_out = e.render_child("nix_store",  &v);
        assert_eq!(nix_out, store_out);
    }

    #[test]
    fn nixos_test_uses_nix_renderer() {
        let e = Evaluator::new("/tmp".into());
        // nixos_test^() should also use render_nix for splicing
        let v = OValue::int(99);
        assert_eq!(e.render_child("nixos_test", &v), "99");
    }

    // ── eval_document semantics ───────────────────────────────────────────────

    #[test]
    fn eval_document_empty_returns_null() {
        let mut e = Evaluator::new("/tmp".into());
        let result = e.eval_document(vec![]).unwrap();
        assert_eq!(result, OValue::Null);
    }

    #[test]
    fn eval_document_rawtext_returns_ostr() {
        let mut e = Evaluator::new("/tmp".into());
        let result = e
            .eval_document(vec![ONode::RawText("hello".to_string())])
            .unwrap();
        assert_eq!(result, OValue::str_("hello"));
    }

    #[test]
    fn eval_document_all_null_returns_null() {
        let mut e = Evaluator::new("/tmp".into());
        let result = e.eval_document(vec![ONode::RawText(String::new())]).unwrap();
        // OStr("") is not null — empty string is a valid value
        assert!(!result.is_null());
    }

    #[test]
    fn eval_document_last_nonnull_wins() {
        let mut e = Evaluator::new("/tmp".into());
        // Two RawText nodes: the last non-null should be the second
        let result = e
            .eval_document(vec![
                ONode::RawText("first".to_string()),
                ONode::RawText("second".to_string()),
            ])
            .unwrap();
        assert_eq!(result, OValue::str_("second"));
    }

    #[test]
    fn eval_node_varref_undefined_is_error() {
        let mut e = Evaluator::new("/tmp".into());
        let result = e.eval_node(&ONode::VarRef("missing".to_string()), &HashMap::new());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("missing"));
    }

    #[test]
    fn eval_node_varref_found_returns_value() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("x".to_string(), OValue::int(99));
        let result = e
            .eval_node(&ONode::VarRef("x".to_string()), &scope)
            .unwrap();
        assert_eq!(result, OValue::int(99));
    }
}
