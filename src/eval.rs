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

use crate::nix_ops;
use crate::parser::ONode;
use crate::process::ProcessRegistry;
use crate::value::{OValue, RequestKind};

// ═════════════════════════════════════════════════════════════════════════════
// Policy — WHEN does a Request execute?
//
// Step-2 ships only Eager. Lazy is reserved for STEP3, when the surface
// syntax for entering a lazy region (block? attribute? directive?) is
// designed properly.
//
// The reason this is an enum and not a bool: STEP3 will add at least
// `Autonomous` (scheduler-decided), and possibly speculative / goal-driven
// variants. Making it an enum from the start keeps additions additive.
// ═════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Requests are auto-resolved (executed) at let-binding boundaries and
    /// at the top level. The user sees Derivations/StorePaths, never raw
    /// Requests. This is the only policy implemented in step 2 and is the
    /// hardcoded default in eval_document.
    Eager,

    /// STEP3: Requests pass through let-bindings as values. The user must
    /// explicitly call `now(req)` to perform a request. Not yet wired —
    /// changing the policy here has no effect because there is no surface
    /// syntax to enter a Lazy region.
    #[allow(dead_code)]
    Lazy,
}

// ═════════════════════════════════════════════════════════════════════════════
// Executor — HOW is a Request performed?
//
// Step-2 ships a synchronous, single-threaded ImmediateExecutor with an
// in-memory cache keyed by fingerprint. STEP3 will introduce a scheduler
// that implements this same trait but with concurrency, batching, persistent
// caching, and policy-driven dispatch.
//
// The trait stays narrow on purpose: anything richer (parallel completion,
// progress reporting, cancellation) gets added when STEP3 actually needs it,
// not now on speculation.
// ═════════════════════════════════════════════════════════════════════════════

pub trait Executor {
    /// Perform a Request. Recursively executes nested Requests in the source
    /// chain before doing this request's own work. Cache hits short-circuit.
    fn execute(&mut self, req: &OValue) -> Result<OValue>;
}

/// The step-2 executor: synchronous immediate-mode with an in-memory cache.
///
/// STEP3 deferrals:
///   - cache is in-memory only; STEP3 wants a persistent on-disk cache
///     (probably backed by Nix's store, since drv_path IS a cache key)
///   - no concurrency; STEP3's scheduler runs independent requests in parallel
///   - no progress callbacks, cancellation, or retry — added in STEP3 when
///     the scheduler needs them
pub struct ImmediateExecutor {
    /// Fingerprint → result. Lives for the duration of the Evaluator.
    cache: HashMap<String, OValue>,
}

impl ImmediateExecutor {
    pub fn new() -> Self {
        Self { cache: HashMap::new() }
    }
}

impl Default for ImmediateExecutor {
    fn default() -> Self { Self::new() }
}

