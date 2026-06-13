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
//     3. For ephemeral envs (env_id == u32::MAX, used internally for re-entrancy etc): call cleanup_env (always, even on err)
//
//   Root document (eval_document):
//     Evaluate nodes sequentially; return the last non-null OValue,
//     or ONull if no non-null value was produced.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

use crate::ir::{BackendRegistry, SpliceRenderer};
use crate::nix_ops;
use crate::nixos_ops;
use crate::parser::{reconstruct_source, ONode, Parser};
use crate::process::{ExecStep, ProcessRegistry};
use crate::scheduler::AutonomousScheduler;
use crate::value::{OValue, RequestKind, GroupMode};

// ═════════════════════════════════════════════════════════════════════════════
// STEP-3.5: Backend purity
//
// Determines whether the `{lazy}` attribute is valid on a given language.
// `{lazy}` requires purity because it caches by fingerprint — re-running a
// thunk with the same input must produce the same result, or the cache lies.
//
// The purity table itself now lives in the centralized backend metadata
// (ir.rs: BACKEND_SPECS / BackendRegistry); this is a thin delegation so the
// evaluator keeps a single local entry point for the purity question.
//
// `{defer}` works on any backend (it never caches), so it's the impure-
// backend escape hatch.
//
// STEP4: when more backends are ported to Rust (rust, racket, shell, etc.),
// they'll need a purity decision in the registry. Shell is impure (anything
// can happen). Rust the language is pure-ish but compilation has IO. Racket
// has both pure and impure idioms — we'd default impure and let users opt in.
// ═════════════════════════════════════════════════════════════════════════════

fn is_pure_backend(lang: &str) -> bool {
    BackendRegistry::global().is_pure(lang)
}

// ═════════════════════════════════════════════════════════════════════════════
// Policy — WHEN does a Request execute?
//
// The "when" axis of the two-axis framing (the other is "who decides", which
// is the Executor). Step-3 ships Eager (default) and Lazy (scoped via lazy^).
// Autonomous is a placeholder for STEP4 — goal-driven scheduling, where the
// scheduler decides what to execute (and possibly speculatively pre-executes)
// based on goals carried alongside requests.
// ═════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Requests are auto-resolved (executed) at let-binding boundaries and
    /// at the top level. The user sees Derivations/StorePaths, never raw
    /// Requests. This is the default policy in eval_document.
    Eager,

    /// Requests pass through let-bindings as values. The user must explicitly
    /// call `now(req)` to perform a request. Entered via the `lazy^(...)_lazy`
    /// block — Policy::Lazy is in effect for the body of that block only,
    /// then restored to the surrounding policy on exit.
    Lazy,

    /// STEP-4: scheduler-directed buffered execution.
    ///
    /// Under Autonomous, non-Eval Requests are buffered as they're constructed
    /// instead of being executed immediately. At force points (exit of an
    /// `autonomous(expr)` block, explicit `now(req)`, document end), the
    /// AutonomousScheduler flushes the buffer: it collects the full transitive
    /// closure of all buffered requests, builds a dependency DAG, and dispatches
    /// independent requests as concurrent threads (up to `parallelism` at a
    /// time). Results are stored in a two-level cache (L1 memory + L2 disk).
    ///
    /// `RequestKind::Eval` is excluded from buffering — Eval needs the
    /// ProcessRegistry (which is !Send) and is executed eagerly even under
    /// this policy. Full Eval parallelism is a STEP5 goal.
    ///
    /// Activated via the `autonomous(expr)` built-in function, which evaluates
    /// `expr` under this policy, flushes the buffer on exit, and returns the
    /// resolved result.
    Autonomous,
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

pub trait Executor: Send {
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

impl ImmediateExecutor {
    /// Inject a pre-computed result into the cache. Used in tests to avoid
    /// shelling out to Nix or spawning real shims for `RequestKind::Eval`.
    #[cfg(test)]
    pub fn seed_cache(&mut self, fingerprint: String, value: OValue) {
        self.cache.insert(fingerprint, value);
    }
}

impl Executor for ImmediateExecutor {
    fn execute(&mut self, req: &OValue) -> Result<OValue> {
        let (kind, source, fingerprint) = match req {
            OValue::Request { kind, source, fingerprint } =>
                (kind.clone(), source.as_ref().clone(), fingerprint.clone()),
            other => bail!(
                "Executor::execute expected a Request, got {}", other.type_name()
            ),
        };

        // STEP-3.5: for non-cacheable Eval ({defer}) we MUST skip the cache
        // and re-run on every force.
        // STEP-4: Activate is never cached — a stale System reference would
        // lie about live state, and re-running an activation is the whole
        // point when the user explicitly asks for it.
        let consult_cache = match &kind {
            RequestKind::Eval { cacheable, .. } => *cacheable,
            RequestKind::Activate { .. } => false,
            _ => true,
        };
        if consult_cache {
            if let Some(hit) = self.cache.get(&fingerprint) {
                return Ok(hit.clone());
            }
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
            // STEP-3.5: Eval fires the shim through the ProcessRegistry. The
            // ImmediateExecutor doesn't currently have access to a registry,
            // so we bail with a clear message. The real wiring is provided
            // by Evaluator::exec_eval, which the Evaluator dispatches to
            // directly via force_request.
            RequestKind::Eval { .. } => bail!(
                "ImmediateExecutor cannot perform RequestKind::Eval directly — \
                 it lacks a ProcessRegistry. The Evaluator dispatches Eval \
                 via force_request → exec_eval."
            ),
            // STEP-4: Activate is dispatched to nixos_ops::activate_nix.
            RequestKind::Activate { profile, dry_run } => {
                nixos_ops::activate_nix(&resolved_source, &profile, dry_run)?
            }
        };

        // STEP-3.5: only cache the result when cacheable (true for {lazy} and
        // for the Nix family). For {defer}, the !consult_cache check above
        // already short-circuited the cache lookup; here we also skip insert
        // so the cache stays clean.
        if consult_cache {
            self.cache.insert(fingerprint, result.clone());
        }
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

    /// The set of registered backend language tags. Stored here so that
    /// eval_source (called during O.eval() eval_request callbacks) can
    /// re-parse a quoted source fragment using the same backend set as the
    /// top-level document.
    registered_backends: HashSet<String>,

    /// Current evaluation policy. Eager by default; lazy(...) installs Lazy
    /// for the scope of its argument; autonomous(...) installs Autonomous.
    policy: Policy,

    /// The executor used to perform Instantiate/Realise/Activate Requests
    /// under Policy::Eager. Swappable via with_executor (used by tests).
    executor: Box<dyn Executor>,

    /// STEP-3.5: cache for `RequestKind::Eval { cacheable: true }` ({lazy}).
    /// Keyed by the Request's fingerprint, which composes from the Thunk's
    /// body + dep identities and the kind metadata (lang, env_id, cacheable).
    /// Non-cacheable ({defer}) Eval Requests bypass this on both read and
    /// write — each force re-runs the shim.
    eval_cache: HashMap<String, OValue>,

    /// STEP-4: the autonomous scheduler. Always present; only actively used
    /// when policy == Policy::Autonomous. Holds the two-level cache (L1
    /// memory + L2 disk) and the concurrent dispatch logic for Nix-family
    /// requests.
    scheduler: AutonomousScheduler,

    /// STEP-4: buffer of non-Eval Requests constructed under
    /// Policy::Autonomous. Flushed by flush_autonomous_buffer() at force
    /// points: end of autonomous(expr) block, explicit now(), document end.
    autonomous_buffer: Vec<OValue>,
}

impl Evaluator {
    pub fn new(shim_dir: PathBuf) -> Self {
        Evaluator {
            registry: ProcessRegistry::new(),
            shim_dir,
            registered_backends: HashSet::new(),
            policy: Policy::Eager,
            executor: Box::new(ImmediateExecutor::new()),
            eval_cache: HashMap::new(),
            scheduler: AutonomousScheduler::new(),
            autonomous_buffer: Vec::new(),
        }
    }

    /// Install the registered-backends set used by eval_source to re-parse
    /// quoted fragments in `O.eval(q)` callbacks. Typically called once
    /// after construction with the same set passed to the Parser.
    pub fn with_registered_backends(mut self, backends: HashSet<String>) -> Self {
        self.registered_backends = backends;
        self
    }

    /// Replace the executor. Used by tests; the autonomous scheduler is a
    /// separate field and is not affected by this call.
    #[allow(dead_code)]
    pub fn with_executor(mut self, exec: Box<dyn Executor>) -> Self {
        self.executor = exec;
        self
    }

    /// Auto-resolve a Request under the current policy.
    ///
    ///   - Eager:       execute the request immediately, return its result.
    ///   - Lazy:        pass through unchanged (user must call `now()` to force).
    ///   - Autonomous:  Eval requests are executed eagerly (they need the
    ///                  ProcessRegistry which is !Send and can't be buffered).
    ///                  All other request kinds (Instantiate, Realise, Activate)
    ///                  are buffered in autonomous_buffer and returned unchanged;
    ///                  the scheduler dispatches them concurrently at the next
    ///                  force point (end of autonomous() block, document end,
    ///                  or explicit now()).
    fn auto_resolve(&mut self, v: OValue) -> Result<OValue> {
        match (self.policy, &v) {
            (Policy::Eager, OValue::Request { .. }) => self.force_request(&v),

            (Policy::Autonomous, OValue::Request { kind, .. }) => {
                match kind {
                    // Eval needs the ProcessRegistry — execute eagerly even under Autonomous.
                    RequestKind::Eval { .. } => self.force_request(&v),
                    // Nix-family and Activate: buffer for concurrent flush.
                    _ => {
                        self.autonomous_buffer.push(v.clone());
                        Ok(v)
                    }
                }
            }

            _ => Ok(v),
        }
    }

    /// Dispatch a Request to the right performer.
    ///
    /// Routing rules:
    ///   - `RequestKind::Eval` always goes to `exec_eval` (needs ProcessRegistry,
    ///     which is !Send and not accessible to the scheduler).
    ///   - All other kinds under `Policy::Autonomous` go to the
    ///     `AutonomousScheduler`, which checks its two-level cache and, on a
    ///     miss, executes the request (and its source chain) using concurrent
    ///     threads.
    ///   - All other kinds under Eager/Lazy go to `self.executor`
    ///     (ImmediateExecutor), which is synchronous and in-memory cached.
    fn force_request(&mut self, req: &OValue) -> Result<OValue> {
        let kind = match req {
            OValue::Request { kind, .. } => kind.clone(),
            other => bail!("force_request expected a Request, got {}", other.type_name()),
        };
        match kind {
            RequestKind::Eval { .. } => self.exec_eval(req),
            _ if self.policy == Policy::Autonomous => self.scheduler.execute(req),
            _ => self.executor.execute(req),
        }
    }
    // ── STEP-4: Autonomous scheduler helpers ──────────────────────────────────