impl Executor for ImmediateExecutor {
    fn execute(&mut self, req: &OValue) -> Result<OValue> {
        let (kind, source, fingerprint) = match req {
            OValue::Request { kind, source, fingerprint } =>
                (*kind, source.as_ref().clone(), fingerprint.clone()),
            other => bail!(
                "Executor::execute expected a Request, got {}", other.type_name()
            ),
        };

        // Cache hit short-circuits any recursive work.
        if let Some(hit) = self.cache.get(&fingerprint) {
            return Ok(hit.clone());
        }

        // If source is itself a Request, recursively perform it first.
        // This is how `realise(instantiate(expr))` works: the outer Request
        // executes; it sees source is a Request; it executes that first to
        // get the actual Derivation; then performs the realise.
        let resolved_source = match source {
            OValue::Request { .. } => self.execute(&source)?,
            other => other,
        };

        let result = match kind {
            RequestKind::Instantiate => nix_ops::instantiate_nix(&resolved_source)?,
            RequestKind::Realise     => nix_ops::realise_nix(&resolved_source)?,
        };

        self.cache.insert(fingerprint, result.clone());
        Ok(result)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Evaluator
// ═════════════════════════════════════════════════════════════════════════════

pub struct Evaluator {
    registry: ProcessRegistry,
    /// Directory containing one backend shim executable per language.
    /// Shim path for a language `lang` is `shim_dir/lang`.
    shim_dir: PathBuf,

    /// Current evaluation policy. Step-2: always Eager; STEP3 will allow
    /// scoped overrides via a lazy region marker.
    policy: Policy,

    /// The executor used to perform Requests under the current policy.
    /// Step-2: ImmediateExecutor; STEP3 swaps in a scheduler.
    executor: Box<dyn Executor>,
}

impl Evaluator {
    pub fn new(shim_dir: PathBuf) -> Self {
        Evaluator {
            registry: ProcessRegistry::new(),
            shim_dir,
            policy: Policy::Eager,
            executor: Box::new(ImmediateExecutor::new()),
        }
    }

    /// Replace the executor. Used by tests; STEP3's scheduler will use this
    /// to install itself.
    #[allow(dead_code)]
    pub fn with_executor(mut self, exec: Box<dyn Executor>) -> Self {
        self.executor = exec;
        self
    }

    /// Auto-resolve a Request under the current policy. Under Eager (the
    /// only policy in step 2), Requests are executed; under Lazy (STEP3)
    /// they would pass through unchanged.
    fn auto_resolve(&mut self, v: OValue) -> Result<OValue> {
        match (self.policy, &v) {
            (Policy::Eager, OValue::Request { .. }) => self.executor.execute(&v),
            _ => Ok(v),
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
                    // STEP-2: auto-resolve Requests under the Eager policy
                    // before binding. This is the request/perform boundary
                    // expressed at the user's let-line.
                    let raw   = self.eval_node(expr, &scope)?;
                    let value = self.auto_resolve(raw)?;
                    scope.insert(name.clone(), value.clone());

                    if !matches!(value, OValue::Null) {
                        last = value;
                    }
                }

                _ => {
                    let raw   = self.eval_node(&node, &scope)?;
                    let value = self.auto_resolve(raw)?;

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

            ONode::Call { fn_name, args } => {
                self.eval_call(fn_name, args, scope)
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Call dispatch — the step-2 built-ins
    //
    // Three built-ins ship in step 2:
    //   instantiate(expr)   → Request[Instantiate]
    //   realise(drv)        → Request[Realise]
    //   now(req)            → executes the request immediately, even under
    //                         a Lazy policy (which doesn't exist yet, but the
    //                         operator is here so STEP3 doesn't need to
    //                         retrofit it)
    //
    // STEP3 builtins to add:
    //   lazy(...)           → enter a Lazy region (or be a block, TBD)
    //   batch(req, req, ..) → bundle multiple requests for the scheduler
    //   activate(cfg)       → OS-as-participant: switch system to a config
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_call(
        &mut self,
        fn_name: &str,
        args:    &[ONode],
        scope:   &HashMap<String, OValue>,
    ) -> Result<OValue> {
        // Evaluate args left-to-right (applicative order, like everywhere else).
        let arg_vals: Vec<OValue> = args
            .iter()
            .map(|a| self.eval_node(a, scope))
            .collect::<Result<_>>()?;

        match fn_name {
            "instantiate" => {
                if arg_vals.len() != 1 {
                    bail!("instantiate(expr) takes exactly 1 argument, got {}", arg_vals.len());
                }
                Ok(OValue::request(RequestKind::Instantiate, arg_vals.into_iter().next().unwrap()))
            }
            "realise" => {
                if arg_vals.len() != 1 {
                    bail!("realise(drv) takes exactly 1 argument, got {}", arg_vals.len());
                }
                Ok(OValue::request(RequestKind::Realise, arg_vals.into_iter().next().unwrap()))
            }
            "now" => {
                if arg_vals.len() != 1 {
                    bail!("now(req) takes exactly 1 argument, got {}", arg_vals.len());
                }
                let req = arg_vals.into_iter().next().unwrap();
                match &req {
                    OValue::Request { .. } => self.executor.execute(&req),
                    other => bail!(
                        "now(req) expected a Request, got {}", other.type_name()
                    ),
                }
            }
            other => bail!("Unknown built-in function: `{}(...)`", other),
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
        // For `nix_expr` blocks we also collect the evaluated child OValues as
        // deps (by reference, step-1 decision) so the returned ONixExpr carries
        // the full dependency tree for later re-traversal.
        let mut buf  = String::new();
        let mut deps: Vec<OValue> = Vec::new();

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
                    if lang == "nix_expr" {
                        deps.push(val.clone());
                    }
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
                    if lang == "nix_expr" {
                        deps.push(child_val);
                    }
                }

                ONode::Call { fn_name, args } => {
                    // STEP-2: the parser does not currently emit a Call inside
                    // a typed-expr body (the `expected_closer.is_none()` guard
                    // in parser::parse_until prevents that). This arm exists
                    // for exhaustiveness — and for forward-compatibility with
                    // STEP3, when Calls may be allowed inside bodies. The
                    // semantics mirror nested TypedExpr: evaluate the call,
                    // auto-resolve any Request under Eager, render the result
                    // via the parent language's render_child, splice in.
                    let raw = self.eval_call(fn_name, args, scope)?;
                    let child_val = self.auto_resolve(raw)?;
                    buf.push_str(&self.render_child(lang, &child_val));
                    if lang == "nix_expr" {
                        deps.push(child_val);
                    }
                }
            }
        }

        // Short-circuit for `nix_expr`: return a lazy ONixExpr instead of
        // calling the Nix shim immediately.  The fingerprint is sha256(body)
        // — the cheap step-1 scheme.  `nix^` (immediate evaluation) is
        // unchanged (step-1 decision, option a).
        if lang == "nix_expr" {
            return Ok(OValue::nix_expr(buf, deps));
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
        // An ONixExpr spliced into a Nix context is its already-assembled body —
        // it is a valid Nix expression that can be parenthesised inline.
        OValue::NixExpr { body, .. } => format!("({})", body),
        // A Derivation in a Nix context is its .drv path literal.
        OValue::Derivation { drv_path, .. } => serde_json::to_string(drv_path)
            .unwrap_or_else(|_| "\"".to_string()),
        // A Request rendered into Nix source is almost certainly a user error —
        // the user spliced a control value into source text. We embed the
        // splice marker; STEP3 can elevate this to a hard error or auto-resolve.
        OValue::Request { kind, fingerprint, .. } => {
            let k = match kind {
                RequestKind::Instantiate => "instantiate",
                RequestKind::Realise     => "realise",
            };
            format!("\"<request:{} fp={}>\"", k, &fingerprint[..8])
        }
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

        OValue::NixExpr { body, fingerprint, deps } => {
            let body_lit = serde_json::to_string(body).unwrap_or_else(|_| "''".to_string());
            let fp_lit   = serde_json::to_string(fingerprint).unwrap_or_else(|_| "''".to_string());
            let deps_rendered = deps
                .iter()
                .map(render_python)
                .collect::<Vec<_>>()
                .join(", ");
            format!("ONixExpr({}, fp={}, deps=[{}])", body_lit, fp_lit, deps_rendered)
        }

        OValue::Derivation { drv_path, outputs, .. } => {
            let drv_lit = serde_json::to_string(drv_path).unwrap_or_else(|_| "''".to_string());
            let outs_lit = outputs
                .iter()
                .map(|o| serde_json::to_string(o).unwrap_or_else(|_| "''".to_string()))
                .collect::<Vec<_>>()
                .join(", ");
            format!("ODerivation({}, outputs=[{}])", drv_lit, outs_lit)
        }

        OValue::Request { kind, fingerprint, .. } => {
            let k = match kind {
                RequestKind::Instantiate => "instantiate",
                RequestKind::Realise     => "realise",
            };
            let fp_lit = serde_json::to_string(fingerprint).unwrap_or_else(|_| "''".to_string());
            format!("ORequest(kind={:?}, fp={})", k, fp_lit)
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

        OValue::NixExpr { body, fingerprint, .. } => {
            format!(
                "<code class=\"o-nix-expr\" data-fp=\"{}\">{}</code>",
                html_escape(fingerprint),
                html_escape(body),
            )
        }

        OValue::Derivation { drv_path, outputs, .. } => {
            format!(
                "<code class=\"o-derivation\" data-outputs=\"{}\">{}</code>",
                html_escape(&outputs.join(",")),
                html_escape(drv_path),
            )
        }

        OValue::Request { kind, fingerprint, .. } => {
            let k = match kind {
                RequestKind::Instantiate => "instantiate",
                RequestKind::Realise     => "realise",
            };
            format!(
                "<code class=\"o-request\" data-kind=\"{}\" data-fp=\"{}\">&lt;request&gt;</code>",
                html_escape(k),
                html_escape(&fingerprint[..8]),
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
        OValue::NixExpr { body, .. } => format!("\\texttt{{{}}}", body.replace("_", "\\_")),
        OValue::Derivation { drv_path, .. } => {
            format!("\\texttt{{{}}}", drv_path.replace("_", "\\_"))
        }
        OValue::Request { kind, fingerprint, .. } => {
            let k = match kind {
                RequestKind::Instantiate => "instantiate",
                RequestKind::Realise     => "realise",
            };
            format!("\\texttt{{<request:{} fp={}>}}", k, &fingerprint[..8])
        }
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
        OValue::NixExpr { body, .. } => format!("`{}`", body),
        OValue::Derivation { drv_path, .. } => format!("`{}`", drv_path),
        OValue::Request { kind, fingerprint, .. } => {
            let k = match kind {
                RequestKind::Instantiate => "instantiate",
                RequestKind::Realise     => "realise",
            };
            format!("`<request:{} fp={}>`", k, &fingerprint[..8])
        }
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

    // ── nix_expr backend ─────────────────────────────────────────────────────

    /// `nix_expr^(...)_nix_expr` must return an ONixExpr without calling the
    /// Nix shim.  No shim process is spawned — the body is captured lazily.
    #[test]
    fn nix_expr_block_returns_onixexpr_without_calling_shim() {
        let mut e = Evaluator::new("/tmp".into());
        let result = e.eval_typed_expr(
            "nix_expr",
            u32::MAX,
            &[ONode::RawText("pkgs.hello".to_string())],
            &HashMap::new(),
        ).unwrap();

        assert!(result.is_nix_expr(), "expected ONixExpr, got {:?}", result);

        if let OValue::NixExpr { body, deps, fingerprint } = &result {
            assert_eq!(body, "pkgs.hello");
            assert!(deps.is_empty());
            assert_eq!(fingerprint.len(), 64, "fingerprint must be 64 hex chars");
        }
    }

    /// Child OValues from inner typed expressions should appear in deps
    /// and their rendered form should be spliced into body.
    #[test]
    fn nix_expr_block_collects_deps_from_child_typed_exprs() {
        let mut e    = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("n".to_string(), OValue::int(7));

        // nix_expr^( prefix $n suffix )_nix_expr
        // $n is a VarRef that resolves to OValue::Int(7)
        let body_nodes = vec![
            ONode::RawText("prefix ".to_string()),
            ONode::VarRef("n".to_string()),
            ONode::RawText(" suffix".to_string()),
        ];

        let result = e.eval_typed_expr("nix_expr", u32::MAX, &body_nodes, &scope).unwrap();

        if let OValue::NixExpr { body, deps, .. } = &result {
            // render_nix for OInt(7) → "7"
            assert_eq!(body, "prefix 7 suffix");
            assert_eq!(deps.len(), 1);
            assert_eq!(deps[0], OValue::int(7));
        } else {
            panic!("expected OValue::NixExpr, got {:?}", result);
        }
    }

    /// A NixExpr value spliced into a nix context is parenthesised so it
    /// composes cleanly as a sub-expression.
    #[test]
    fn nix_expr_render_in_nix_context_is_parenthesised() {
        let e   = Evaluator::new("/tmp".into());
        let val = OValue::nix_expr("pkgs.hello", vec![]);
        let rendered = e.render_child("nix", &val);
        assert_eq!(rendered, "(pkgs.hello)");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // STEP-2: Executor, dispatch, auto-resolve
    //
    // We test the orchestration without actually shelling out to Nix by
    // installing a MockExecutor that records calls and returns canned values.
    // The real `nix eval`/`nix build` integration is tested in nix_ops.rs's
    // #[ignore]'d integration tests.
    // ─────────────────────────────────────────────────────────────────────────

    /// Test executor that returns canned Derivations / StorePaths and records
    /// every fingerprint it was asked to execute. Used to verify the orchestration
    /// in the Evaluator without touching Nix.
    struct MockExecutor {
        calls: Vec<String>,
    }

    impl MockExecutor {
        fn new() -> Self { Self { calls: vec![] } }
    }

    impl Executor for MockExecutor {
        fn execute(&mut self, req: &OValue) -> Result<OValue> {
            let (kind, source, fingerprint) = match req {
                OValue::Request { kind, source, fingerprint } =>
                    (*kind, source.as_ref().clone(), fingerprint.clone()),
                _ => panic!("MockExecutor only handles Requests"),
            };
            self.calls.push(fingerprint);

            // Chained source: recursively execute first to resolve to a non-Request.
            let resolved = match source {
                OValue::Request { .. } => self.execute(&source)?,
                other => other,
            };

            match (kind, &resolved) {
                (RequestKind::Instantiate, OValue::NixExpr { .. }) => {
                    Ok(OValue::derivation(
                        "/nix/store/mockhash-foo.drv",
                        vec!["out".into()],
                        vec![],
                    ))
                }
                (RequestKind::Realise, OValue::Derivation { .. }) => {
                    Ok(OValue::store_path("/nix/store/mockhash-foo"))
                }
                (k, s) => panic!("MockExecutor: unexpected ({:?}, {})", k, s.type_name()),
            }
        }
    }

    /// `let drv = instantiate($expr)` under Eager policy must bind `drv` to a
    /// Derivation, not to a Request. The Request is auto-resolved at the
    /// let-binding boundary.
    #[test]
    fn eager_let_auto_resolves_instantiate_request() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        // Build the AST directly: let drv = instantiate($expr)
        let call = ONode::Call {
            fn_name: "instantiate".into(),
            args:    vec![ONode::VarRef("expr".into())],
        };
        let raw = e.eval_node(&call, &scope).unwrap();
        // Before auto_resolve, the call produces a Request value.
        assert!(raw.is_request(),
            "eval_call should return a Request before auto-resolve");

        let resolved = e.auto_resolve(raw).unwrap();
        assert!(resolved.is_derivation(),
            "Eager policy should auto-resolve Instantiate to a Derivation");
    }

    /// `realise(instantiate($expr))` must chain — the outer Request's source
    /// is the inner Request, and the executor walks the chain.
    #[test]
    fn nested_call_produces_chained_request_and_resolves_to_store_path() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let inner = ONode::Call {
            fn_name: "instantiate".into(),
            args:    vec![ONode::VarRef("expr".into())],
        };
        let outer = ONode::Call {
            fn_name: "realise".into(),
            args:    vec![inner],
        };

        let raw = e.eval_node(&outer, &scope).unwrap();
        if let OValue::Request { kind, source, .. } = &raw {
            assert_eq!(*kind, RequestKind::Realise);
            assert!(source.is_request(), "outer request's source must be inner Request");
        } else { panic!("expected Request, got {:?}", raw); }

        let resolved = e.auto_resolve(raw).unwrap();
        if let OValue::StorePath { path } = &resolved {
            assert!(path.starts_with("/nix/store/"));
        } else { panic!("expected StorePath, got {:?}", resolved); }
    }

    /// The ImmediateExecutor's cache must hit on identical fingerprints.
    /// Two requests built from the same NixExpr have the same fingerprint
    /// (by content_identity composition) and so share a cache slot.
    #[test]
    fn executor_cache_hits_on_repeated_fingerprint() {
        let mut exec = ImmediateExecutor::new();

        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req1 = OValue::request(RequestKind::Instantiate, expr.clone());
        let req2 = OValue::request(RequestKind::Instantiate, expr);

        // Pre-seed the cache so we never actually call nix.
        if let OValue::Request { fingerprint, .. } = &req1 {
            exec.cache.insert(
                fingerprint.clone(),
                OValue::derivation("/nix/store/seeded.drv", vec!["out".into()], vec![]),
            );
        }

        let r1 = exec.execute(&req1).expect("cached execute should succeed");
        let r2 = exec.execute(&req2).expect("cached execute should succeed");
        // Same identity → same cached result on both calls.
        if let (OValue::Derivation { drv_path: d1, .. },
                OValue::Derivation { drv_path: d2, .. }) = (&r1, &r2) {
            assert_eq!(d1, d2);
            assert_eq!(d1, "/nix/store/seeded.drv");
        } else { panic!("expected Derivation results"); }
    }

    /// Unknown call names must error cleanly rather than silently no-op.
    #[test]
    fn unknown_call_errors_with_clear_message() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let call = ONode::Call {
            fn_name: "frobnicate".into(),
            args:    vec![],
        };
        let err = e.eval_node(&call, &scope).unwrap_err().to_string();
        assert!(err.contains("frobnicate"), "error must name the unknown function");
    }

    /// `now(req)` performs the request immediately and returns its result,
    /// regardless of policy. (In step 2 the policy is always Eager, so this
    /// is functionally redundant — but it'll matter in STEP3 when Lazy
    /// arrives.)
    #[test]
    fn now_call_executes_request_directly() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req  = OValue::request(RequestKind::Instantiate, expr);
        scope.insert("req".into(), req);

        let call = ONode::Call {
            fn_name: "now".into(),
            args:    vec![ONode::VarRef("req".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert!(result.is_derivation(),
            "now(req) on an Instantiate request should produce a Derivation");
    }
}