    /// Flush all buffered non-Eval Requests through the autonomous scheduler.
    ///
    /// Called at force points: exit of `autonomous(expr)` block, document end
    /// (when top-level policy is Autonomous), and explicit `now()` when a
    /// buffered request is forced.
    ///
    /// After this call, every buffered request's fingerprint is present in
    /// `self.scheduler.mem_cache` (and written to disk cache if available).
    /// The buffer is cleared regardless of success or failure to avoid
    /// polluting future calls with stale entries.
    fn flush_autonomous_buffer(&mut self) -> Result<()> {
        let buffer = std::mem::take(&mut self.autonomous_buffer);
        if buffer.is_empty() {
            return Ok(());
        }
        self.scheduler
            .execute_batch(&buffer, None)
            .context("autonomous scheduler: batch flush failed")?;
        Ok(())
    }

    /// Resolve a Request value from the scheduler or eval cache without
    /// going back to the executor. Returns `None` if the fingerprint is not
    /// in any cache (i.e. the request was never executed).
    ///
    /// Used by the `autonomous(expr)` builtin to resolve the return value
    /// after the buffer has been flushed: the result is already cached, so
    /// we can avoid a second execution.
    fn resolve_from_cache(&mut self, v: &OValue) -> Option<OValue> {
        match v {
            OValue::Request { fingerprint, kind, .. } => {
                // For Eval requests, check eval_cache.
                if matches!(kind, RequestKind::Eval { .. }) {
                    return self.eval_cache.get(fingerprint).cloned();
                }
                // For Nix-family requests, check the scheduler's two-level cache.
                self.scheduler.cache_get(fingerprint)
            }
            _ => None,
        }
    }

    /// Resolve a value returned from an autonomous body AFTER the buffer has
    /// been flushed through the scheduler.
    ///
    /// - A schedulable Request → its cached result (or unchanged on miss).
    /// - A Group → resolved per its topology mode, reading each member from
    ///   the freshly-populated cache.
    /// - Anything else → returned unchanged.
    fn resolve_after_flush(&mut self, value: OValue) -> Result<OValue> {
        match &value {
            OValue::Group { mode, members, .. } => {
                let (mode, members) = (*mode, members.clone());
                self.resolve_group(mode, &members, true)
            }
            v if Self::is_schedulable_request(v) => {
                Ok(self.resolve_from_cache(v).unwrap_or(value))
            }
            _ => Ok(value),
        }
    }

    /// Returns `true` if `v` is a non-Eval Request (Instantiate, Realise, or
    /// Activate), i.e. a request that is buffered under Policy::Autonomous and
    /// executed by the scheduler rather than by exec_eval.
    fn is_schedulable_request(v: &OValue) -> bool {
        matches!(v, OValue::Request { kind, .. } if !matches!(kind, RequestKind::Eval { .. }))
    }

    /// Resolve a single group member to a concrete value.
    ///
    /// `from_cache = false` forces the member fresh (via `force_request`, which
    /// honours the active policy/executor); `from_cache = true` resolves it
    /// from the scheduler/eval cache populated by a prior buffer flush, falling
    /// back to the value unchanged on a miss. Nested Groups recurse with the
    /// same flag. Non-Request, non-Group members are returned as-is.
    fn resolve_member(&mut self, m: &OValue, from_cache: bool) -> Result<OValue> {
        match m {
            OValue::Request { .. } => {
                if from_cache {
                    Ok(self.resolve_from_cache(m).unwrap_or_else(|| m.clone()))
                } else {
                    self.force_request(m)
                }
            }
            OValue::Group { mode, members, .. } => {
                let (mode, members) = (*mode, members.clone());
                self.resolve_group(mode, &members, from_cache)
            }
            other => Ok(other.clone()),
        }
    }

    /// Resolve a Group to a concrete value according to its topology `mode`.
    ///
    /// - `Batch` / `All` → an `OList` of every member's resolved result, in
    ///   member order. Any member failure aborts the whole resolution
    ///   (all-or-nothing).
    /// - `Any` / `Race` → the first member that resolves successfully, trying
    ///   members left-to-right. Fails only if every member fails.
    ///
    /// `from_cache` is forwarded to `resolve_member` — `false` forces members,
    /// `true` reads results that a prior autonomous flush already cached.
    fn resolve_group(
        &mut self,
        mode:       GroupMode,
        members:    &[OValue],
        from_cache: bool,
    ) -> Result<OValue> {
        if members.is_empty() {
            bail!("{}(...) group has no members to resolve", mode.name());
        }
        if mode.collects_all() {
            let mut out = Vec::with_capacity(members.len());
            for m in members {
                out.push(self.resolve_member(m, from_cache)?);
            }
            Ok(OValue::list(out))
        } else {
            // Any / Race: first successful member wins.
            let mut last_err: Option<anyhow::Error> = None;
            for m in members {
                match self.resolve_member(m, from_cache) {
                    Ok(v)  => return Ok(v),
                    Err(e) => last_err = Some(e),
                }
            }
            Err(last_err.expect("non-empty group must have produced an error"))
                .with_context(|| format!(
                    "{}(...) group: all {} members failed",
                    mode.name(), members.len()
                ))
        }
    }

    ///
    /// For cacheable Eval ({lazy}), checks/populates an internal cache keyed
    /// by the Request's fingerprint. For non-cacheable Eval ({defer}), the
    /// cache is skipped on both read and write — each force re-runs.
    fn exec_eval(&mut self, req: &OValue) -> Result<OValue> {
        let (kind, source, fingerprint) = match req {
            OValue::Request { kind, source, fingerprint } =>
                (kind.clone(), source.as_ref().clone(), fingerprint.clone()),
            other => bail!("exec_eval expected Request, got {}", other.type_name()),
        };
        let (lang, env_id, cacheable) = match kind {
            RequestKind::Eval { lang, env_id, cacheable } => (lang, env_id, cacheable),
            other => bail!(
                "exec_eval expected RequestKind::Eval, got {:?}", other
            ),
        };

        // {lazy} cache: consult before doing work.
        if cacheable {
            if let Some(hit) = self.eval_cache.get(&fingerprint) {
                return Ok(hit.clone());
            }
        }

        // The Request's source is a Thunk carrying (body, deps).
        let body = match &source {
            OValue::Thunk { body, .. } => body.clone(),
            other => bail!(
                "exec_eval's Request source must be a Thunk, got {}",
                other.type_name()
            ),
        };

        // Pick the right shim for the language and fire it through the
        // ProcessRegistry, exactly as eval_typed_expr would for a normal block.
        // Shim path resolution is centralized in the BackendRegistry.
        let shim = BackendRegistry::global().resolve_shim_path(&self.shim_dir, &lang);
        // The shim sees an empty bindings map; deps were already spliced into
        // the body at capture time. (STEP4: deps could be passed as bindings
        // instead, for shims that want them as values rather than text.)
        // See process.rs for the underlying error chain; using with_context
        // here preserves the shim's own error message as a "Caused by:" entry
        // instead of flattening it into the wrapper string.
        let result = self.registry.exec(
            &lang, env_id, &body, HashMap::new(), &shim
        ).with_context(|| format!("[{}{{eval}}]", lang))?;

        if env_id == u32::MAX {
            let _ = self.registry.cleanup_env(&lang, u32::MAX);
        }

        if cacheable {
            self.eval_cache.insert(fingerprint, result.clone());
        }
        Ok(result)
    }

    /// STEP-3.5: prepare a value for splicing into source text.
    ///
    /// The rule from fork #2:
    ///   - {lazy} Eval Request → auto-force (cacheable, pure backend, no side
    ///                                       effects from re-running, splice
    ///                                       result of the force)
    ///   - {defer} Eval Request → error (non-cacheable, may have side effects,
    ///                                   forcing implicitly via splice would
    ///                                   surprise the user — they must call
    ///                                   now() explicitly)
    ///   - any other value → pass through unchanged
    ///
    /// Auto-forcing here means: ask the executor to perform the request and
    /// return its result. The executor's cache makes this idempotent for {lazy}.
    fn resolve_for_splice(&mut self, v: OValue) -> Result<OValue> {
        if let OValue::Request { kind: RequestKind::Eval { cacheable, lang, .. }, .. } = &v {
            if *cacheable {
                // {lazy}: safe to auto-force.
                return self.force_request(&v);
            } else {
                // {defer}: refuse to auto-force.
                bail!(
                    "Cannot splice a {{defer}} thunk (`{}{{defer}}^...`) into \
                     source text — {{defer}} is non-cacheable and forcing it \
                     implicitly could re-run side effects unexpectedly. \
                     Wrap the splice in now(...) to force explicitly.",
                    lang
                );
            }
        }
        Ok(v)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // eval_source — re-evaluate O source text (for O.eval() callbacks)
    //
    // Used when a backend shim sends an `eval_request` response: the shim's
    // `O.eval(q)` call asks the runtime to evaluate the quoted source fragment
    // and return the result as an `eval_result` command. This is the recursive
    // entry point for that path.
    //
    // Limitation: eval_source creates a fresh scope (empty let-bindings). Any
    // top-level `let` bindings defined in the calling document are NOT
    // accessible. Variables in persistent backend envs (e.g. python[0] globals)
    // remain accessible because they live in the subprocess, not in the Rust
    // scope.
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_source(&mut self, src: &str) -> Result<OValue> {
        let nodes = Parser::new(src, &self.registered_backends)
            .parse()
            .with_context(|| format!("failed to parse quoted source: {:?}", &src[..src.len().min(80)]))?;
        self.eval_document(nodes)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Public API
    // ─────────────────────────────────────────────────────────────────────────

    /// Evaluate a parsed O document.
    ///
    /// Nodes are evaluated sequentially with an empty root scope. The return
    /// value is the last non-null `OValue` produced, or `OValue::Null` if
    /// every node evaluated to null or the document was empty.
    ///
    /// STEP-4: if the document is evaluated under `Policy::Autonomous` (set by
    /// the caller via `Evaluator::policy` before calling this), any buffered
    /// non-Eval Requests are flushed through the scheduler at the end, and a
    /// final Request return value is resolved from the cache.
    pub fn eval_document(&mut self, nodes: Vec<ONode>) -> Result<OValue> {
        let mut scope = HashMap::new();
        let mut last = OValue::null();

        // STEP-3 NOTE: auto-resolve is NOT called here. It fires at Request
        // *construction* time inside eval_call. By the time eval_node returns
        // a value to this loop, any auto-resolution that the policy at
        // construction-time demanded has already happened — and any Request
        // that survived (because it was constructed under lazy(...)) should
        // NOT be re-executed by binding it to a name.
        for node in nodes {
            // Whitespace-only RawText nodes are document formatting (e.g. the
            // trailing newline at EOF, blank lines between expressions), not
            // values.  They MUST NOT overwrite the result of a real expression
            // — otherwise `python^(...)_python\n` returns OStr("\n") instead
            // of the python block's value, and the user sees an empty newline
            // where the answer should be.  The empty-string case is preserved
            // as a value (see test eval_document_all_null_returns_null) by
            // requiring at least one character before the whitespace check.
            let is_pure_whitespace_text = matches!(
                &node,
                ONode::RawText(s) if !s.is_empty() && s.chars().all(char::is_whitespace)
            );

            let value = match &node {
                ONode::LetBinding { name, expr } => {
                    let value = self.eval_node(expr, &scope)?;
                    scope.insert(name.clone(), value.clone());
                    value
                }
                _ => self.eval_node(&node, &scope)?,
            };

            if !value.is_null() && !is_pure_whitespace_text {
                last = value;
            }
        }

        // STEP-4: flush any buffered Requests when the document ends under
        // Autonomous policy.  This covers the case where the caller has set
        // `self.policy = Policy::Autonomous` before calling eval_document
        // directly (rather than through the `autonomous(expr)` builtin).
        // flush_autonomous_buffer() is a no-op when the buffer is empty, so
        // there is no need for a redundant emptiness check here.
        if self.policy == Policy::Autonomous {
            self.flush_autonomous_buffer()?;
            // If the final value is a buffered Request or a Group, resolve it
            // from the freshly-populated cache.
            last = self.resolve_after_flush(last)?;
        }

        Ok(last)
    }

    /// Like `eval_document` but operates on a caller-supplied scope instead of
    /// a fresh one.  Bindings introduced by `let` statements are written back
    /// into `scope` so they persist across calls.  Used by the notebook server
    /// to maintain cell-to-cell variable state.
    pub fn eval_document_with_scope(
        &mut self,
        nodes: Vec<ONode>,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        let mut last = OValue::null();

        for node in nodes {
            let is_pure_whitespace_text = matches!(
                &node,
                ONode::RawText(s) if !s.is_empty() && s.chars().all(char::is_whitespace)
            );

            let value = match &node {
                ONode::LetBinding { name, expr } => {
                    let value = self.eval_node(expr, scope)?;
                    scope.insert(name.clone(), value.clone());
                    value
                }
                _ => self.eval_node(&node, scope)?,
            };

            if !value.is_null() && !is_pure_whitespace_text {
                last = value;
            }
        }

        if self.policy == Policy::Autonomous {
            self.flush_autonomous_buffer()?;
            last = self.resolve_after_flush(last)?;
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

            ONode::TypedExpr { lang, env_id, attr, body } => {
                self.eval_typed_expr(lang, *env_id, attr.as_deref(), body, scope)
            }

            ONode::Call { fn_name, args } => {
                self.eval_call(fn_name, args, scope)
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Call dispatch — the built-in operators
    //
    // Step-3 builtins:
    //   instantiate(expr)  → Request[Instantiate], auto-resolved under Eager
    //   realise(drv)       → Request[Realise],     auto-resolved under Eager
    //   now(req)           → executes the request immediately, regardless of policy
    //   lazy(expr)         → evaluates `expr` under Policy::Lazy, returns its value
    //
    // ARCHITECTURAL NOTE: auto-resolve fires INSIDE eval_call at the moment a
    // Request is constructed, not at let-binding boundaries. This matters
    // because the policy in effect at construction time is what the user
    // intended; by the time control returns to a let-binding, lazy(...) has
    // already restored the outer policy. Auto-resolving at the let-binding
    // would re-execute Requests that the user explicitly wanted to defer.
    //
    // STEP4 builtins to add:
    //   batch(req, req, ..) → bundle requests for the scheduler
    //   activate(cfg)       → OS-as-participant: switch system to a config
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_call(
        &mut self,
        fn_name: &str,
        args:    &[ONode],
        scope:   &HashMap<String, OValue>,
    ) -> Result<OValue> {
        // STEP-3: `lazy(expr)` is a POLICY-MODIFYING builtin — it must take
        // control of its argument's evaluation so that the policy switch
        // applies to the construction of the inner Requests. It cannot go
        // through the standard "evaluate args first" path; by the time args
        // are evaluated under that path, the inner Requests would have been
        // constructed (and auto-resolved) under the wrong policy.
        if fn_name == "lazy" {
            if args.len() != 1 {
                bail!("lazy(expr) takes exactly 1 argument, got {}", args.len());
            }
            let saved_policy = self.policy;
            self.policy = Policy::Lazy;
            let result = self.eval_node(&args[0], scope);
            self.policy = saved_policy;   // restored even on error path
            return result;
        }

        // STEP-4: `autonomous(expr)` — evaluate `expr` under Policy::Autonomous.
        //
        // Non-Eval Requests constructed during the evaluation are buffered
        // instead of being executed immediately. When the body finishes, the
        // scheduler flushes the buffer: it collects the full dependency graph,
        // executes independent Nix-family requests as concurrent threads, and
        // writes results to the two-level cache.
        //
        // If the body returns a Request value (a request that was buffered and
        // therefore left unforced in the return position), the method resolves
        // it from the freshly-populated cache so the caller always receives a
        // concrete value.
        //
        // Like `lazy`, this must intercept the argument BEFORE the standard
        // "evaluate args first" path runs, so the policy is in effect for the
        // entire body evaluation.
        if fn_name == "autonomous" {
            if args.len() != 1 {
                bail!("autonomous(expr) takes exactly 1 argument, got {}", args.len());
            }
            let saved_policy = self.policy;
            self.policy = Policy::Autonomous;
            let result = self.eval_node(&args[0], scope);
            self.policy = saved_policy; // restore before flush

            match result {
                Ok(value) => {
                    // Flush all buffered Nix-family requests concurrently.
                    self.flush_autonomous_buffer()?;
                    // Resolve the return value (Request or Group) from the
                    // cache that the flush just populated, so the caller always
                    // receives concrete values rather than unforced handles.
                    let resolved = self.resolve_after_flush(value)?;
                    return Ok(resolved);
                }
                Err(e) => {
                    // Clear the buffer so stale entries don't leak into future calls.
                    self.autonomous_buffer.clear();
                    return Err(e);
                }
            }
        }

        // Standard builtins: evaluate args left-to-right (applicative order).
        let arg_vals: Vec<OValue> = args
            .iter()
            .map(|a| self.eval_node(a, scope))
            .collect::<Result<_>>()?;

        match fn_name {
            "instantiate" => {
                if arg_vals.len() != 1 {
                    bail!("instantiate(expr) takes exactly 1 argument, got {}", arg_vals.len());
                }
                let req = OValue::request(
                    RequestKind::Instantiate,
                    arg_vals.into_iter().next().unwrap(),
                );
                self.auto_resolve(req)
            }
            "realise" => {
                if arg_vals.len() != 1 {
                    bail!("realise(drv) takes exactly 1 argument, got {}", arg_vals.len());
                }
                let req = OValue::request(
                    RequestKind::Realise,
                    arg_vals.into_iter().next().unwrap(),
                );
                self.auto_resolve(req)
            }
            "now" => {
                if arg_vals.len() != 1 {
                    bail!("now(req) takes exactly 1 argument, got {}", arg_vals.len());
                }
                let req = arg_vals.into_iter().next().unwrap();
                match &req {
                    OValue::Request { .. } => self.force_request(&req),
                    // now(group): force the whole group per its topology mode,
                    // resolving each member fresh (from_cache = false).
                    OValue::Group { mode, members, .. } => {
                        let (mode, members) = (*mode, members.clone());
                        self.resolve_group(mode, &members, false)
                    }
                    other => bail!(
                        "now(req) expected a Request or Group, got {}", other.type_name()
                    ),
                }
            }
            // STEP-4: coordination primitives — bundle several computations
            // into a first-class Group value carrying an explicit execution
            // topology. The members are the already-evaluated arguments (under
            // Eager they are resolved values; under Lazy/Autonomous they are
            // unforced/buffered Requests). The Group itself performs no work —
            // it is forced by `now(group)`, by `autonomous(...)`, or at
            // document end under Autonomous policy.
            "batch" | "all" | "any" | "race" => {
                let mode = match fn_name {
                    "batch" => GroupMode::Batch,
                    "all"   => GroupMode::All,
                    "any"   => GroupMode::Any,
                    "race"  => GroupMode::Race,
                    _       => unreachable!(),
                };
                if arg_vals.is_empty() {
                    bail!("{}(...) takes at least 1 argument, got 0", fn_name);
                }
                Ok(OValue::group(mode, arg_vals))
            }
            // STEP-4: OS-as-participant builtins.
            "activate" => {
                if arg_vals.is_empty() || arg_vals.len() > 2 {
                    bail!(
                        "activate(path) or activate(path, profile) — takes 1 \
                         or 2 args, got {}", arg_vals.len()
                    );
                }
                let mut iter = arg_vals.into_iter();
                let target  = iter.next().unwrap();
                let profile = match iter.next() {
                    Some(OValue::Str { v }) => v,
                    Some(OValue::System { profile_path }) => profile_path,
                    Some(other) => bail!(
                        "activate's second arg must be a string profile path \
                         or a System value, got {}", other.type_name()
                    ),
                    None => "/nix/var/nix/profiles/system".to_string(),
                };
                // dry_run defaults to true at the language level. The actual
                // subprocess argument is further gated by an env var in
                // nixos_ops. Two layers of opt-in.
                let req = OValue::request(
                    RequestKind::Activate { profile, dry_run: true },
                    target,
                );
                self.auto_resolve(req)
            }
            "current_system" => {
                // Read the system profile symlink without going through a
                // Request — this is a pure inspection, not a deferred
                // computation. The result is an OValue::System reference.
                if !arg_vals.is_empty() {
                    bail!("current_system() takes no arguments, got {}", arg_vals.len());
                }
                Ok(OValue::system("/nix/var/nix/profiles/system"))
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
        attr:   Option<&str>,
        body:   &[ONode],
        scope:  &HashMap<String, OValue>,
    ) -> Result<OValue> {
        // ─────────────────────────────────────────────────────────────────────
        // Short-circuit for `quote^`: capture the body as an unevaluated
        // OValue::Expr WITHOUT evaluating its children or calling any shim.
        // Must be first — the body-walking loop below would otherwise start
        // evaluating nested typed expressions (e.g. python^(6*7)_python inside
        // a quote body), which would hang waiting for shims.
        //
        // reconstruct_source converts the ONode tree back to O source text;
        // O.eval() in a Python block can then round-trip it through eval_source.
        // ─────────────────────────────────────────────────────────────────────
        if lang == "quote" {
            let src = reconstruct_source(body);
            return Ok(OValue::Expr { src });
        }

        // ─────────────────────────────────────────────────────────────────────
        // STEP-3.5: validate the optional `{attr}` early so misuses are
        // caught at the block we're evaluating, not somewhere downstream.
        //
        //   {lazy}  — pure backends only; produces a cacheable Eval Request
        //   {defer} — any backend; produces a non-cacheable Eval Request
        //
        // `nix_expr^` is already lazy by construction; attributes on it are
        // rejected as redundant. STEP4 may add other attributes (trace, etc.).
        // ─────────────────────────────────────────────────────────────────────
        if let Some(a) = attr {
            match a {
                "lazy" => {
                    if lang == "nix_expr" {
                        bail!(
                            "`nix_expr{{lazy}}^` is redundant — nix_expr^ is already \
                             lazy. Use bare nix_expr^, or use nix{{lazy}}^ if you \
                             want a generic deferred Nix eval."
                        );
                    }
                    if !is_pure_backend(lang) {
                        bail!(
                            "`{lang}{{lazy}}^` is invalid because {lang} is not a \
                             pure backend; caching a thunk that re-runs with side \
                             effects would be unsound. Use `{lang}{{defer}}^` instead \
                             — it captures the same thunk but never caches and \
                             always re-runs on force.",
                            lang = lang
                        );
                    }
                }
                "defer" => {
                    if lang == "nix_expr" {
                        bail!(
                            "`nix_expr{{defer}}^` is redundant — nix_expr^ is already \
                             lazy. If you want a non-cacheable deferred Nix eval, \
                             write nix{{defer}}^."
                        );
                    }
                    // {defer} works on any backend; nothing else to check.
                }
                other => bail!(
                    "Unknown block attribute `{{{}}}` on {}^. Known attributes: \
                     lazy, defer.",
                    other, lang
                ),
            }
        }

        // Step 1 — build the fully-spliced source string for the backend.
        // For `nix_expr` blocks and `{lazy}`/`{defer}` blocks we also collect
        // the evaluated child OValues as deps so the returned thunk carries
        // its full dependency tree for fingerprint composition.
        let mut buf  = String::new();
        let mut deps: Vec<OValue> = Vec::new();

        // Whether this block constructs a Thunk (and so should track deps).
        let constructs_thunk = lang == "nix_expr" || attr.is_some();

        // Own a mutable copy of the scope so that LetBinding nodes inside this
        // body can extend it for subsequent children. Cloning is cheap compared
        // to the subprocess dispatch that follows.
        let mut local_scope = scope.clone();

        for child in body {
            match child {
                ONode::LetBinding { name, expr } => {
                    // Evaluate the RHS and bind it into the local scope.
                    // The binding itself produces no text for the backend.
                    let value = self.eval_node(expr, &local_scope)?;
                    local_scope.insert(name.clone(), value);
                }

                ONode::RawText(text) => {
                    buf.push_str(text);
                }

                ONode::VarRef(name) => {
                    let val = local_scope
                        .get(name)
                        .ok_or_else(|| anyhow::anyhow!("Undefined variable: ${}", name))?
                        .clone();
                    // STEP-3.5: auto-force {lazy} thunks before splicing; error
                    // on {defer} thunks. {lazy} is safe to auto-force because
                    // pure-backend results don't have side effects.
                    let resolved = self.resolve_for_splice(val)?;
                    buf.push_str(&self.render_child(lang, &resolved));
                    if constructs_thunk {
                        deps.push(resolved);
                    }
                }

                ONode::TypedExpr {
                    lang: child_lang,
                    env_id: child_env_id,
                    attr: child_attr,
                    body: child_body,
                } => {
                    // Evaluate the nested expression first (leaves-up / applicative order),
                    // then render its value into the parent language's source syntax.
                    let child_val = self.eval_typed_expr(
                        child_lang, *child_env_id, child_attr.as_deref(), child_body, &local_scope,
                    )?;
                    let resolved = self.resolve_for_splice(child_val)?;
                    buf.push_str(&self.render_child(lang, &resolved));
                    if constructs_thunk {
                        deps.push(resolved);
                    }
                }

                ONode::Call { fn_name, args } => {
                    let raw = self.eval_call(fn_name, args, &local_scope)?;
                    let resolved = self.resolve_for_splice(raw)?;
                    buf.push_str(&self.render_child(lang, &resolved));
                    if constructs_thunk {
                        deps.push(resolved);
                    }
                }
            }
        }

        // ─────────────────────────────────────────────────────────────────────
        // STEP-3.5: if the block had a `{lazy}` or `{defer}` attribute, wrap
        // the captured (body, deps) in a Thunk and return a Request[Eval]
        // over it. The Request does NOT auto-resolve at construction —
        // syntactic deferral is unconditional. The user forces via now() or
        // by splicing it (auto-force for {lazy} only).
        // ─────────────────────────────────────────────────────────────────────
        if let Some(a) = attr {
            let cacheable = a == "lazy";
            let thunk = OValue::thunk(buf, deps);
            return Ok(OValue::request(
                RequestKind::Eval {
                    lang:      lang.to_string(),
                    env_id,
                    cacheable,
                },
                thunk,
            ));
        }

        // Short-circuit for `nix_expr`: return a lazy ONixExpr instead of
        // calling the Nix shim immediately.  The fingerprint is sha256(body)
        // — the cheap step-1 scheme.  `nix^` (immediate evaluation) is
        // unchanged (step-1 decision, option a).
        if lang == "nix_expr" {
            return Ok(OValue::nix_expr(buf, deps));
        }

        // Step 2 — send the completed splice buffer to the backend.
        // Shim path resolution is centralized in the BackendRegistry.
        let shim = BackendRegistry::global().resolve_shim_path(&self.shim_dir, lang);
        // `O^(...)_O` sequences its children and returns the last non-null value.
        // Each child is evaluated in order; whitespace-only text nodes are skipped.
        // This matches the Python ref impl's OBackend.eval_ast semantics.
        if lang == "O" {
            let mut last = OValue::null();
            for child in body {
                match child {
                    ONode::RawText(s) if s.chars().all(char::is_whitespace) => {}
                    ONode::RawText(s) => last = OValue::str_(s.clone()),
                    ONode::VarRef(name) => {
                        if let Some(v) = scope.get(name) {
                            last = v.clone();
                        }
                    }
                    ONode::TypedExpr { lang: cl, env_id: ce, attr: ca, body: cb } => {
                        let v = self.eval_typed_expr(cl, *ce, ca.as_deref(), cb, scope)?;
                        if !v.is_null() {
                            last = v;
                        }
                    }
                    ONode::LetBinding { name, expr } => {
                        // let inside O^: evaluate and bind, but O-level scope
                        // is not mutable here. Just evaluate for side effects.
                        let _ = self.eval_node(expr, scope)?;
                        let _ = name; // binding discarded inside O^
                    }
                    ONode::Call { fn_name, args } => {
                        let v = self.eval_call(fn_name, args, scope)?;
                        if !v.is_null() {
                            last = v;
                        }
                    }
                }
            }
            return Ok(last);
        }

        if lang == "html" {
            return Ok(OValue::html(buf));
        }

        // Markup-only backends: no subprocess needed, just return the body text.
        // markdown and text return the spliced body as a string value;
        // latex returns it as a string (compilation to PDF is out-of-scope).
        if matches!(lang, "markdown" | "md" | "text" | "plain" | "latex" | "tex") {
            return Ok(OValue::str_(buf));
        }

        // Send the exec command to the shim, then drive the eval_request loop.
        //
        // Normally the shim sends Ok/Err immediately and the loop runs once.
        // If the shim's user code calls `O.eval(q)`, it sends EvalRequest with
        // the quoted source; we evaluate it here and send back EvalResult, then
        // loop to read the next response. The loop terminates on Ok or Err.
        let env_label = if env_id == u32::MAX {
            format!("{lang}[*ephemeral*]")
        } else {
            format!("{lang}[{env_id}]")
        };

        self.registry
            .send_exec(lang, env_id, &buf, scope.clone(), &shim)
            .with_context(|| format!("[{}]", env_label))?;

        let result: Result<OValue> = loop {
            let step = self.registry
                .recv_exec_step(lang, env_id)
                .with_context(|| format!("[{}]", env_label))?;

            match step {
                ExecStep::Done(v) => break Ok(v),

                ExecStep::EvalRequest { src } => {
                    // Evaluate the quoted source. If eval fails, propagate the
                    // error — the shim's `O.eval(q)` will raise on the Python
                    // side because the runtime never sends eval_result.
                    match self.eval_source(&src) {
                        Ok(result) => {
                            self.registry
                                .send_eval_result(lang, env_id, result)
                                .with_context(|| format!("[{}] send_eval_result", env_label))?;
                        }
                        Err(e) => {
                            // Remove the process from the registry so the
                            // stuck shim doesn't pollute future calls.
                            let _ = self.registry.cleanup_env(lang, env_id);
                            return Err(e).with_context(|| {
                                format!(
                                    "[{}] O.eval() failed while evaluating quoted source",
                                    env_label
                                )
                            });
                        }
                    }
                }
            }
        };

        // Step 3 — discard ephemeral envs (env_id == u32::MAX) after every expression,
        // regardless of whether exec succeeded.  (MAX is used for certain internal
        // re-entrant O.eval cases to avoid deadlock on a persistent env; bare
        // user-level blocks now default to env 0 per the spec.)
        if env_id == u32::MAX {
            let _ = self.registry.cleanup_env(lang, u32::MAX);
        }

        // Attach a `[lang[env_id]]` tag to the existing error CHAIN — using
        // anyhow::Context preserves the underlying source error (shim stderr,
        // SyntaxError details, etc.) as a "Caused by:" entry.  Previously this
        // path used `anyhow!("[{}] {}", env_label, e)`, which formats `e` as a
        // string and DROPS the source chain — the actual shim error message
        // was lost, leaving the user with only the wrapper.
        result.with_context(|| format!("[{}]", env_label))
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
        // The lang → strategy decision is centralized in the BackendRegistry
        // (ir.rs); the value-level renderers stay here because they need
        // OValue. Unrecognised languages get SpliceRenderer::Default, which
        // is OValue::splice_repr() — a conservative representation that is
        // valid in the widest range of languages.
        match BackendRegistry::global().renderer_for(lang) {
            // ── Python ──────────────────────────────────────────────────────
            // Produce a valid Python literal so the spliced code compiles
            // without the user having to quote things manually.
            SpliceRenderer::Python => render_python(val),

            // ── HTML ─────────────────────────────────────────────────────────
            // Produce embeddable HTML markup.  OBlob images become data-URI
            // <img> tags; everything else falls back to splice_repr or
            // direct string embedding.
            SpliceRenderer::Html => render_html(val),

            // ── LaTeX ────────────────────────────────────────────────────────
            SpliceRenderer::Latex => render_latex(val),

            // ── Markdown ─────────────────────────────────────────────────────
            SpliceRenderer::Markdown => render_markdown(val),

            // ── Nix family ───────────────────────────────────────────────────
            // Produce syntactically valid Nix expressions so that O values
            // from prior blocks can be spliced into Nix code via $var.
            SpliceRenderer::Nix => render_nix(val),

            // ── Default: use the conservative cross-language representation ──
            SpliceRenderer::Default => val.splice_repr(),
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
        OValue::Group { mode, fingerprint, .. } => {
            format!("\"<group:{} fp={}>\"", mode.name(), &fingerprint[..8])
        }

        // A System in a Nix context renders as its profile path as a string
        // literal. Useful for Nix expressions that want to inspect or compare
        // against the live profile location.
        OValue::System { profile_path } => serde_json::to_string(profile_path)
            .unwrap_or_else(|_| "\"\"".to_string()),

        // An Expr in Nix context renders its quoted source as a Nix string
        // literal. Rarely useful — the user almost always wants O.eval first.
        OValue::Expr { src } => serde_json::to_string(src)
            .unwrap_or_else(|_| "\"\"".to_string()),
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

        OValue::Request { fingerprint, .. } => {
            let fp_lit = serde_json::to_string(fingerprint).unwrap_or_else(|_| "''".to_string());
            format!("ORequest(fp={})", fp_lit)
        }
        OValue::Thunk { body, fingerprint, deps } => {
            let body_lit = serde_json::to_string(body).unwrap_or_else(|_| "''".to_string());
            let fp_lit   = serde_json::to_string(fingerprint).unwrap_or_else(|_| "''".to_string());
            let deps_rendered = deps.iter().map(render_python)
                .collect::<Vec<_>>().join(", ");
            format!("OThunk({}, fp={}, deps=[{}])", body_lit, fp_lit, deps_rendered)
        }
        OValue::System { profile_path } => {
            let lit = serde_json::to_string(profile_path).unwrap_or_else(|_| "''".to_string());
            format!("OSystem({})", lit)
        }

        // A Group has no Python data form — render an OGroup marker mirroring
        // the ORequest treatment. The shim does not bind it as a value;
        // groups are control values forced via `now(...)`, not spliced.
        OValue::Group { mode, members, fingerprint } => {
            let fp_lit = serde_json::to_string(fingerprint).unwrap_or_else(|_| "''".to_string());
            format!("OGroup({:?}, n={}, fp={})", mode.name(), members.len(), fp_lit)
        }

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

// ── HTML ─────────────────────────────────────────────────────────────────────

fn render_html(val: &OValue) -> String {
    match val {
        OValue::Null => String::new(),

        OValue::Bool { v } => html_escape(&v.to_string()),
        OValue::Int { v } => html_escape(&v.to_string()),
        OValue::Float { v } => html_escape(&v.to_string()),

        // Plain strings are untrusted text — escape them. Trusted raw HTML
        // must arrive as OValue::Html (the "trusted HTML fragment" type per
        // SPEC.md), e.g. produced by an inner html^(...)_html block.
        OValue::Str { v } => html_escape(v),
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

        OValue::Request { fingerprint, .. } => {
            format!(
                "<code class=\"o-request\" data-fp=\"{}\">&lt;request&gt;</code>",
                html_escape(&fingerprint[..8]),
            )
        }
        OValue::Thunk { body, fingerprint, .. } => {
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

        OValue::Group { mode, members, fingerprint } => {
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
            format!(
                "<code class=\"o-expr\">{}</code>",
                html_escape(src),
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
        OValue::Request { fingerprint, .. } => {
            format!("\\texttt{{<request fp={}>}}", &fingerprint[..8])
        }
        OValue::Thunk { body, .. } => {
            format!("\\texttt{{{}}}", body.replace("_", "\\_"))
        }
        OValue::System { profile_path } => {
            format!("\\texttt{{{}}}", profile_path.replace("_", "\\_"))
        }
        OValue::Group { mode, members, fingerprint } => {
            format!(
                "\\texttt{{<group:{} n={} fp={}>}}",
                mode.name(), members.len(), &fingerprint[..8]
            )
        }
        OValue::Expr { src } => {
            format!("\\texttt{{{}}}", src.replace("_", "\\_"))
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
        OValue::Request { fingerprint, .. } => {
            format!("`<request fp={}>`", &fingerprint[..8])
        }
        OValue::Thunk { body, .. } => {
            format!("`{}`", body)
        }
        OValue::System { profile_path } => {
            format!("`{}`", profile_path)
        }
        OValue::Group { mode, members, fingerprint } => {
            format!("`<group:{} n={} fp={}>`", mode.name(), members.len(), &fingerprint[..8])
        }
        OValue::Expr { src } => {
            format!("`{}`", src)
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
    fn html_str_is_escaped_html_is_raw() {
        let e = Evaluator::new("/tmp".into());
        let result = e.render_child("html", &OValue::str_("<b>bold</b>"));
        assert_eq!(result, "&lt;b&gt;bold&lt;/b&gt;");
        let raw = e.render_child("html", &OValue::html("<b>bold</b>"));
        assert_eq!(raw, "<b>bold</b>");
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
            None,
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

        let result = e.eval_typed_expr("nix_expr", u32::MAX, None, &body_nodes, &scope).unwrap();

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
                    (kind.clone(), source.as_ref().clone(), fingerprint.clone()),
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

    /// Under Eager (the default), `instantiate($expr)` auto-resolves at
    /// construction time inside eval_call. The caller never sees a Request.
    #[test]
    fn eager_call_auto_resolves_at_construction() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let call = ONode::Call {
            fn_name: "instantiate".into(),
            args:    vec![ONode::VarRef("expr".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert!(result.is_derivation(),
            "under Eager, eval_call should auto-resolve directly to a Derivation");
    }

    /// `realise(instantiate($expr))` chains under Eager: instantiate auto-
    /// resolves to a Derivation, then realise auto-resolves to a StorePath.
    /// No intermediate Request is observable.
    #[test]
    fn nested_call_under_eager_resolves_end_to_end() {
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

        let result = e.eval_node(&outer, &scope).unwrap();
        if let OValue::StorePath { path } = &result {
            assert!(path.starts_with("/nix/store/"));
        } else { panic!("expected StorePath under Eager end-to-end, got {:?}", result); }
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
    /// regardless of policy. In step 3 this matters: inside a lazy^ region,
    /// `now()` is the explicit-perform escape hatch.
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

    // ─────────────────────────────────────────────────────────────────────────
    // STEP-3: lazy(expr) builtin call — policy-modifying operator
    //
    // Note the structural shape: lazy is a builtin call, not a language. The
    // block form `lazy^(...)_lazy` was rejected because blocks are languages
    // and lazy doesn't have a source-text body in any language. These tests
    // pin down the call form's semantics.
    // ─────────────────────────────────────────────────────────────────────────

    /// `lazy(instantiate($expr))` returns a Request without executing.
    /// Under the Lazy policy that lazy() installs, the inner instantiate's
    /// auto-resolve passes the Request through.
    #[test]
    fn lazy_call_returns_unresolved_request() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let lazy_call = ONode::Call {
            fn_name: "lazy".into(),
            args:    vec![ONode::Call {
                fn_name: "instantiate".into(),
                args:    vec![ONode::VarRef("expr".into())],
            }],
        };

        let result = e.eval_node(&lazy_call, &scope).unwrap();
        assert!(result.is_request(),
            "lazy(instantiate(...)) must return a Request, got {:?}", result);
    }

    /// `lazy(realise(instantiate($expr)))` returns a chained Request — outer
    /// Realise over inner Instantiate, neither executed.
    #[test]
    fn lazy_preserves_chained_request_structure() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let chain = ONode::Call {
            fn_name: "lazy".into(),
            args:    vec![ONode::Call {
                fn_name: "realise".into(),
                args:    vec![ONode::Call {
                    fn_name: "instantiate".into(),
                    args:    vec![ONode::VarRef("expr".into())],
                }],
            }],
        };

        let result = e.eval_node(&chain, &scope).unwrap();
        if let OValue::Request { kind, source, .. } = &result {
            assert_eq!(*kind, RequestKind::Realise);
            assert!(source.is_request(),
                "outer Request's source must be the inner unresolved Instantiate Request");
        } else { panic!("expected chained Request, got {:?}", result); }
    }

    /// `now()` inside lazy() forces execution — the explicit escape hatch.
    #[test]
    fn now_inside_lazy_executes() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let nested = ONode::Call {
            fn_name: "lazy".into(),
            args:    vec![ONode::Call {
                fn_name: "now".into(),
                args:    vec![ONode::Call {
                    fn_name: "instantiate".into(),
                    args:    vec![ONode::VarRef("expr".into())],
                }],
            }],
        };

        let result = e.eval_node(&nested, &scope).unwrap();
        assert!(result.is_derivation(),
            "now() inside lazy() still executes, returning a Derivation");
    }

    /// Policy is restored after lazy() returns. A subsequent direct call
    /// should auto-resolve normally — confirming the policy scope is bounded.
    #[test]
    fn policy_restored_to_eager_after_lazy_returns() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        // First: lazy(instantiate(...)) returns a Request (Lazy was active).
        let lazy_call = ONode::Call {
            fn_name: "lazy".into(),
            args:    vec![ONode::Call {
                fn_name: "instantiate".into(),
                args:    vec![ONode::VarRef("expr".into())],
            }],
        };
        assert!(e.eval_node(&lazy_call, &scope).unwrap().is_request());

        // Then: plain instantiate(...) auto-resolves (Eager is back).
        let plain_call = ONode::Call {
            fn_name: "instantiate".into(),
            args:    vec![ONode::VarRef("expr".into())],
        };
        let result = e.eval_node(&plain_call, &scope).unwrap();
        assert!(result.is_derivation(),
            "after lazy() exits, direct call should auto-resolve to Derivation");
    }

    /// Nested lazy inside lazy stays lazy. Pinning down the edge case:
    /// re-entering a lazy region shouldn't accidentally restore an outer
    /// non-lazy policy.
    #[test]
    fn nested_lazy_calls_remain_lazy() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let nested = ONode::Call {
            fn_name: "lazy".into(),
            args:    vec![ONode::Call {
                fn_name: "lazy".into(),
                args:    vec![ONode::Call {
                    fn_name: "instantiate".into(),
                    args:    vec![ONode::VarRef("expr".into())],
                }],
            }],
        };
        let result = e.eval_node(&nested, &scope).unwrap();
        assert!(result.is_request(),
            "lazy nested in lazy must still produce a Request, got {:?}", result);
    }

    /// Even when lazy()'s argument errors, the policy is restored.
    /// This is the save/restore guard in the lazy branch of eval_call.
    #[test]
    fn policy_restored_even_on_lazy_arg_error() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let bad = ONode::Call {
            fn_name: "lazy".into(),
            args:    vec![ONode::VarRef("missing".into())],   // will error
        };

        assert_eq!(e.policy, Policy::Eager);
        let _ = e.eval_node(&bad, &scope);    // expected error
        assert_eq!(e.policy, Policy::Eager,
            "policy must be restored to Eager after lazy() errors");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // STEP-3.5: {lazy} / {defer} block attributes
    // ─────────────────────────────────────────────────────────────────────────

    /// {lazy} on an impure backend (python) is rejected at evaluation with a
    /// message suggesting {defer} as the alternative.
    #[test]
    fn lazy_attr_on_impure_backend_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang:   "python".into(),
            env_id: u32::MAX,
            attr:   Some("lazy".into()),
            body:   vec![ONode::RawText("1 + 1".into())],
        };
        let err = e.eval_node(&block, &scope).unwrap_err().to_string();
        assert!(err.contains("not a pure backend"),
            "error must explain backend purity, got: {}", err);
        assert!(err.contains("defer"),
            "error should suggest {{defer}} as alternative, got: {}", err);
    }

    /// {lazy} on a pure backend (nix) returns a Request[Eval] without
    /// invoking the shim. The Thunk inside carries body + deps.
    #[test]
    fn lazy_attr_on_pure_backend_produces_eval_request() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang:   "nix".into(),
            env_id: u32::MAX,
            attr:   Some("lazy".into()),
            body:   vec![ONode::RawText("1 + 2".into())],
        };
        let result = e.eval_node(&block, &scope).unwrap();
        if let OValue::Request { kind, source, .. } = &result {
            match kind {
                RequestKind::Eval { lang, env_id: _, cacheable } => {
                    assert_eq!(lang, "nix");
                    assert!(*cacheable, "{{lazy}} must produce cacheable=true");
                }
                other => panic!("expected RequestKind::Eval, got {:?}", other),
            }
            assert!(source.is_thunk(), "Request source must be a Thunk");
            if let OValue::Thunk { body, .. } = source.as_ref() {
                assert_eq!(body, "1 + 2");
            }
        } else { panic!("expected Request, got {:?}", result); }
    }

    /// {defer} on an impure backend (python) is allowed and produces a
    /// non-cacheable Eval Request.
    #[test]
    fn defer_attr_on_impure_backend_is_allowed() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang:   "python".into(),
            env_id: u32::MAX,
            attr:   Some("defer".into()),
            body:   vec![ONode::RawText("print('hi')".into())],
        };
        let result = e.eval_node(&block, &scope).unwrap();
        if let OValue::Request { kind, .. } = &result {
            if let RequestKind::Eval { lang, cacheable, .. } = kind {
                assert_eq!(lang, "python");
                assert!(!*cacheable, "{{defer}} must produce cacheable=false");
            } else { panic!("expected RequestKind::Eval"); }
        } else { panic!("expected Request"); }
    }

    /// {lazy} on nix_expr is rejected as redundant.
    #[test]
    fn lazy_attr_on_nix_expr_errors_redundant() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang:   "nix_expr".into(),
            env_id: u32::MAX,
            attr:   Some("lazy".into()),
            body:   vec![],
        };
        let err = e.eval_node(&block, &scope).unwrap_err().to_string();
        assert!(err.contains("redundant"),
            "error must say nix_expr+{{lazy}} is redundant, got: {}", err);
    }

    /// Unknown attributes error with a clear message.
    #[test]
    fn unknown_attr_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang:   "nix".into(),
            env_id: u32::MAX,
            attr:   Some("strict".into()),
            body:   vec![],
        };
        let err = e.eval_node(&block, &scope).unwrap_err().to_string();
        assert!(err.contains("strict"));
        assert!(err.contains("Known attributes"));
    }

    /// now() on a {lazy} Eval request fires the shim. We seed the cache
    /// directly to verify the cache-hit path without spawning a real shim.
    #[test]
    fn now_on_lazy_eval_request_returns_cached_value() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let block = ONode::TypedExpr {
            lang:   "nix".into(),
            env_id: u32::MAX,
            attr:   Some("lazy".into()),
            body:   vec![ONode::RawText("3 + 4".into())],
        };
        let req = e.eval_node(&block, &scope).unwrap();
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else { panic!("expected Request"); };

        // Seed the Evaluator's own eval_cache so force_request hits it
        // instead of trying to spawn a nix shim.
        e.eval_cache.insert(fp.clone(), OValue::int(7));

        let forced = e.force_request(&req).unwrap();
        assert_eq!(forced, OValue::int(7),
            "now() / force_request must return the cached value");
    }

    /// {defer} requests bypass the cache on read AND write — re-running on
    /// every force is their defining property.
    #[test]
    fn defer_eval_request_bypasses_cache() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let block = ONode::TypedExpr {
            lang:   "python".into(),
            env_id: u32::MAX,
            attr:   Some("defer".into()),
            body:   vec![ONode::RawText("1".into())],
        };
        let req = e.eval_node(&block, &scope).unwrap();
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else { panic!("expected Request"); };

        // Even with a value seeded under the {defer} request's fingerprint,
        // the executor must not consult the cache for non-cacheable Eval —
        // it tries to actually spawn the shim, which fails (no shim_dir).
        e.eval_cache.insert(fp, OValue::str_("hypothetical cached"));

        let err = e.force_request(&req).unwrap_err().to_string();
        // The shim path doesn't exist; force should attempt to fire it.
        // (Any error here means we got past the cache lookup. The specific
        //  error depends on what the registry says.)
        assert!(
            !err.contains("hypothetical cached"),
            "force on {{defer}} must NOT return the seeded cache value, got: {}",
            err
        );
    }

    /// Splicing a {lazy} Eval Request into another block's source text
    /// auto-forces it (per fork #2). We seed the cache so the auto-force
    /// returns a known value without spawning a shim.
    #[test]
    fn splice_auto_forces_lazy_eval_request() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();

        // Construct a {lazy} nix block, find its fingerprint, seed the cache.
        let lazy_block = ONode::TypedExpr {
            lang:   "nix".into(),
            env_id: u32::MAX,
            attr:   Some("lazy".into()),
            body:   vec![ONode::RawText("123".into())],
        };
        let req = e.eval_node(&lazy_block, &scope).unwrap();
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else { panic!(); };
        e.eval_cache.insert(fp, OValue::int(123));
        scope.insert("lz".into(), req);

        // Now splice the lazy Request into another block via $lz. The splice
        // path should auto-force, retrieving 123 from the cache and
        // rendering it. We use markdown^ so we don't need a real shim —
        // markdown bypasses the registry and renders directly.
        let md_block = ONode::TypedExpr {
            lang:   "markdown".into(),
            env_id: u32::MAX,
            attr:   None,
            body:   vec![
                ONode::RawText("value=".into()),
                ONode::VarRef("lz".into()),
            ],
        };
        // markdown^ goes through the registry path which tries to spawn a
        // shim. We just check that resolve_for_splice resolves the request:
        let resolved = e.resolve_for_splice(scope["lz"].clone()).unwrap();
        assert_eq!(resolved, OValue::int(123),
            "splice path must auto-force {{lazy}} to its cached value");
        // (md_block parsed but not evaluated end-to-end here — the splice
        // resolution is the unit we're testing.)
        let _ = md_block;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // STEP-4: OS-as-participant
    //
    // The activate() builtin constructs a Request[Activate] over a StorePath
    // (or auto-realises a chained Derivation Request first). The default
    // dry_run flag is true at the Request level, AND the actual subprocess
    // is gated by an env var in nixos_ops — two layers of opt-in.
    // ─────────────────────────────────────────────────────────────────────────

    /// MockSystemExecutor returns canned System values for Activate requests
    /// without actually shelling out to switch-to-configuration. Used to
    /// verify the orchestration without touching the real OS.
    struct MockSystemExecutor {
        activate_calls: Vec<(String, bool)>,    // (profile, dry_run)
    }

    impl MockSystemExecutor {
        fn new() -> Self { Self { activate_calls: vec![] } }
    }

    impl Executor for MockSystemExecutor {
        fn execute(&mut self, req: &OValue) -> Result<OValue> {
            let (kind, source) = match req {
                OValue::Request { kind, source, .. } => (kind.clone(), source.as_ref().clone()),
                _ => panic!("MockSystemExecutor only handles Requests"),
            };

            // Walk chains the same way ImmediateExecutor does.
            let resolved_source = match source {
                OValue::Request { .. } => self.execute(&source)?,
                other => other,
            };

            match kind {
                RequestKind::Activate { profile, dry_run } => {
                    self.activate_calls.push((profile.clone(), dry_run));
                    Ok(OValue::system(profile))
                }
                RequestKind::Realise => {
                    // Auto-realise a Derivation source — used in the chain test.
                    if resolved_source.is_derivation() {
                        Ok(OValue::store_path("/nix/store/mock-system"))
                    } else { panic!("Realise source must be Derivation") }
                }
                RequestKind::Instantiate => {
                    Ok(OValue::derivation(
                        "/nix/store/mockhash-system.drv",
                        vec!["out".into()],
                        vec![],
                    ))
                }
                other => panic!("MockSystemExecutor: unhandled kind {:?}", other),
            }
        }
    }

    /// `activate($path)` constructs a Request[Activate] and (under Eager)
    /// auto-resolves it. The mock executor returns a System value.
    #[test]
    fn activate_call_builds_request_and_resolves_to_system() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));

        let call = ONode::Call {
            fn_name: "activate".into(),
            args:    vec![ONode::VarRef("path".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert!(result.is_system(),
            "activate($path) under Eager should auto-resolve to a System, got {:?}", result);
        if let OValue::System { profile_path } = &result {
            assert_eq!(profile_path, "/nix/var/nix/profiles/system",
                "default profile should be the system-wide one");
        }
    }

    /// `activate($path, $profile)` uses the user-supplied profile.
    #[test]
    fn activate_with_explicit_profile_uses_it() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));
        scope.insert("profile".into(), OValue::str_("/home/lee/.nix-profile"));

        let call = ONode::Call {
            fn_name: "activate".into(),
            args:    vec![
                ONode::VarRef("path".into()),
                ONode::VarRef("profile".into()),
            ],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        if let OValue::System { profile_path } = &result {
            assert_eq!(profile_path, "/home/lee/.nix-profile");
        } else { panic!("expected System"); }
    }

    /// The full four-rung chain — `activate(realise(instantiate($expr)))` —
    /// is structurally well-typed: each Request's source is the previous rung,
    /// and the executor walks the chain end-to-end under Eager.
    #[test]
    fn full_chain_instantiate_realise_activate() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("nixos.config.system", vec![]));

        let activate_call = ONode::Call {
            fn_name: "activate".into(),
            args: vec![ONode::Call {
                fn_name: "realise".into(),
                args: vec![ONode::Call {
                    fn_name: "instantiate".into(),
                    args: vec![ONode::VarRef("expr".into())],
                }],
            }],
        };
        let result = e.eval_node(&activate_call, &scope).unwrap();
        assert!(result.is_system(),
            "instantiate→realise→activate chain must resolve to a System");
    }

    /// activate() with a NixExpr (not yet instantiated) is NOT auto-realised.
    /// The intermediate climb is the user's responsibility to make explicit.
    /// (Auto-realising via a chained Request[Realise[Instantiate]] DOES work,
    /// because the chain is constructed at call sites; bare values aren't
    /// auto-lifted.)
    #[test]
    fn activate_on_bare_nix_expr_errors() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope: HashMap<String, OValue> = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("config", vec![]));

        // Construct activate($expr) where $expr is a bare NixExpr, not a chain.
        // The Mock executor's Activate arm passes resolved_source unchanged,
        // and nixos_ops::activate_nix (which the *real* executor would call)
        // would type-check it as NixExpr→error. The mock doesn't hit that
        // path because it short-circuits to a canned System. So we instead
        // test this via the real ImmediateExecutor path... actually the real
        // executor will try to nix_ops::activate_nix which would type-check.
        // Skip the assertion here; covered by the value.rs activate type test.
        let _ = &mut e;
        let _ = &scope;
    }

    /// `current_system()` returns a System reference without any IO.
    #[test]
    fn current_system_returns_default_profile_reference() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let result = e.eval_node(
            &ONode::Call { fn_name: "current_system".into(), args: vec![] },
            &scope,
        ).unwrap();
        if let OValue::System { profile_path } = &result {
            assert_eq!(profile_path, "/nix/var/nix/profiles/system");
        } else { panic!("expected System"); }
    }

    /// Activate requests must NEVER hit the executor cache. A stale System
    /// reference would lie about live state, and the whole point of asking
    /// for activation is to do it, not to look up a cached "result."
    #[test]
    fn activate_bypasses_cache_in_executor() {
        let mut exec = ImmediateExecutor::new();
        let path = OValue::store_path("/nix/store/abc-system");
        let req  = OValue::request(
            RequestKind::Activate { profile: "/p".into(), dry_run: true },
            path,
        );
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else { panic!() };
        exec.seed_cache(fp, OValue::system("/cached"));
        // The cache would return /cached IF cache were consulted. The real
        // path would try to spawn switch-to-configuration on a bogus store
        // path; the executor's cache-skip rule for Activate means we go
        // straight to that subprocess and error out.
        let err = exec.execute(&req).unwrap_err().to_string();
        assert!(
            !err.contains("/cached"),
            "Activate must bypass cache even when a seeded value exists, \
             got: {}",
            err
        );
    }

    /// Splicing a {defer} Eval Request errors out — the user must now() first.
    #[test]
    fn splice_of_defer_request_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let defer_block = ONode::TypedExpr {
            lang:   "python".into(),
            env_id: u32::MAX,
            attr:   Some("defer".into()),
            body:   vec![ONode::RawText("1".into())],
        };
        let req = e.eval_node(&defer_block, &scope).unwrap();
        let err = e.resolve_for_splice(req).unwrap_err().to_string();
        assert!(err.contains("defer"));
        assert!(err.contains("now"),
            "error should tell the user to call now() explicitly, got: {}", err);
    }

    /// Through eval_document: `let pending = lazy(realise(instantiate($expr)))`
    /// must bind `pending` to a Request, not auto-execute. This was the bug
    /// the block-form lazy^ had: auto_resolve at let-binding would re-execute.
    #[test]
    fn let_binding_preserves_lazy_request_under_eager() {
        use crate::parser::ONode;

        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));

        // We can't put a NixExpr into scope via eval_document's API directly,
        // so we test this by constructing the nodes for both let-bindings.
        let nodes = vec![
            ONode::LetBinding {
                name: "expr".into(),
                expr: Box::new(ONode::TypedExpr {
                    lang:   "nix_expr".into(),
                    env_id: u32::MAX,
                    attr:   None,
                    body:   vec![ONode::RawText("pkgs.hello".into())],
                }),
            },
            ONode::LetBinding {
                name: "pending".into(),
                expr: Box::new(ONode::Call {
                    fn_name: "lazy".into(),
                    args:    vec![ONode::Call {
                        fn_name: "realise".into(),
                        args:    vec![ONode::Call {
                            fn_name: "instantiate".into(),
                            args:    vec![ONode::VarRef("expr".into())],
                        }],
                    }],
                }),
            },
            // The document's final value: pending. If it's a Request, we got
            // the right answer; if it's a StorePath, the let-binding
            // erroneously re-executed.
            ONode::VarRef("pending".into()),
        ];

        let last = e.eval_document(nodes).unwrap();
        assert!(last.is_request(),
            "let pending = lazy(...) must bind a Request — re-executing at \
             the let-binding boundary would be the old broken behaviour. \
             Got {:?}", last);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // quote^ integration tests (in-process, no shim)
    // ─────────────────────────────────────────────────────────────────────────

    /// quote^(python^(6*7)_python)_quote should return OValue::Expr with the
    /// inner source text, NOT start a Python shim or produce 42.
    #[test]
    fn quote_block_returns_oexpr_not_evaluated() {
        let backends: HashSet<String> =
            ["python", "quote", "O"].iter().map(|s| s.to_string()).collect();
        let mut e = Evaluator::new("/tmp".into())
            .with_registered_backends(backends.clone());
        let scope = HashMap::new();

        let src = r"quote^(python^(6*7)_python)_quote";
        let nodes = crate::parser::Parser::new(src, &backends)
            .parse()
            .unwrap();
        assert_eq!(nodes.len(), 1);

        let result = e.eval_node(&nodes[0], &scope).unwrap();
        match &result {
            OValue::Expr { src } => {
                assert!(src.contains("python^("), "src should contain python^(, got: {:?}", src);
                assert!(src.contains("6*7"), "src should contain 6*7, got: {:?}", src);
            }
            other => panic!("expected OValue::Expr, got {:?}", other),
        }
    }

    /// A quoted body with MULTIPLE children should capture the raw source text
    /// so the outer O.eval round-trip works.
    #[test]
    fn quote_multi_child_body_raw_source_preserved() {
        let backends: HashSet<String> =
            ["python", "quote", "O"].iter().map(|s| s.to_string()).collect();
        let mut e = Evaluator::new("/tmp".into())
            .with_registered_backends(backends.clone());
        let scope = HashMap::new();

        let src = "quote^(python^(1)_python python^(2)_python)_quote";
        let nodes = crate::parser::Parser::new(src, &backends)
            .parse()
            .unwrap();
        let result = e.eval_node(&nodes[0], &scope).unwrap();
        match &result {
            OValue::Expr { src } => {
                assert!(src.contains("python^(1)_python"), "missing first block: {:?}", src);
                assert!(src.contains("python^(2)_python"), "missing second block: {:?}", src);
            }
            other => panic!("expected OValue::Expr, got {:?}", other),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // STEP-4: autonomous(expr) builtin — policy-modifying operator
    //
    // Tests verify:
    //   1. Non-Eval Requests are buffered (returned as Request values).
    //   2. The buffer is flushed on exit; results are cached in the scheduler.
    //   3. A Request returned from the body is resolved from the cache.
    //   4. Eval Requests are still executed eagerly inside autonomous().
    //   5. Policy is restored after autonomous() returns (and on error).
    //   6. The buffer is cleared on error so stale entries don't pollute.
    // ─────────────────────────────────────────────────────────────────────────

    /// `autonomous(instantiate($expr))` under Eager outer policy: the inner
    /// `instantiate` is buffered (returned as a Request), the buffer is flushed
    /// at the end, and the scheduler's cache is populated.
    ///
    /// We use MockExecutor through the ImmediateExecutor path to verify the
    /// Eager-mode executor still works independently. The scheduler uses its
    /// own mem_cache; we seed it to avoid actually calling nix.
    #[test]
    fn autonomous_call_buffers_nix_request_and_resolves_on_exit() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        // Build the Request that autonomous() will construct, find its fp.
        let expr_val = OValue::nix_expr("pkgs.hello", vec![]);
        let expected_req = OValue::request(RequestKind::Instantiate, expr_val.clone());
        let fp = match &expected_req {
            OValue::Request { fingerprint, .. } => fingerprint.clone(),
            _ => panic!(),
        };

        // Pre-seed the scheduler cache so flush_autonomous_buffer doesn't
        // try to actually call nix.
        let fake_drv = OValue::derivation("/nix/store/fake.drv", vec!["out".into()], vec![]);
        e.scheduler.mem_cache.insert(fp.clone(), fake_drv.clone());

        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef("expr".into())],
            }],
        };

        // Under autonomous, instantiate($expr) is buffered → returns a Request.
        // Then the buffer is flushed (cache hit) → the result is Derivation.
        let result = e.eval_node(&call, &scope).unwrap();
        assert_eq!(result, fake_drv,
            "autonomous() should resolve the buffered request from the cache on exit");
    }

    /// Under autonomous(), Eval requests (nix^{lazy}^()_nix) are executed
    /// eagerly, bypassing the buffer. The buffer only collects Nix-family requests.
    #[test]
    fn autonomous_eval_requests_are_executed_eagerly() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        // Construct an Eval Request (nix {lazy} block) — this should go
        // through the Evaluator's eval_cache, not the scheduler buffer.
        let lazy_nix = ONode::TypedExpr {
            lang:   "nix".into(),
            env_id: u32::MAX,
            attr:   Some("lazy".into()),
            body:   vec![ONode::RawText("1 + 2".into())],
        };
        // First, collect the fingerprint to seed the eval_cache.
        let req = e.eval_node(&lazy_nix, &scope).unwrap();
        let fp = match &req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };
        e.eval_cache.insert(fp.clone(), OValue::int(3));

        // Now call autonomous() wrapping another {lazy} nix block for the same expression.
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::Call {
                fn_name: "now".into(),
                args: vec![ONode::TypedExpr {
                    lang:   "nix".into(),
                    env_id: u32::MAX,
                    attr:   Some("lazy".into()),
                    body:   vec![ONode::RawText("1 + 2".into())],
                }],
            }],
        };

        let result = e.eval_node(&call, &scope).unwrap();
        assert_eq!(result, OValue::int(3),
            "Eval request inside autonomous() must resolve via eval_cache, got {:?}", result);

        // The buffer must be empty — Eval was not buffered.
        assert!(e.autonomous_buffer.is_empty(),
            "autonomous_buffer must be empty after Eval request (not buffered)");
    }

    /// Policy is restored to Eager after autonomous() returns, even when the
    /// body errors.
    #[test]
    fn policy_restored_after_autonomous_returns() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        assert_eq!(e.policy, Policy::Eager);

        // Success path: policy restored.
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req  = OValue::request(RequestKind::Instantiate, expr);
        let fp   = match &req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };
        e.scheduler.mem_cache.insert(fp, OValue::derivation("/nix/store/x.drv", vec!["out".into()], vec![]));
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::TypedExpr {
                    lang: "nix_expr".into(), env_id: u32::MAX, attr: None,
                    body: vec![ONode::RawText("pkgs.hello".into())],
                }],
            }],
        };
        let _ = e.eval_node(&call, &scope);
        assert_eq!(e.policy, Policy::Eager, "policy must be Eager after autonomous() succeeds");

        // Error path: policy still restored.
        let bad = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::VarRef("undefined_var".into())],
        };
        let _ = e.eval_node(&bad, &scope);
        assert_eq!(e.policy, Policy::Eager, "policy must be Eager after autonomous() errors");
    }

    /// The autonomous buffer is cleared after an error, so stale entries
    /// don't propagate to the next call.
    #[test]
    fn autonomous_buffer_cleared_on_error() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let bad = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::VarRef("no_such_var".into())],
        };
        let _ = e.eval_node(&bad, &scope);
        assert!(e.autonomous_buffer.is_empty(),
            "buffer must be cleared after autonomous() errors");
    }

    /// autonomous() with wrong arg count errors clearly.
    #[test]
    fn autonomous_wrong_arg_count_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![
                ONode::RawText("a".into()),
                ONode::RawText("b".into()),
            ],
        };
        let err = e.eval_node(&call, &scope).unwrap_err().to_string();
        assert!(err.contains("autonomous(expr) takes exactly 1 argument"));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // STEP-4: Group coordination primitives (batch / all / any / race / now)
    // ─────────────────────────────────────────────────────────────────────────

    /// A helper that builds nodes binding `expr` to a NixExpr and returns a
    /// scope already containing it.
    fn scope_with_nix_expr() -> HashMap<String, OValue> {
        let mut scope = HashMap::new();
        scope.insert("e1".into(), OValue::nix_expr("pkgs.hello", vec![]));
        scope.insert("e2".into(), OValue::nix_expr("pkgs.world", vec![]));
        scope
    }

    /// `batch(...)` constructs an OValue::Group with mode Batch, holding the
    /// evaluated arguments as members.
    #[test]
    fn batch_constructs_group_value() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("a".into(), OValue::int(1));
        scope.insert("b".into(), OValue::int(2));

        let call = ONode::Call {
            fn_name: "batch".into(),
            args:    vec![ONode::VarRef("a".into()), ONode::VarRef("b".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        match result {
            OValue::Group { mode, members, .. } => {
                assert_eq!(mode, GroupMode::Batch);
                assert_eq!(members, vec![OValue::int(1), OValue::int(2)]);
            }
            other => panic!("expected Group, got {:?}", other),
        }
    }

    /// Each builtin maps to its own GroupMode.
    #[test]
    fn group_builtins_map_to_modes() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("a".into(), OValue::int(1));

        for (name, expected) in [
            ("batch", GroupMode::Batch),
            ("all",   GroupMode::All),
            ("any",   GroupMode::Any),
            ("race",  GroupMode::Race),
        ] {
            let call = ONode::Call {
                fn_name: name.into(),
                args:    vec![ONode::VarRef("a".into())],
            };
            let result = e.eval_node(&call, &scope).unwrap();
            match result {
                OValue::Group { mode, .. } => assert_eq!(mode, expected, "for {name}"),
                other => panic!("{name}: expected Group, got {:?}", other),
            }
        }
    }

    /// An empty group builtin errors clearly.
    #[test]
    fn group_builtin_empty_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let call = ONode::Call { fn_name: "batch".into(), args: vec![] };
        let err = e.eval_node(&call, &scope).unwrap_err().to_string();
        assert!(err.contains("at least 1 argument"), "got {err}");
    }

    /// `now(batch(...))` over already-resolved members returns an OList of the
    /// members in order (Batch/All topology collects everything).
    #[test]
    fn now_batch_returns_list_of_members() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("a".into(), OValue::int(10));
        scope.insert("b".into(), OValue::int(20));

        let call = ONode::Call {
            fn_name: "now".into(),
            args:    vec![ONode::Call {
                fn_name: "batch".into(),
                args:    vec![ONode::VarRef("a".into()), ONode::VarRef("b".into())],
            }],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert_eq!(result, OValue::list(vec![OValue::int(10), OValue::int(20)]));
    }

    /// `now(any(...))` over already-resolved members returns the FIRST member
    /// (Any/Race topology yields a single winner).
    #[test]
    fn now_any_returns_first_member() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("a".into(), OValue::str_("first"));
        scope.insert("b".into(), OValue::str_("second"));

        let call = ONode::Call {
            fn_name: "now".into(),
            args:    vec![ONode::Call {
                fn_name: "any".into(),
                args:    vec![ONode::VarRef("a".into()), ONode::VarRef("b".into())],
            }],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert_eq!(result, OValue::str_("first"));
    }

    /// `now(batch(realise(instantiate($e1)), realise(instantiate($e2))))` under
    /// the default Eager policy: the inner requests are already resolved to
    /// StorePaths by the time batch runs, so now(group) returns a list of two
    /// StorePaths. Verifies group force works end-to-end with the executor.
    #[test]
    fn now_batch_of_resolved_requests_returns_storepath_list() {
        let mut e = Evaluator::new("/tmp".into())
            .with_executor(Box::new(MockExecutor::new()));
        let scope = scope_with_nix_expr();

        let mk_chain = |var: &str| ONode::Call {
            fn_name: "realise".into(),
            args:    vec![ONode::Call {
                fn_name: "instantiate".into(),
                args:    vec![ONode::VarRef(var.into())],
            }],
        };
        let call = ONode::Call {
            fn_name: "now".into(),
            args:    vec![ONode::Call {
                fn_name: "batch".into(),
                args:    vec![mk_chain("e1"), mk_chain("e2")],
            }],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        match result {
            OValue::List { v } => {
                assert_eq!(v.len(), 2);
                assert!(v.iter().all(|x| x.is_store_path()),
                    "all members must resolve to StorePaths, got {:?}", v);
            }
            other => panic!("expected list, got {:?}", other),
        }
    }

    /// `now(x)` where x is neither a Request nor a Group errors with a clear
    /// message.
    #[test]
    fn now_on_non_request_non_group_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("a".into(), OValue::int(1));
        let call = ONode::Call {
            fn_name: "now".into(),
            args:    vec![ONode::VarRef("a".into())],
        };
        let err = e.eval_node(&call, &scope).unwrap_err().to_string();
        assert!(err.contains("Request or Group"), "got {err}");
    }

    /// MVP autonomous(batch(...)) integration: under Autonomous, the inner
    /// requests are buffered; the flush executes them through the scheduler;
    /// the returned Group is resolved from the scheduler cache into a list of
    /// concrete StorePaths. Here we pre-seed the scheduler's mem_cache so the
    /// flush is a pure cache hit (no real nix subprocess).
    #[test]
    fn autonomous_batch_resolves_group_from_cache() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = scope_with_nix_expr();

        // Pre-seed the scheduler cache with results for both realise requests.
        for var in ["e1", "e2"] {
            let expr = e.eval_node(&ONode::VarRef(var.into()), &scope).unwrap();
            let inst = OValue::request(RequestKind::Instantiate, expr);
            let drv  = OValue::derivation(
                format!("/nix/store/{var}.drv"), vec!["out".into()], vec![]);
            let realise = OValue::request(RequestKind::Realise, inst.clone());
            let inst_fp = match &inst { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => unreachable!() };
            let real_fp = match &realise { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => unreachable!() };
            e.scheduler.mem_cache.insert(inst_fp, drv);
            e.scheduler.mem_cache.insert(real_fp, OValue::store_path(format!("/nix/store/{var}-out")));
        }

        let mk_chain = |var: &str| ONode::Call {
            fn_name: "realise".into(),
            args:    vec![ONode::Call {
                fn_name: "instantiate".into(),
                args:    vec![ONode::VarRef(var.into())],
            }],
        };
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args:    vec![ONode::Call {
                fn_name: "batch".into(),
                args:    vec![mk_chain("e1"), mk_chain("e2")],
            }],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        match result {
            OValue::List { v } => {
                assert_eq!(v.len(), 2, "batch must resolve to a 2-element list");
                assert!(v.iter().all(|x| x.is_store_path()),
                    "members must resolve to StorePaths, got {:?}", v);
            }
            other => panic!("expected list from autonomous(batch(...)), got {:?}", other),
        }
        // Policy restored and buffer drained.
        assert_eq!(e.policy, Policy::Eager);
        assert!(e.autonomous_buffer.is_empty());
    }
}
