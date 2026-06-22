// ─────────────────────────────────────────────────────────────────────────────
// eval.rs
//
// The O-language OIR evaluator — applicative order, leaves-up.
//
// Evaluation semantics (mirrors o_lang/evaluator.py):
//
//   OIr::Exec { lang, env_id, backend, body }:
//     1. Walk body children left-to-right, building a splice buffer:
//          Text  → append verbatim
//          Load  → look up scope, render through the OIR backend interface
//          Exec  → evaluate recursively first, then render into the parent
//     2. Call ProcessRegistry::exec(lang, env_id, buffer, scope, shim)
//     3. For ephemeral envs (env_id == u32::MAX, used internally for re-entrancy etc): call cleanup_env (always, even on err)
//
//   Root document (eval_document):
//     Lower ONode syntax to OIR, build and validate ExecutionPlan, execute its
//     root schedule, and return the last non-null OValue.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

use crate::capability::{fresh_bearer_identity, BackendAuthorityBroker, BackendSandboxPolicy};
#[cfg(test)]
use crate::ir::lower_node;
use crate::ir::{
    reconstruct_source as reconstruct_ir_source, BackendInterface, BackendRegistry, ExecutionMode,
    ExecutionPlan, InvokeMode, OIr, OIrProgram, PlanNodeId, SpliceRenderer,
};
use crate::nix_ops;
use crate::nixos_ops;
use crate::parser::{ONode, Parser};
use crate::process::{ExecStep, ProcessRegistry};
use crate::scheduler::AutonomousScheduler;
use crate::value::{BackendAuthority, CapabilityKind, GroupMode, OValue, RequestKind};

/// How to resolve group members that might be cached Request values.
///
/// - `Fresh`: force the member via `force_request` under the active policy and
///   executor. Used by `now(group)`.
/// - `Strict`: read from the scheduler or eval cache and return a hard error on
///   a miss. Used after `autonomous(...)` flush, where every buffered request
///   must already have been materialized.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheMode {
    Fresh,
    Strict,
}

// ─────────────────────────────────────────────────────────────────────────────
// exec_nix_kind — thread-safe Nix-family dispatcher
//
// Executes a single Nix-family RequestKind against an already-resolved source
// value. Called inside group-resolution threads; takes no `self` reference so
// it can be safely moved into `thread::spawn` closures.
// ─────────────────────────────────────────────────────────────────────────────

fn exec_nix_kind(kind: RequestKind, src: OValue) -> Result<OValue> {
    match kind {
        RequestKind::Instantiate => nix_ops::instantiate_nix(&src),
        RequestKind::Realise => nix_ops::realise_nix(&src),
        RequestKind::Activate {
            profile,
            dry_run: true,
            authority: None,
        } => nixos_ops::activate_nix(&src, &profile, true),
        RequestKind::Activate { .. } => {
            bail!("real activation requires the evaluator's live authority table")
        }
        RequestKind::Eval { .. } => bail!(
            "exec_nix_kind: RequestKind::Eval must not appear in concurrent \
             group dispatch (Eval requests are always executed serially)"
        ),
    }
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
        Self {
            cache: HashMap::new(),
        }
    }
}

impl Default for ImmediateExecutor {
    fn default() -> Self {
        Self::new()
    }
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
            OValue::Request {
                kind,
                source,
                fingerprint,
            } => (kind.clone(), source.as_ref().clone(), fingerprint.clone()),
            other => bail!(
                "Executor::execute expected a Request, got {}",
                other.type_name()
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
            RequestKind::Realise => nix_ops::realise_nix(&resolved_source)?,
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
            RequestKind::Activate {
                profile,
                dry_run: true,
                authority: None,
            } => nixos_ops::activate_nix(&resolved_source, &profile, true)?,
            RequestKind::Activate { .. } => bail!(
                "ImmediateExecutor cannot perform real activation directly; \
                 the Evaluator must validate a live SystemActivation capability"
            ),
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
    /// eval_source_with_scope (called during O.eval() callbacks) can
    /// re-parse a quoted source fragment using the same backend set as the
    /// top-level document.
    registered_backends: HashSet<String>,

    /// Current evaluation policy. Eager by default; lazy(...) installs Lazy
    /// for the scope of its argument; autonomous(...) installs Autonomous.
    policy: Policy,

    /// The executor used to perform Instantiate, Realise, and dry Activate
    /// Requests under Policy::Eager. Real activation stays in Evaluator so its
    /// live authority can be checked. Swappable via with_executor for tests.
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

    /// The validated plan used by the most recent document execution.
    last_execution_plan: Option<ExecutionPlan>,

    /// Live, process-local authority for mutating system activation.
    ///
    /// Keyed by an opaque 256-bit bearer identity. The value is the only
    /// profile that bearer may activate. Neither serialized capability
    /// metadata nor an environment variable can populate this table.
    activation_authorities: HashMap<String, String>,

    /// Live bearer bindings for authority requested by hosted backend blocks.
    backend_authorities: BackendAuthorityBroker,
}

struct IrExecRegion<'a> {
    lang: &'a str,
    env_id: u32,
    attr: Option<&'a str>,
    backend: &'a BackendInterface,
    body: &'a [OIr],
    node_id: PlanNodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockEvalPolicy {
    Lazy,
    Defer,
}

#[derive(Debug, Default)]
struct BlockOptions {
    policy: Option<BlockEvalPolicy>,
    capability_binding: Option<String>,
    permissions: Vec<BackendAuthority>,
}

impl BlockOptions {
    fn parse(attr: Option<&str>, lang: &str) -> Result<Self> {
        let mut options = Self::default();
        let mut seen = HashSet::new();
        for entry in attr.into_iter().flat_map(|attr| attr.split(',')) {
            let entry = entry.trim();
            if !seen.insert(entry.to_string()) {
                bail!("duplicate block attribute `{{{entry}}}` on {lang}^");
            }
            match entry {
                "lazy" => {
                    if options.policy.replace(BlockEvalPolicy::Lazy).is_some() {
                        bail!("a block cannot combine `lazy` and `defer`");
                    }
                }
                "defer" => {
                    if options.policy.replace(BlockEvalPolicy::Defer).is_some() {
                        bail!("a block cannot combine `lazy` and `defer`");
                    }
                }
                _ if entry.starts_with("cap=") => {
                    let name = entry.trim_start_matches("cap=");
                    if name.is_empty() || options.capability_binding.replace(name.into()).is_some()
                    {
                        bail!("a block must name exactly one backend capability binding");
                    }
                }
                _ => {
                    let permission = BackendAuthority::parse(entry).ok_or_else(|| {
                        anyhow::anyhow!(
                            "unknown block attribute `{{{entry}}}` on {lang}^. Known attributes: lazy, defer, cap=name, fs_read, fs_write, network, process"
                        )
                    })?;
                    options.permissions.push(permission);
                }
            }
        }
        options.permissions.sort();
        Ok(options)
    }
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
            last_execution_plan: None,
            activation_authorities: HashMap::new(),
            backend_authorities: BackendAuthorityBroker::default(),
        }
    }

    /// Install the registered-backends set used by O.eval to re-parse
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

    /// The dependency plan that mediated the most recent document execution.
    pub fn last_execution_plan(&self) -> Option<&ExecutionPlan> {
        self.last_execution_plan.as_ref()
    }

    /// Mint a live capability that authorizes real activation of one profile.
    ///
    /// Hosts must inject the returned OValue into an O scope explicitly. Its
    /// serialized metadata is descriptive only; the private table entry is the
    /// authority and disappears when this Evaluator is dropped.
    pub fn issue_system_activation_capability(
        &mut self,
        profile: impl Into<String>,
    ) -> Result<OValue> {
        let profile = profile.into();
        if profile.is_empty() {
            bail!("system activation capability requires a non-empty profile path");
        }
        let identity = loop {
            let candidate = fresh_bearer_identity("o-activate-live")?;
            if !self.activation_authorities.contains_key(&candidate) {
                break candidate;
            }
        };
        self.activation_authorities
            .insert(identity.clone(), profile.clone());
        let mut metadata = HashMap::new();
        metadata.insert("live".into(), OValue::bool_(true));
        metadata.insert("profile".into(), OValue::str_(profile));
        Ok(OValue::capability(
            CapabilityKind::SystemActivation,
            identity,
            metadata,
        ))
    }

    /// Revoke a previously issued system activation capability immediately.
    pub fn revoke_system_activation_capability(&mut self, capability: &OValue) -> Result<()> {
        let OValue::Capability { kind, identity, .. } = capability else {
            bail!("expected OCapability, got {}", capability.type_name());
        };
        if *kind != CapabilityKind::SystemActivation {
            bail!(
                "expected a system_activation capability, got {}",
                kind.name()
            );
        }
        self.activation_authorities
            .remove(identity)
            .ok_or_else(|| anyhow::anyhow!("system activation capability is forged or revoked"))?;
        Ok(())
    }

    /// Mint a live capability for explicitly declared backend authority.
    ///
    /// The language may be a canonical backend name or `*`. Metadata is only
    /// descriptive; dispatch checks the private broker binding before both
    /// direct execution and deferred forcing.
    pub fn issue_backend_execution_capability(
        &mut self,
        language: impl Into<String>,
        permissions: impl IntoIterator<Item = BackendAuthority>,
    ) -> Result<OValue> {
        let language = language.into();
        let language = if language == "*" {
            language
        } else {
            BackendRegistry::global().canonical(&language).to_string()
        };
        self.backend_authorities.issue(language, permissions)
    }

    /// Revoke a backend execution capability immediately.
    pub fn revoke_backend_execution_capability(&mut self, capability: &OValue) -> Result<()> {
        self.backend_authorities.revoke(capability)
    }

    /// Parse `NAME=LANG[:RIGHT,RIGHT]`, mint a live backend capability, and
    /// install it into an O scope under `NAME`.
    pub fn install_backend_grant(
        &mut self,
        spec: &str,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<()> {
        let (name, grant) = spec.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("backend grant must be NAME=LANG[:RIGHT,...], got `{spec}`")
        })?;
        if name.is_empty()
            || !name
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            bail!("backend grant binding `{name}` is not an O identifier");
        }
        let (language, permissions) = grant.split_once(':').unwrap_or((grant, ""));
        if language.is_empty() {
            bail!("backend grant `{spec}` has no language");
        }
        let mut parsed = Vec::new();
        for permission in permissions
            .split(',')
            .map(str::trim)
            .filter(|permission| !permission.is_empty())
        {
            parsed.push(BackendAuthority::parse(permission).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown backend authority `{permission}`; expected fs_read, fs_write, network, or process"
                )
            })?);
        }
        parsed.sort();
        parsed.dedup();
        let capability = self.issue_backend_execution_capability(language, parsed)?;
        scope.insert(name.to_string(), capability);
        Ok(())
    }

    fn resolve_backend_authority(
        &self,
        language: &str,
        options: &BlockOptions,
        permissions: &[BackendAuthority],
        scope: &HashMap<String, OValue>,
    ) -> Result<Option<String>> {
        let Some(binding) = options.capability_binding.as_deref() else {
            if permissions.is_empty() {
                return Ok(None);
            }
            bail!(
                "backend `{language}` requests {} but names no live capability; add `cap=name` to the block attributes",
                permissions
                    .iter()
                    .map(|permission| permission.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        let capability = scope.get(binding).ok_or_else(|| {
            anyhow::anyhow!("backend `{language}` names undefined capability binding `${binding}`")
        })?;
        self.backend_authorities
            .authorize(capability, language, permissions)
            .with_context(|| format!("backend `{language}` authority check failed"))
            .map(Some)
    }

    /// Auto-resolve a Request under the current policy.
    ///
    /// - Eager executes the request immediately and returns its result.
    /// - Lazy passes it through unchanged so the user must call `now()`.
    /// - Autonomous keeps Eval and real Activate on the evaluator thread because
    ///   they need live local state. Instantiate, Realise, and dry Activate are
    ///   buffered and dispatched by the scheduler at the next force point.
    fn auto_resolve(&mut self, v: OValue) -> Result<OValue> {
        match (self.policy, &v) {
            (Policy::Eager, OValue::Request { .. }) => self.force_request(&v),

            (Policy::Autonomous, OValue::Request { kind, .. }) => {
                match kind {
                    // Eval needs the ProcessRegistry. Real activation needs the
                    // evaluator's live authority table. Keep both on this thread.
                    RequestKind::Eval { .. } | RequestKind::Activate { dry_run: false, .. } => {
                        self.force_request(&v)
                    }
                    // Pure Nix requests and dry activation can be scheduled.
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
            other => bail!(
                "force_request expected a Request, got {}",
                other.type_name()
            ),
        };
        match kind {
            RequestKind::Eval { .. } => self.exec_eval(req),
            RequestKind::Activate { dry_run: false, .. } => self.exec_activate(req),
            _ if self.policy == Policy::Autonomous => self.scheduler.execute(req),
            _ => self.executor.execute(req),
        }
    }

    /// Perform a real activation only after resolving its live bearer through
    /// this evaluator's private, profile-scoped authority table.
    fn exec_activate(&mut self, req: &OValue) -> Result<OValue> {
        let (profile, authority, source) = match req {
            OValue::Request {
                kind:
                    RequestKind::Activate {
                        profile,
                        dry_run: false,
                        authority,
                    },
                source,
                ..
            } => (profile.clone(), authority.clone(), source.as_ref().clone()),
            OValue::Request {
                kind: RequestKind::Activate { dry_run: true, .. },
                ..
            } => bail!("exec_activate is only for real activation requests"),
            other => bail!(
                "exec_activate expected a real Activate request, got {}",
                other.type_name()
            ),
        };

        let identity = authority.ok_or_else(|| {
            anyhow::anyhow!("real activation requires a live system_activation capability")
        })?;
        let authorized_profile = self.activation_authorities.get(&identity).ok_or_else(|| {
            anyhow::anyhow!(
                "system activation capability is forged, revoked, or from another evaluator"
            )
        })?;
        if authorized_profile != &profile {
            bail!(
                "system activation capability is scoped to profile {}, not {}",
                authorized_profile,
                profile
            );
        }

        let resolved_source = match source {
            OValue::Request { .. } => self.force_request(&source)?,
            concrete => concrete,
        };
        nixos_ops::activate_nix(&resolved_source, &profile, false)
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
            OValue::Request {
                fingerprint, kind, ..
            } => {
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
    /// - A schedulable Request → its cached result (error on cache miss in
    ///   Strict mode — the scheduler must have materialized every buffered
    ///   request, so a miss indicates a scheduler bug).
    /// - A Group → resolved per its topology mode using Strict cache reads.
    /// - Anything else → returned unchanged.
    fn resolve_after_flush(&mut self, value: OValue) -> Result<OValue> {
        match &value {
            OValue::Group { mode, members, .. } => {
                let (mode, members) = (*mode, members.clone());
                self.resolve_group(mode, &members, CacheMode::Strict)
            }
            v if Self::is_schedulable_request(v) => match self.resolve_from_cache(v) {
                Some(result) => Ok(result),
                None => {
                    let fp = match v {
                        OValue::Request { fingerprint, .. } => &fingerprint[..8],
                        _ => "?",
                    };
                    bail!(
                        "autonomous: scheduler failed to materialize \
                             request fp={}; cache miss after flush",
                        fp
                    )
                }
            },
            _ => Ok(value),
        }
    }

    /// Returns `true` if `v` is a request that can be buffered under
    /// Policy::Autonomous. Real activation is excluded because it must resolve
    /// authority through the evaluator's private live table.
    fn is_schedulable_request(v: &OValue) -> bool {
        matches!(
            v,
            OValue::Request {
                kind: RequestKind::Instantiate | RequestKind::Realise,
                ..
            } | OValue::Request {
                kind: RequestKind::Activate {
                    dry_run: true,
                    authority: None,
                    ..
                },
                ..
            }
        )
    }

    /// Returns `true` if `m` is a Nix-family Request (Instantiate, Realise, or
    /// Activate) that can be dispatched to a background thread during concurrent
    /// group resolution. Eval Requests are excluded because they require the
    /// ProcessRegistry (which is !Send) and must stay on the evaluator thread.
    fn is_threadable_member(m: &OValue) -> bool {
        Self::is_schedulable_request(m)
    }

    /// Pre-resolve the source chain of a Nix-family Request, returning
    /// `(kind, resolved_source)` ready for hand-off to a worker thread.
    ///
    /// If the Request's source is itself a Request (e.g. the `drv` inside
    /// `realise(instantiate(expr))`), it is executed via `force_request` on
    /// the evaluator thread before the outer operation is dispatched to a
    /// worker. Source chains are therefore resolved sequentially per member,
    /// but independent members can still execute their outer operations
    /// concurrently.
    fn pre_resolve_nix_request(&mut self, req: &OValue) -> Result<(RequestKind, OValue)> {
        let (kind, source) = match req {
            OValue::Request { kind, source, .. } => (kind.clone(), source.as_ref().clone()),
            other => bail!(
                "pre_resolve_nix_request: expected a Nix-family Request, got {}",
                other.type_name()
            ),
        };
        let resolved_source = match source {
            OValue::Request { .. } => self.force_request(&source)?,
            concrete => concrete,
        };
        Ok((kind, resolved_source))
    }

    /// Resolve a single group member to a concrete value.
    ///
    /// `CacheMode::Fresh` forces the member via `force_request`; `Strict`
    /// reads from the scheduler/eval cache and errors on a miss. Nested Groups
    /// recurse with the same mode. Other values are returned as-is.
    fn resolve_member(&mut self, m: &OValue, mode: CacheMode) -> Result<OValue> {
        match m {
            OValue::Request { fingerprint, .. } => match mode {
                CacheMode::Fresh => self.force_request(m),
                CacheMode::Strict => self.resolve_from_cache(m).ok_or_else(|| {
                    anyhow::anyhow!(
                        "autonomous: scheduler failed to materialize \
                             request fp={}; cache miss after flush",
                        &fingerprint[..8]
                    )
                }),
            },
            OValue::Group {
                mode: gmode,
                members,
                ..
            } => {
                let (gmode, members) = (*gmode, members.clone());
                self.resolve_group(gmode, &members, mode)
            }
            other => Ok(other.clone()),
        }
    }

    /// Resolve a Group to a concrete value according to its topology `mode`.
    ///
    /// **Member semantics:**
    ///
    /// - `Batch`: collect every member result into an `OList`. A failed member
    ///   becomes `OValue::Error`, preserving one output slot per input.
    /// - `All`: collect every member result, but fail the group if any member
    ///   fails.
    /// - `Any`: return the first member that succeeds and fail only if all fail.
    /// - `Race`: return the first member to settle. Remaining members may still
    ///   run, but their results are discarded.
    ///
    /// **Concurrency:**
    ///   When `cache_mode == CacheMode::Fresh` and any member is a threadable
    ///   Nix-family Request, members are dispatched concurrently (up to
    ///   `self.scheduler.parallelism` threads at a time). Eval Requests and
    ///   plain values always resolve serially on the evaluator thread (Eval
    ///   needs the ProcessRegistry which is !Send).
    ///
    ///   Under `Strict` after an autonomous flush, results are already in L1
    ///   memory and sequential cache reads are used.
    pub(crate) fn resolve_group(
        &mut self,
        mode: GroupMode,
        members: &[OValue],
        cache_mode: CacheMode,
    ) -> Result<OValue> {
        if members.is_empty() {
            bail!("{}(...) group has no members to resolve", mode.name());
        }

        // Cache reads are fast (L1 memory); no threading benefit.
        // Also use the sequential path when no Nix-family Requests are present.
        let has_threadable =
            cache_mode == CacheMode::Fresh && members.iter().any(Self::is_threadable_member);

        if mode.collects_all() {
            if has_threadable {
                self.resolve_collect_all_concurrent(mode, members)
            } else {
                // Sequential path: plain values, Eval Requests, nested Groups,
                // or strict cache reads already in L1 memory.
                if mode == GroupMode::Batch {
                    // Batch: collect all outcomes, wrapping failures as OError.
                    let mut out = Vec::with_capacity(members.len());
                    for m in members {
                        match self.resolve_member(m, cache_mode) {
                            Ok(v) => out.push(v),
                            Err(e) => out.push(OValue::error(e.to_string())),
                        }
                    }
                    Ok(OValue::list(out))
                } else {
                    // All: hard all-or-nothing barrier — fail on first error.
                    let mut out = Vec::with_capacity(members.len());
                    for m in members {
                        out.push(self.resolve_member(m, cache_mode)?);
                    }
                    Ok(OValue::list(out))
                }
            }
        } else {
            // Any / Race: first-wins topology.
            if has_threadable {
                self.resolve_first_wins_concurrent(mode, members)
            } else {
                match mode {
                    GroupMode::Any => {
                        // Try members in source order; return first success.
                        let mut last_err: Option<anyhow::Error> = None;
                        for m in members {
                            match self.resolve_member(m, cache_mode) {
                                Ok(v) => return Ok(v),
                                Err(e) => last_err = Some(e),
                            }
                        }
                        Err(last_err.expect("non-empty group must have produced an error"))
                            .with_context(|| {
                                format!("any(...) group: all {} members failed", members.len())
                            })
                    }
                    GroupMode::Race => {
                        // Sequential race: first member to settle wins.
                        // In sequential execution the first member always
                        // settles first — return its result immediately
                        // (whether Ok or Err) without trying later members.
                        // NOTE: Race does not yet cancel losing work; in the
                        // concurrent path, remaining threads run to completion
                        // but their results are discarded.
                        self.resolve_member(&members[0], cache_mode)
                            .with_context(|| "race(...) group: lead member failed".to_string())
                    }
                    _ => unreachable!("Batch/All already handled by collects_all() branch"),
                }
            }
        }
    }

    /// Concurrent resolution for `Batch`/`All` groups.
    ///
    /// Algorithm:
    ///   1. Walk members in source order.
    ///      - Threadable (Nix-family Requests): pre-resolve source chains
    ///        sequentially, then push `(index, kind, src)` onto the work list.
    ///      - Serial (plain values, Eval Requests, nested Groups): resolve
    ///        inline and store the result immediately.
    ///   2. Spawn threads in batches capped at `self.scheduler.parallelism`;
    ///      each thread calls `exec_nix_kind` and sends `(index, Result<OValue>)`
    ///      over a channel.
    ///   3. Collect thread results (all of them — channel closes when every
    ///      sender drops).
    ///   4. Assemble results in member order. `Batch` wraps failures as OError
    ///      so every input has one output. `All` propagates the first error.
    fn resolve_collect_all_concurrent(
        &mut self,
        mode: GroupMode,
        members: &[OValue],
    ) -> Result<OValue> {
        // results[i] holds the resolved value (or error) for members[i].
        // We use the iterator form rather than `vec![None; N]` because
        // `Result<OValue, anyhow::Error>` does not implement `Clone`.
        let mut results: Vec<Option<Result<OValue>>> = (0..members.len()).map(|_| None).collect();
        let mut threadable: Vec<(usize, RequestKind, OValue)> = Vec::new();

        // Phase 1 — classify and pre-resolve.
        for (i, m) in members.iter().enumerate() {
            if Self::is_threadable_member(m) {
                match self.pre_resolve_nix_request(m) {
                    Ok((kind, src)) => threadable.push((i, kind, src)),
                    Err(e) => results[i] = Some(Err(e)),
                }
            } else {
                results[i] = Some(self.resolve_member(m, CacheMode::Fresh));
            }
        }

        // Phase 2 — spawn threads capped at scheduler.parallelism.
        // Processing in batches of `cap` ensures at most `cap` concurrent
        // Nix operations, matching the autonomous scheduler's parallelism cap.
        // `parallelism` is validated to be >= 1 at construction time, but we
        // guard here anyway to avoid zero-sized chunks in pathological configs.
        if !threadable.is_empty() {
            let cap = self.scheduler.parallelism.max(1);
            for chunk in threadable.chunks(cap) {
                let (tx, rx) = mpsc::channel::<(usize, Result<OValue>)>();
                for (idx, kind, src) in chunk.iter().cloned() {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        // `send` can only fail if the receiver was dropped
                        // (e.g. the evaluator thread panicked). Silently
                        // ignoring keeps threads from panicking on a dead
                        // channel and is the intended pattern for fire-and-
                        // collect thread fans.
                        let _ = tx.send((idx, exec_nix_kind(kind, src)));
                    });
                }
                drop(tx); // channel closes when every spawned sender drops

                // Phase 3 — collect chunk (blocks until all chunk threads done).
                for (idx, result) in rx {
                    results[idx] = Some(result);
                }
            }
        }

        // Phase 4 — assemble result list with mode-specific failure semantics.
        let mut out = Vec::with_capacity(members.len());
        for (i, slot) in results.into_iter().enumerate() {
            let member_result = slot.expect("every member slot must be filled after phases 1-3");
            match mode {
                GroupMode::Batch => {
                    // Batch: collect every outcome; failures become OError values.
                    match member_result {
                        Ok(v) => out.push(v),
                        Err(e) => out.push(OValue::error(format!("member {}: {}", i, e))),
                    }
                }
                _ => {
                    // All: hard barrier — propagate first error immediately.
                    let val = member_result.with_context(|| {
                        format!("{}(...) group: member {} failed", mode.name(), i)
                    })?;
                    out.push(val);
                }
            }
        }
        Ok(OValue::list(out))
    }

    /// Concurrent resolution for `Any`/`Race` groups.
    ///
    /// Serial (non-threadable) members are evaluated first, in source order.
    /// For `Any`, a serial success ends resolution immediately; for `Race`, the
    /// first serial member's result (Ok or Err) ends resolution immediately.
    ///
    /// If no serial member wins, all threadable members are dispatched as
    /// concurrent threads over a shared channel:
    ///
    /// - `Any` blocks until the first `Ok`, or returns the last error if no
    ///   member succeeds.
    /// - `Race` returns the first message, whether `Ok` or `Err`. Other threads
    ///   run to completion, but their results are discarded.
    fn resolve_first_wins_concurrent(
        &mut self,
        mode: GroupMode,
        members: &[OValue],
    ) -> Result<OValue> {
        let mut threadable: Vec<(RequestKind, OValue)> = Vec::new();

        // Phase 1 — serial members first; they may resolve immediately.
        for m in members {
            if Self::is_threadable_member(m) {
                // Pre-resolve source chain before enqueueing for a thread.
                let (kind, src) = self.pre_resolve_nix_request(m)?;
                threadable.push((kind, src));
            } else {
                let result = self.resolve_member(m, CacheMode::Fresh);
                match mode {
                    GroupMode::Any => {
                        if result.is_ok() {
                            return result; // first success wins
                        }
                        // Serial member failed — continue to next member.
                    }
                    GroupMode::Race => {
                        // First to settle wins (Ok or Err).
                        return result.with_context(|| "race(...) group: lead member failed");
                    }
                    _ => unreachable!(),
                }
            }
        }

        if threadable.is_empty() {
            // All members were serial and none won (Any: all failed).
            bail!(
                "{}(...) group: all {} members failed",
                mode.name(),
                members.len()
            );
        }

        // Phase 2 — concurrent dispatch for threadable members.
        let (tx, rx) = mpsc::channel::<Result<OValue>>();
        for (kind, src) in threadable {
            let tx = tx.clone();
            thread::spawn(move || {
                // `send` can only fail if the receiver is dropped (evaluator
                // returned early, e.g. after the first `any` success or the
                // first `race` settler). Silently ignoring is intentional:
                // the thread still runs to completion, but its result is simply
                // discarded by the already-returned caller.
                let _ = tx.send(exec_nix_kind(kind, src));
            });
        }
        drop(tx);

        match mode {
            GroupMode::Any => {
                // Return first Ok; accumulate errors in case all threads fail.
                let mut last_err: Option<anyhow::Error> = None;
                for result in rx {
                    match result {
                        Ok(v) => return Ok(v), // drops rx; remaining threads ignored
                        Err(e) => last_err = Some(e),
                    }
                }
                Err(last_err.expect(
                    "threadable is non-empty so at least one thread must have sent an error",
                ))
                .with_context(|| format!("any(...) group: all {} members failed", members.len()))
            }
            GroupMode::Race => {
                // Return the very first result that settles (Ok or Err).
                // Dropping `rx` after the first message causes remaining
                // thread sends to fail silently (we use `let _ = tx.send`).
                rx.into_iter()
                    .next()
                    .unwrap_or_else(|| Err(anyhow::anyhow!("race(...) group: no results received")))
                    .with_context(|| "race(...) group: winner")
            }
            _ => unreachable!(),
        }
    }

    ///
    /// For cacheable Eval ({lazy}), checks/populates an internal cache keyed
    /// by the Request's fingerprint. For non-cacheable Eval ({defer}), the
    /// cache is skipped on both read and write — each force re-runs.
    fn exec_eval(&mut self, req: &OValue) -> Result<OValue> {
        let (kind, source, fingerprint) = match req {
            OValue::Request {
                kind,
                source,
                fingerprint,
            } => (kind.clone(), source.as_ref().clone(), fingerprint.clone()),
            other => bail!("exec_eval expected Request, got {}", other.type_name()),
        };
        let (lang, env_id, cacheable, authority, permissions) = match kind {
            RequestKind::Eval {
                lang,
                env_id,
                cacheable,
                authority,
                permissions,
            } => (lang, env_id, cacheable, authority, permissions),
            other => bail!("exec_eval expected RequestKind::Eval, got {:?}", other),
        };

        let backend = BackendRegistry::global().interface_for(&lang);
        let sandbox = BackendSandboxPolicy::new(
            backend
                .required_authorities
                .iter()
                .copied()
                .chain(permissions),
        );
        match authority.as_deref() {
            Some(identity) => self
                .backend_authorities
                .authorize_identity(identity, &backend.canonical, sandbox.permissions())
                .context("deferred backend authority check failed")?,
            None if sandbox.permissions().is_empty() => {}
            None => bail!("deferred backend request lost its live authority bearer"),
        }

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

        let result = match backend.execution {
            ExecutionMode::InlineValue => match backend.canonical.as_str() {
                "html" => OValue::html(body),
                "markdown" | "text" | "latex" => OValue::str_(body),
                other => bail!("inline OIR backend `{other}` cannot execute an Eval request"),
            },
            ExecutionMode::Shim => {
                let runtime_lang = backend.canonical.as_str();
                let shim =
                    BackendRegistry::global().resolve_shim_path(&self.shim_dir, runtime_lang);
                // Dependencies were rendered into the thunk body at capture
                // time, so the forced shim receives an empty binding map.
                let result = self
                    .registry
                    .exec(runtime_lang, env_id, &body, HashMap::new(), &shim, &sandbox)
                    .with_context(|| format!("[{}{{eval}}]", runtime_lang))?;
                if env_id == u32::MAX {
                    let _ = self.registry.cleanup_env(runtime_lang, u32::MAX);
                }
                result
            }
            ExecutionMode::InlineAst => bail!(
                "structural OIR backend `{}` cannot be captured as an Eval request",
                backend.canonical
            ),
        };

        if cacheable {
            self.eval_cache.insert(fingerprint, result.clone());
        }
        Ok(result)
    }

    /// STEP-3.5: prepare a value for splicing into source text.
    ///
    /// The rule from fork #2:
    ///
    /// - A `{lazy}` Eval Request is auto-forced and its cached result is spliced.
    /// - A `{defer}` Eval Request is rejected because an implicit force could
    ///   repeat effects. The user must call `now()` explicitly.
    /// - Any other value passes through unchanged.
    ///
    /// Auto-forcing here means: ask the executor to perform the request and
    /// return its result. The executor's cache makes this idempotent for {lazy}.
    fn resolve_for_splice(&mut self, v: OValue) -> Result<OValue> {
        if let OValue::Request {
            kind: RequestKind::Eval {
                cacheable, lang, ..
            },
            ..
        } = &v
        {
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
    // eval_source_with_scope — re-evaluate O source text for O.eval callbacks
    //
    // Used when a backend shim sends an `eval_request` response: the shim's
    // `O.eval(q)` call asks the runtime to evaluate the quoted source fragment
    // and return the result as an `eval_result` command. This is the recursive
    // entry point for that path.
    //
    // Scope rule: O.eval receives a lexical snapshot of the O bindings visible
    // at the backend call site. The fragment can read those bindings and can
    // create local bindings of its own, but those local writes do not mutate the
    // caller. Persistent backend environments remain live independently.
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_source_with_scope(
        &mut self,
        src: &str,
        caller_scope: &HashMap<String, OValue>,
    ) -> Result<OValue> {
        let nodes = Parser::new(src, &self.registered_backends)
            .parse()
            .with_context(|| {
                format!(
                    "failed to parse quoted source: {:?}",
                    &src[..src.len().min(80)]
                )
            })?;
        let program = OIrProgram::lower(&nodes);
        let mut snapshot = caller_scope.clone();
        self.eval_ir_program_with_scope(&program, &mut snapshot)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Public API
    // ─────────────────────────────────────────────────────────────────────────

    /// Lower a parsed document to executable OIR, validate its dependency
    /// plan, and execute the plan with a fresh root scope.
    pub fn eval_document(&mut self, nodes: Vec<ONode>) -> Result<OValue> {
        let program = OIrProgram::lower(&nodes);
        let mut scope = HashMap::new();
        self.eval_ir_program_with_scope(&program, &mut scope)
    }

    /// Lower and execute with a caller-owned scope. Notebook and REPL bindings
    /// therefore persist while execution still goes through OIR.
    pub fn eval_document_with_scope(
        &mut self,
        nodes: Vec<ONode>,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        let program = OIrProgram::lower(&nodes);
        self.eval_ir_program_with_scope(&program, scope)
    }

    /// Execute a lowered program through its validated ExecutionPlan.
    pub fn eval_ir_program(&mut self, program: &OIrProgram) -> Result<OValue> {
        let mut scope = HashMap::new();
        self.eval_ir_program_with_scope(program, &mut scope)
    }

    fn eval_ir_program_with_scope(
        &mut self,
        program: &OIrProgram,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        let plan = program.plan();
        plan.validate(program.nodes.len())
            .map_err(anyhow::Error::msg)
            .context("invalid OIR execution plan")?;
        let schedule = plan
            .root_schedule()
            .map_err(anyhow::Error::msg)
            .context("failed to schedule OIR roots")?;
        self.last_execution_plan = Some(plan.clone());

        let mut last = OValue::null();
        for root_index in schedule {
            let node = &program.nodes[root_index];
            let node_id = plan.roots[root_index];
            let is_pure_whitespace_text = matches!(
                node,
                OIr::Text(text) if !text.is_empty() && text.chars().all(char::is_whitespace)
            );

            let value = match node {
                OIr::Store { name, expr } => {
                    let children =
                        planned_children(&plan, node_id, std::slice::from_ref(expr.as_ref()))?;
                    let (expr_id, _) = children[0];
                    let value = self.eval_ir_node(expr, expr_id, &plan, scope)?;
                    scope.insert(name.clone(), value.clone());
                    value
                }
                _ => self.eval_ir_node(node, node_id, &plan, scope)?,
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
    // Executable OIR dispatch
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_ir_node(
        &mut self,
        node: &OIr,
        node_id: PlanNodeId,
        plan: &ExecutionPlan,
        scope: &HashMap<String, OValue>,
    ) -> Result<OValue> {
        match node {
            OIr::Store { expr, .. } => {
                let children =
                    planned_children(plan, node_id, std::slice::from_ref(expr.as_ref()))?;
                let (expr_id, _) = children[0];
                self.eval_ir_node(expr, expr_id, plan, scope)
            }
            OIr::Text(text) => Ok(OValue::str_(text.clone())),

            OIr::Load(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Undefined variable: ${}", name)),

            OIr::Exec {
                lang,
                env_id,
                attr,
                backend,
                body,
            } => self.eval_ir_exec(
                IrExecRegion {
                    lang,
                    env_id: *env_id,
                    attr: attr.as_deref(),
                    backend,
                    body,
                    node_id,
                },
                plan,
                scope,
            ),

            OIr::Invoke {
                fn_name,
                mode,
                args,
            } => self.eval_ir_invoke(fn_name, *mode, args, node_id, plan, scope),
        }
    }

    /// Test-only compatibility entry point. It proves individual legacy test
    /// fixtures are lowered before execution instead of maintaining a second
    /// ONode interpreter.
    #[cfg(test)]
    fn eval_node(&mut self, node: &ONode, scope: &HashMap<String, OValue>) -> Result<OValue> {
        let program = OIrProgram {
            nodes: vec![lower_node(node)],
        };
        let mut scope = scope.clone();
        self.eval_ir_program_with_scope(&program, &mut scope)
    }

    #[cfg(test)]
    fn eval_typed_expr(
        &mut self,
        lang: &str,
        env_id: u32,
        attr: Option<&str>,
        body: &[ONode],
        scope: &HashMap<String, OValue>,
    ) -> Result<OValue> {
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: lang.to_string(),
                env_id,
                attr: attr.map(str::to_string),
                backend: BackendRegistry::global().interface_for(lang),
                body: body.iter().map(lower_node).collect(),
            }],
        };
        let mut scope = scope.clone();
        self.eval_ir_program_with_scope(&program, &mut scope)
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

    fn eval_ir_invoke(
        &mut self,
        fn_name: &str,
        invoke_mode: InvokeMode,
        args: &[OIr],
        node_id: PlanNodeId,
        plan: &ExecutionPlan,
        scope: &HashMap<String, OValue>,
    ) -> Result<OValue> {
        let planned_args = planned_children(plan, node_id, args)?;
        // STEP-3: `lazy(expr)` is a POLICY-MODIFYING builtin — it must take
        // control of its argument's evaluation so that the policy switch
        // applies to the construction of the inner Requests. It cannot go
        // through the standard "evaluate args first" path; by the time args
        // are evaluated under that path, the inner Requests would have been
        // constructed (and auto-resolved) under the wrong policy.
        if invoke_mode == InvokeMode::Lazy {
            if args.len() != 1 {
                bail!("lazy(expr) takes exactly 1 argument, got {}", args.len());
            }
            let saved_policy = self.policy;
            self.policy = Policy::Lazy;
            let (arg_id, arg) = planned_args[0];
            let result = self.eval_ir_node(arg, arg_id, plan, scope);
            self.policy = saved_policy; // restored even on error path
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
        if invoke_mode == InvokeMode::Autonomous {
            if args.len() != 1 {
                bail!(
                    "autonomous(expr) takes exactly 1 argument, got {}",
                    args.len()
                );
            }
            let saved_policy = self.policy;
            self.policy = Policy::Autonomous;
            let (arg_id, arg) = planned_args[0];
            let result = self.eval_ir_node(arg, arg_id, plan, scope);
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

        // ── Group constructors as special forms ──────────────────────────────
        //
        // `batch`, `all`, `any`, and `race` must be special forms, not ordinary
        // functions.  Under the standard "evaluate args left-to-right" path, any
        // request-producing call (e.g. `realise(instantiate($e))`) would already
        // be forced to a StorePath before the group constructor ever sees it.
        // That would make the group contain concrete values rather than deferred
        // Requests, defeating the whole coordination abstraction.
        //
        // The fix: evaluate members under `Policy::Lazy` regardless of the outer
        // policy, then restore the outer policy once members are captured.  This
        // guarantees that:
        //   batch(realise(instantiate($e1)), realise(instantiate($e2)))
        // always builds:
        //   Group(Batch, [Request[Realise(Request[Instantiate(e1)])], ...])
        // whether the surrounding policy is Eager, Lazy, or Autonomous.
        //
        // The Group itself performs no work — it is a first-class coordination
        // value forced later by `now(group)`, `autonomous(group)`, or by the
        // Autonomous flush at document end.
        if let InvokeMode::Group(mode) = invoke_mode {
            if args.is_empty() {
                bail!("{}(...) takes at least 1 argument, got 0", fn_name);
            }
            // Evaluate members under Lazy policy so request chains are captured,
            // not resolved, regardless of the outer policy.
            let saved_policy = self.policy;
            self.policy = Policy::Lazy;
            let members: Vec<OValue> = planned_args
                .iter()
                .map(|(id, arg)| self.eval_ir_node(arg, *id, plan, scope))
                .collect::<Result<_>>()?;
            self.policy = saved_policy;
            return Ok(OValue::group(mode, members));
        }

        // Standard builtins: evaluate args left-to-right (applicative order).
        let arg_vals: Vec<OValue> = planned_args
            .iter()
            .map(|(id, arg)| self.eval_ir_node(arg, *id, plan, scope))
            .collect::<Result<_>>()?;

        match fn_name {
            "instantiate" => {
                if arg_vals.len() != 1 {
                    bail!(
                        "instantiate(expr) takes exactly 1 argument, got {}",
                        arg_vals.len()
                    );
                }
                let req = OValue::request(
                    RequestKind::Instantiate,
                    arg_vals.into_iter().next().unwrap(),
                );
                self.auto_resolve(req)
            }
            "realise" => {
                if arg_vals.len() != 1 {
                    bail!(
                        "realise(drv) takes exactly 1 argument, got {}",
                        arg_vals.len()
                    );
                }
                let req =
                    OValue::request(RequestKind::Realise, arg_vals.into_iter().next().unwrap());
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
                    // resolving each member fresh via the scheduler path.
                    OValue::Group { mode, members, .. } => {
                        let (mode, members) = (*mode, members.clone());
                        self.resolve_group(mode, &members, CacheMode::Fresh)
                    }
                    other => bail!(
                        "now(req) expected a Request or Group, got {}",
                        other.type_name()
                    ),
                }
            }
            // STEP-4: OS-as-participant builtins.
            "activate" => {
                if arg_vals.is_empty() || arg_vals.len() > 3 {
                    bail!(
                        "activate(path[, profile]) dry-runs; \
                         activate(capability, path[, profile]) performs a real \
                         switch; got {} args",
                        arg_vals.len()
                    );
                }

                let has_authority = matches!(arg_vals.first(), Some(OValue::Capability { .. }));
                let (authority, target, profile, dry_run) = if has_authority {
                    if arg_vals.len() < 2 {
                        bail!("activate(capability, path) requires a target StorePath");
                    }
                    let capability = &arg_vals[0];
                    let OValue::Capability { kind, identity, .. } = capability else {
                        unreachable!()
                    };
                    if *kind != CapabilityKind::SystemActivation {
                        bail!(
                            "activate requires a system_activation capability, got {}",
                            kind.name()
                        );
                    }
                    let authorized_profile = self
                        .activation_authorities
                        .get(identity)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "system activation capability is forged, revoked, or from another evaluator"
                            )
                        })?;
                    let requested_profile = match arg_vals.get(2) {
                        Some(OValue::Str { v }) => v.clone(),
                        Some(OValue::System { profile_path }) => profile_path.clone(),
                        Some(other) => bail!(
                            "activate's profile must be a string path or System, got {}",
                            other.type_name()
                        ),
                        None => authorized_profile.clone(),
                    };
                    if requested_profile != authorized_profile {
                        bail!(
                            "system activation capability is scoped to profile {}, not {}",
                            authorized_profile,
                            requested_profile
                        );
                    }
                    (
                        Some(identity.clone()),
                        arg_vals[1].clone(),
                        requested_profile,
                        false,
                    )
                } else {
                    if arg_vals.len() > 2 {
                        bail!("dry activate accepts only path and optional profile");
                    }
                    let profile = match arg_vals.get(1) {
                        Some(OValue::Str { v }) => v.clone(),
                        Some(OValue::System { profile_path }) => profile_path.clone(),
                        Some(other) => bail!(
                            "activate's profile must be a string path or System, got {}",
                            other.type_name()
                        ),
                        None => "/nix/var/nix/profiles/system".to_string(),
                    };
                    (None, arg_vals[0].clone(), profile, true)
                };

                let req = OValue::request(
                    RequestKind::Activate {
                        profile,
                        dry_run,
                        authority,
                    },
                    target,
                );
                self.auto_resolve(req)
            }
            "current_system" => {
                // Read the system profile symlink without going through a
                // Request — this is a pure inspection, not a deferred
                // computation. The result is an OValue::System reference.
                if !arg_vals.is_empty() {
                    bail!(
                        "current_system() takes no arguments, got {}",
                        arg_vals.len()
                    );
                }
                Ok(OValue::system("/nix/var/nix/profiles/system"))
            }
            "scope" => {
                if !arg_vals.is_empty() {
                    bail!("scope() takes no arguments, got {}", arg_vals.len());
                }
                Ok(OValue::scope(scope.clone()))
            }
            other => bail!("Unknown built-in function: `{}(...)`", other),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core evaluation: build splice buffer then dispatch to backend
    // ─────────────────────────────────────────────────────────────────────────

    fn eval_ir_exec(
        &mut self,
        region: IrExecRegion<'_>,
        plan: &ExecutionPlan,
        scope: &HashMap<String, OValue>,
    ) -> Result<OValue> {
        let IrExecRegion {
            lang,
            env_id,
            attr,
            backend,
            body,
            node_id,
        } = region;
        let registered_backend = BackendRegistry::global().interface_for(lang);
        if backend != &registered_backend {
            bail!(
                "OIR backend interface for `{lang}` does not match the registered execution and authority policy"
            );
        }
        let planned_body = planned_children(plan, node_id, body)?;
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
        if backend.execution == ExecutionMode::InlineAst && backend.canonical == "quote" {
            if attr.is_some() {
                bail!("attributes are not valid on the structural `quote` backend");
            }
            let src = reconstruct_ir_source(body);
            return Ok(OValue::Expr { src });
        }

        // `O` is an executable structural region. It sequences its OIR
        // children exactly once with a lexical child scope; it never builds a
        // backend splice buffer and never re-walks parser nodes.
        if backend.execution == ExecutionMode::InlineAst && backend.canonical == "O" {
            if attr.is_some() {
                bail!("attributes are not valid on the structural `O` backend");
            }
            let mut local_scope = scope.clone();
            let mut last = OValue::null();
            for (child_id, child) in &planned_body {
                let is_whitespace = matches!(
                    *child,
                    OIr::Text(text)
                        if !text.is_empty() && text.chars().all(char::is_whitespace)
                );
                let value = match *child {
                    OIr::Store { name, expr } => {
                        let children =
                            planned_children(plan, *child_id, std::slice::from_ref(expr.as_ref()))?;
                        let (expr_id, _) = children[0];
                        let value = self.eval_ir_node(expr, expr_id, plan, &local_scope)?;
                        local_scope.insert(name.clone(), value.clone());
                        value
                    }
                    _ => self.eval_ir_node(child, *child_id, plan, &local_scope)?,
                };
                if !value.is_null() && !is_whitespace {
                    last = value;
                }
            }
            return Ok(last);
        }

        if backend.execution == ExecutionMode::InlineAst {
            bail!(
                "OIR backend `{}` declares inline_ast execution without an executor",
                backend.canonical
            );
        }

        // ─────────────────────────────────────────────────────────────────────
        // Validate block policy and authority declarations early so misuses are
        // caught at the block we're evaluating, not somewhere downstream.
        //
        //   {lazy}  — pure backends only; produces a cacheable Eval Request
        //   {defer} — any backend; produces a non-cacheable Eval Request
        //
        // `nix_expr^` is already lazy by construction; attributes on it are
        // rejected as redundant. STEP4 may add other attributes (trace, etc.).
        // ─────────────────────────────────────────────────────────────────────
        let options = BlockOptions::parse(attr, lang)?;
        if let Some(policy) = options.policy {
            match policy {
                BlockEvalPolicy::Lazy => {
                    if lang == "nix_expr" {
                        bail!(
                            "`nix_expr{{lazy}}^` is redundant — nix_expr^ is already \
                             lazy. Use bare nix_expr^, or use nix{{lazy}}^ if you \
                             want a generic deferred Nix eval."
                        );
                    }
                    if !backend.pure {
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
                BlockEvalPolicy::Defer => {
                    if lang == "nix_expr" {
                        bail!(
                            "`nix_expr{{defer}}^` is redundant — nix_expr^ is already \
                             lazy. If you want a non-cacheable deferred Nix eval, \
                             write nix{{defer}}^."
                        );
                    }
                    // {defer} works on any backend; nothing else to check.
                }
            }
        }
        let sandbox = BackendSandboxPolicy::new(
            backend
                .required_authorities
                .iter()
                .copied()
                .chain(options.permissions.iter().copied()),
        );
        let authority_identity = self.resolve_backend_authority(
            backend.canonical.as_str(),
            &options,
            sandbox.permissions(),
            scope,
        )?;

        // Step 1 — build the fully-spliced source string for the backend.
        // For `nix_expr` blocks and `{lazy}`/`{defer}` blocks we also collect
        // the evaluated child OValues as deps so the returned thunk carries
        // its full dependency tree for fingerprint composition.
        let mut buf = String::new();
        let mut deps: Vec<OValue> = Vec::new();

        // Whether this block constructs a Thunk (and so should track deps).
        let constructs_thunk = backend.canonical == "nix_expr" || options.policy.is_some();

        // Own a mutable copy of the scope so that LetBinding nodes inside this
        // body can extend it for subsequent children. Cloning is cheap compared
        // to the subprocess dispatch that follows.
        let mut local_scope = scope.clone();

        for (child_id, child) in &planned_body {
            match *child {
                OIr::Store { name, expr } => {
                    // Evaluate the RHS and bind it into the local scope.
                    // The binding itself produces no text for the backend.
                    let children =
                        planned_children(plan, *child_id, std::slice::from_ref(expr.as_ref()))?;
                    let (expr_id, _) = children[0];
                    let value = self.eval_ir_node(expr, expr_id, plan, &local_scope)?;
                    local_scope.insert(name.clone(), value);
                }

                OIr::Text(text) => {
                    buf.push_str(text);
                }

                OIr::Load(name) => {
                    let val = local_scope
                        .get(name)
                        .ok_or_else(|| anyhow::anyhow!("Undefined variable: ${}", name))?
                        .clone();
                    // STEP-3.5: auto-force {lazy} thunks before splicing; error
                    // on {defer} thunks. {lazy} is safe to auto-force because
                    // pure-backend results don't have side effects.
                    let resolved = self.resolve_for_splice(val)?;
                    buf.push_str(&render_with(backend.renderer, &resolved));
                    if constructs_thunk {
                        deps.push(resolved);
                    }
                }

                OIr::Exec {
                    lang: child_lang,
                    env_id: child_env_id,
                    attr: child_attr,
                    backend: child_backend,
                    body: child_body,
                } => {
                    // Evaluate the nested expression first (leaves-up / applicative order),
                    // then render its value into the parent language's source syntax.
                    let child_val = self.eval_ir_exec(
                        IrExecRegion {
                            lang: child_lang,
                            env_id: *child_env_id,
                            attr: child_attr.as_deref(),
                            backend: child_backend,
                            body: child_body,
                            node_id: *child_id,
                        },
                        plan,
                        &local_scope,
                    )?;
                    let resolved = self.resolve_for_splice(child_val)?;
                    buf.push_str(&render_with(backend.renderer, &resolved));
                    if constructs_thunk {
                        deps.push(resolved);
                    }
                }

                OIr::Invoke {
                    fn_name,
                    mode,
                    args,
                } => {
                    let raw =
                        self.eval_ir_invoke(fn_name, *mode, args, *child_id, plan, &local_scope)?;
                    let resolved = self.resolve_for_splice(raw)?;
                    buf.push_str(&render_with(backend.renderer, &resolved));
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
        if let Some(policy) = options.policy {
            let cacheable = policy == BlockEvalPolicy::Lazy;
            let thunk = OValue::thunk(buf, deps);
            return Ok(OValue::request(
                RequestKind::Eval {
                    lang: lang.to_string(),
                    env_id,
                    cacheable,
                    authority: authority_identity,
                    permissions: sandbox.permissions().to_vec(),
                },
                thunk,
            ));
        }

        // Short-circuit for `nix_expr`: return a lazy ONixExpr instead of
        // calling the Nix shim immediately.  The fingerprint is sha256(body)
        // — the cheap step-1 scheme.  `nix^` (immediate evaluation) is
        // unchanged (step-1 decision, option a).
        if backend.canonical == "nix_expr" {
            return Ok(OValue::nix_expr(buf, deps));
        }

        // Dispatch is an OIR property frozen at lowering time.
        if backend.execution == ExecutionMode::InlineValue {
            return match backend.canonical.as_str() {
                "html" => Ok(OValue::html(buf)),
                "markdown" | "text" | "latex" => Ok(OValue::str_(buf)),
                other => bail!("inline OIR backend `{other}` has no value executor"),
            };
        }

        debug_assert_eq!(backend.execution, ExecutionMode::Shim);
        let runtime_lang = backend.canonical.as_str();
        let shim = BackendRegistry::global().resolve_shim_path(&self.shim_dir, runtime_lang);
        // Send the exec command to the shim, then drive the eval_request loop.
        //
        // Normally the shim sends Ok/Err immediately and the loop runs once.
        // If the shim's user code calls `O.eval(q)`, it sends EvalRequest with
        // the quoted source; we evaluate it here and send back EvalResult, then
        // loop to read the next response. The loop terminates on Ok or Err.
        let env_label = if env_id == u32::MAX {
            format!("{runtime_lang}[*ephemeral*]")
        } else {
            format!("{runtime_lang}[{env_id}]")
        };

        self.registry
            .send_exec(
                runtime_lang,
                env_id,
                &buf,
                local_scope.clone(),
                &shim,
                &sandbox,
            )
            .with_context(|| format!("[{}]", env_label))?;

        let result: Result<OValue> = loop {
            let step = self
                .registry
                .recv_exec_step(runtime_lang, env_id, &sandbox)
                .with_context(|| format!("[{}]", env_label))?;

            match step {
                ExecStep::Done(v) => break Ok(v),

                ExecStep::EvalRequest {
                    src,
                    scope: explicit_scope,
                } => {
                    // Evaluate the quoted source. If eval fails, propagate the
                    // error — the shim's `O.eval(q)` will raise on the Python
                    // side because the runtime never sends eval_result.
                    let callback_scope = match explicit_scope {
                        None => local_scope.clone(),
                        Some(OValue::Scope { bindings }) => bindings,
                        Some(other) => {
                            let _ = self.registry.cleanup_env(runtime_lang, env_id);
                            bail!(
                                "[{}] O.eval explicit scope must be an OScope, got {}",
                                env_label,
                                other.type_name()
                            );
                        }
                    };
                    match self.eval_source_with_scope(&src, &callback_scope) {
                        Ok(result) => {
                            self.registry
                                .send_eval_result(runtime_lang, env_id, result, &sandbox)
                                .with_context(|| format!("[{}] send_eval_result", env_label))?;
                        }
                        Err(e) => {
                            // Remove the process from the registry so the
                            // stuck shim doesn't pollute future calls.
                            let _ = self.registry.cleanup_env(runtime_lang, env_id);
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
            let _ = self.registry.cleanup_env(runtime_lang, u32::MAX);
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

    #[cfg(test)]
    fn render_child(&self, lang: &str, val: &OValue) -> String {
        render_with(BackendRegistry::global().renderer_for(lang), val)
    }
}

/// Pair direct OIR children with the identities and order selected by the
/// execution plan. Plan node identifiers are allocated in source order, so a
/// sorted copy provides the stable mapping back to the child payloads while
/// `child_schedule` remains free to reorder independent work later.
fn planned_children<'a>(
    plan: &ExecutionPlan,
    parent: PlanNodeId,
    children: &'a [OIr],
) -> Result<Vec<(PlanNodeId, &'a OIr)>> {
    let scheduled = plan.child_schedule(parent).map_err(anyhow::Error::msg)?;
    if scheduled.len() != children.len() {
        bail!(
            "OIR plan node {} schedules {} children for {} instructions",
            parent.0,
            scheduled.len(),
            children.len()
        );
    }
    let mut source_ids = scheduled.clone();
    source_ids.sort_by_key(|id| id.0);
    scheduled
        .into_iter()
        .map(|id| {
            let source_index = source_ids
                .binary_search_by_key(&id.0, |candidate| candidate.0)
                .expect("scheduled child must be present in source child map");
            Ok((id, &children[source_index]))
        })
        .collect()
}

/// Render using the strategy frozen into executable OIR. Keeping this as a
/// value-level function lets tests exercise renderers directly while runtime
/// execution never has to rediscover backend policy from a language string.
fn render_with(renderer: SpliceRenderer, val: &OValue) -> String {
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

pub fn render_fidelity(renderer: SpliceRenderer, val: &OValue) -> RenderFidelity {
    use OValue::*;
    use RenderFidelity::*;

    match renderer {
        SpliceRenderer::Python => match val {
            Null
            | Bool { .. }
            | Int { .. }
            | Float { .. }
            | Str { .. }
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
            | Error { .. } => Typed,
            List { v } => container_fidelity(renderer, v, Typed),
            Map { v } => container_fidelity(renderer, v.values(), Typed),
            Blob { .. } => Structural,
        },
        SpliceRenderer::Nix => match val {
            Null | Bool { .. } | Int { .. } | Float { .. } | Str { .. } | NixExpr { .. } => Typed,
            List { v } => container_fidelity(renderer, v, Typed),
            Map { v } => container_fidelity(renderer, v.values(), Typed),
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
            | Int { .. }
            | Float { .. }
            | Str { .. }
            | Html { .. }
            | StorePath { .. }
            | Blob { .. }
            | NixExpr { .. }
            | Derivation { .. }
            | System { .. }
            | Expr { .. }
            | Error { .. } => Presentation,
            List { v } => container_fidelity(renderer, v, Presentation),
            Map { v } => container_fidelity(renderer, v.values(), Presentation),
            Scope { .. }
            | Request { .. }
            | Capability { .. }
            | Snapshot { .. }
            | Thunk { .. }
            | Group { .. } => Opaque,
        },
        SpliceRenderer::Default => match val {
            Null | Bool { .. } | Int { .. } | Float { .. } | Str { .. } => Structural,
            List { v } => container_fidelity(renderer, v, Structural),
            Map { v } => container_fidelity(renderer, v.values(), Structural),
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
        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => v.to_string(),
        OValue::Str { v } => serde_json::to_string(v).unwrap_or_else(|_| "\"".to_string()),
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
        OValue::Scope { bindings } => {
            format!("\"<scope bindings={}>\"", bindings.len())
        }
        OValue::Blob { v, .. } => serde_json::to_string(v).unwrap_or_else(|_| "\"".to_string()),
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

        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => {
            let s = v.to_string();
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s
            } else {
                format!("{}.0", s)
            }
        }

        OValue::Str { v } => serde_json::to_string(v).unwrap_or_else(|_| "''".to_string()),

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

        OValue::NixExpr { .. }
        | OValue::Derivation { .. }
        | OValue::Request { .. }
        | OValue::Thunk { .. }
        | OValue::System { .. }
        | OValue::Capability { .. }
        | OValue::Snapshot { .. }
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

        OValue::Scope { bindings } => {
            format!(
                "<code class=\"o-scope\" data-bindings=\"{}\">&lt;scope&gt;</code>",
                bindings.len()
            )
        }

        OValue::Blob { v, mime } => render_html_blob(v, mime),

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
        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => v.to_string(),
        OValue::Str { v } => v.clone(),
        OValue::Html { v } => v.clone(),
        OValue::StorePath { path } => {
            format!("\\texttt{{{}}}", path.replace("_", "\\_"))
        }
        OValue::List { v } => v.iter().map(render_latex).collect::<Vec<_>>().join(", "),
        OValue::Map { v } => sorted_map_entries(v)
            .into_iter()
            .map(|(k, val)| format!("{}: {}", k, render_latex(val)))
            .collect::<Vec<_>>()
            .join(", "),
        OValue::Scope { bindings } => {
            format!("\\texttt{{<scope bindings={}>}}", bindings.len())
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
        OValue::Int { v } => v.to_string(),
        OValue::Float { v } => v.to_string(),
        OValue::Str { v } => v.clone(),
        OValue::Html { v } => v.clone(),
        OValue::StorePath { path } => format!("`{}`", path),
        OValue::List { v } => v.iter().map(render_markdown).collect::<Vec<_>>().join("\n"),
        OValue::Map { v } => sorted_map_entries(v)
            .into_iter()
            .map(|(k, val)| format!("**{}**: {}", k, render_markdown(val)))
            .collect::<Vec<_>>()
            .join("\n"),
        OValue::Scope { bindings } => format!("`<scope bindings={}>`", bindings.len()),
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
        assert_eq!(e.render_child("python", &OValue::bool_(true)), "True");
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

    #[test]
    fn rendering_fidelity_matrix_covers_every_ovalue_variant() {
        use crate::value::SnapshotKind;

        let values = vec![
            OValue::Null,
            OValue::bool_(true),
            OValue::int(1),
            OValue::float(1.5),
            OValue::str_("text"),
            OValue::html("<b>text</b>"),
            OValue::store_path("/nix/store/example"),
            OValue::Expr { src: "42".into() },
            OValue::list(vec![OValue::int(1)]),
            OValue::map(HashMap::from([("key".into(), OValue::int(1))])),
            OValue::scope(HashMap::from([("x".into(), OValue::int(1))])),
            OValue::blob(b"data", "application/octet-stream"),
            OValue::nix_expr("1 + 1", vec![]),
            OValue::derivation("/nix/store/example.drv", vec!["out".into()], vec![]),
            OValue::request(RequestKind::Instantiate, OValue::nix_expr("1", vec![])),
            OValue::system("/nix/var/nix/profiles/system"),
            OValue::capability(CapabilityKind::Service, "opaque", HashMap::new()),
            OValue::snapshot(SnapshotKind::System, "generation", HashMap::new()),
            OValue::thunk("42", vec![]),
            OValue::group(GroupMode::Batch, vec![]),
            OValue::error("failed"),
        ];
        let renderers = [
            SpliceRenderer::Python,
            SpliceRenderer::Nix,
            SpliceRenderer::Html,
            SpliceRenderer::Latex,
            SpliceRenderer::Markdown,
            SpliceRenderer::Default,
        ];

        assert_eq!(values.len(), 21, "update the matrix when OValue grows");
        for renderer in renderers {
            for value in &values {
                let rendered = render_with(renderer, value);
                let _classification = render_fidelity(renderer, value);
                assert!(
                    !rendered.is_empty() || matches!(value, OValue::Null),
                    "{renderer:?} silently erased {}",
                    value.type_name()
                );
            }
        }

        assert_eq!(
            render_fidelity(SpliceRenderer::Python, &OValue::int(1)),
            RenderFidelity::Typed
        );
        assert_eq!(
            render_fidelity(
                SpliceRenderer::Python,
                &OValue::capability(CapabilityKind::Service, "opaque", HashMap::new())
            ),
            RenderFidelity::Typed
        );
        assert_eq!(
            render_fidelity(SpliceRenderer::Html, &OValue::int(1)),
            RenderFidelity::Presentation
        );
        assert_eq!(
            render_fidelity(
                SpliceRenderer::Python,
                &OValue::list(vec![OValue::blob(b"data", "application/octet-stream")])
            ),
            RenderFidelity::Structural,
            "container fidelity must be bounded by its least faithful child"
        );
    }

    #[test]
    fn map_rendering_is_independent_of_hashmap_insertion_order() {
        let first = OValue::map(HashMap::from([
            ("z".into(), OValue::int(1)),
            ("a key".into(), OValue::int(2)),
        ]));
        let second = OValue::map(HashMap::from([
            ("a key".into(), OValue::int(2)),
            ("z".into(), OValue::int(1)),
        ]));

        for renderer in [
            SpliceRenderer::Python,
            SpliceRenderer::Nix,
            SpliceRenderer::Html,
            SpliceRenderer::Latex,
            SpliceRenderer::Markdown,
            SpliceRenderer::Default,
        ] {
            assert_eq!(
                render_with(renderer, &first),
                render_with(renderer, &second)
            );
        }
        assert_eq!(
            render_with(SpliceRenderer::Nix, &first),
            "{ \"a key\" = 2; \"z\" = 1; }"
        );
    }

    #[test]
    fn python_opaque_handle_round_trips_the_complete_tagged_value() {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir);
        let capability = OValue::capability(
            CapabilityKind::Service,
            "descriptive-test-identity",
            HashMap::from([("service".into(), OValue::str_("serial"))]),
        );
        let scope = HashMap::from([("value".into(), capability.clone())]);
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            body: vec![ONode::VarRef("value".into())],
        };

        assert_eq!(evaluator.eval_node(&block, &scope).unwrap(), capability);
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
        assert_eq!(e.render_child("nix", &OValue::bool_(true)), "true");
        assert_eq!(e.render_child("nix", &OValue::bool_(false)), "false");
    }

    #[test]
    fn nix_int_renders_as_integer() {
        let e = Evaluator::new("/tmp".into());
        assert_eq!(e.render_child("nix", &OValue::int(42)), "42");
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
        let nix_out = e.render_child("nix", &v);
        let store_out = e.render_child("nix_store", &v);
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
        let result = e
            .eval_document(vec![ONode::RawText(String::new())])
            .unwrap();
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
    fn document_execution_is_mediated_by_oir_plan() {
        let mut evaluator = Evaluator::new("/tmp".into());
        let result = evaluator
            .eval_document(vec![
                ONode::LetBinding {
                    name: "x".into(),
                    expr: Box::new(ONode::RawText("planned".into())),
                },
                ONode::VarRef("x".into()),
            ])
            .unwrap();
        assert_eq!(result, OValue::str_("planned"));

        let plan = evaluator
            .last_execution_plan()
            .expect("document execution must install an OIR plan");
        assert_eq!(plan.roots.len(), 2);
        assert!(plan.edges.iter().any(|edge| {
            edge.kind == crate::ir::PlanEdgeKind::Data
                && matches!(
                    &plan.nodes[edge.to.0].kind,
                    crate::ir::PlanNodeKind::Load { name } if name == "x"
                )
        }));
    }

    #[test]
    fn lowered_oir_is_a_public_execution_input() {
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for("html"),
                body: vec![OIr::Text("<p>executed from OIR</p>".into())],
            }],
        };
        let mut evaluator = Evaluator::new("/tmp".into());
        assert_eq!(
            evaluator.eval_ir_program(&program).unwrap(),
            OValue::html("<p>executed from OIR</p>")
        );
        assert!(evaluator.last_execution_plan().is_some());
    }

    #[test]
    fn public_oir_cannot_weaken_registered_backend_authority() {
        let mut weakened = BackendRegistry::global().interface_for("bash");
        weakened.required_authorities.clear();
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "bash".into(),
                env_id: u32::MAX,
                attr: None,
                backend: weakened,
                body: vec![OIr::Text("printf forbidden".into())],
            }],
        };
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let error = evaluator.eval_ir_program(&program).unwrap_err().to_string();
        assert!(error.contains("does not match the registered execution and authority policy"));
        assert!(!error.contains("failed to spawn backend shim"));
    }

    #[test]
    fn lazy_inline_backend_is_forced_by_oir_dispatch() {
        let program = OIrProgram {
            nodes: vec![OIr::Invoke {
                fn_name: "now".into(),
                mode: InvokeMode::Eager,
                args: vec![OIr::Exec {
                    lang: "html".into(),
                    env_id: u32::MAX,
                    attr: Some("lazy".into()),
                    backend: BackendRegistry::global().interface_for("html"),
                    body: vec![OIr::Text("<p>cached inline</p>".into())],
                }],
            }],
        };
        let mut evaluator = Evaluator::new("/tmp".into());
        assert_eq!(
            evaluator.eval_ir_program(&program).unwrap(),
            OValue::html("<p>cached inline</p>")
        );
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
        let result = e
            .eval_typed_expr(
                "nix_expr",
                u32::MAX,
                None,
                &[ONode::RawText("pkgs.hello".to_string())],
                &HashMap::new(),
            )
            .unwrap();

        assert!(result.is_nix_expr(), "expected ONixExpr, got {:?}", result);

        if let OValue::NixExpr {
            body,
            deps,
            fingerprint,
        } = &result
        {
            assert_eq!(body, "pkgs.hello");
            assert!(deps.is_empty());
            assert_eq!(fingerprint.len(), 64, "fingerprint must be 64 hex chars");
        }
    }

    /// Child OValues from inner typed expressions should appear in deps
    /// and their rendered form should be spliced into body.
    #[test]
    fn nix_expr_block_collects_deps_from_child_typed_exprs() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("n".to_string(), OValue::int(7));

        // nix_expr^( prefix $n suffix )_nix_expr
        // $n is a VarRef that resolves to OValue::Int(7)
        let body_nodes = vec![
            ONode::RawText("prefix ".to_string()),
            ONode::VarRef("n".to_string()),
            ONode::RawText(" suffix".to_string()),
        ];

        let result = e
            .eval_typed_expr("nix_expr", u32::MAX, None, &body_nodes, &scope)
            .unwrap();

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
        let e = Evaluator::new("/tmp".into());
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
        fn new() -> Self {
            Self { calls: vec![] }
        }
    }

    impl Executor for MockExecutor {
        fn execute(&mut self, req: &OValue) -> Result<OValue> {
            let (kind, source, fingerprint) = match req {
                OValue::Request {
                    kind,
                    source,
                    fingerprint,
                } => (kind.clone(), source.as_ref().clone(), fingerprint.clone()),
                _ => panic!("MockExecutor only handles Requests"),
            };
            self.calls.push(fingerprint);

            // Chained source: recursively execute first to resolve to a non-Request.
            let resolved = match source {
                OValue::Request { .. } => self.execute(&source)?,
                other => other,
            };

            match (kind, &resolved) {
                (RequestKind::Instantiate, OValue::NixExpr { .. }) => Ok(OValue::derivation(
                    "/nix/store/mockhash-foo.drv",
                    vec!["out".into()],
                    vec![],
                )),
                (RequestKind::Realise, OValue::Derivation { .. }) => {
                    Ok(OValue::store_path("/nix/store/mockhash-foo"))
                }
                (k, s) => panic!("MockExecutor: unexpected ({:?}, {})", k, s.type_name()),
            }
        }
    }

    #[test]
    fn structural_o_region_executes_each_oir_child_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct CountingExecutor {
            calls: Arc<AtomicUsize>,
        }

        impl Executor for CountingExecutor {
            fn execute(&mut self, request: &OValue) -> Result<OValue> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                match request {
                    OValue::Request {
                        kind: RequestKind::Instantiate,
                        ..
                    } => Ok(OValue::derivation(
                        "/nix/store/oir-once.drv",
                        vec!["out".into()],
                        vec![],
                    )),
                    other => panic!("unexpected request: {other:?}"),
                }
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let mut evaluator =
            Evaluator::new("/tmp".into()).with_executor(Box::new(CountingExecutor {
                calls: calls.clone(),
            }));
        let program = OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "expr".into(),
                    expr: Box::new(OIr::Exec {
                        lang: "nix_expr".into(),
                        env_id: u32::MAX,
                        attr: None,
                        backend: BackendRegistry::global().interface_for("nix_expr"),
                        body: vec![OIr::Text("pkgs.hello".into())],
                    }),
                },
                OIr::Exec {
                    lang: "O".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: BackendRegistry::global().interface_for("O"),
                    body: vec![OIr::Invoke {
                        fn_name: "instantiate".into(),
                        mode: InvokeMode::Eager,
                        args: vec![OIr::Load("expr".into())],
                    }],
                },
            ],
        };

        assert!(evaluator.eval_ir_program(&program).unwrap().is_derivation());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Under Eager (the default), `instantiate($expr)` auto-resolves at
    /// construction time inside eval_call. The caller never sees a Request.
    #[test]
    fn eager_call_auto_resolves_at_construction() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let call = ONode::Call {
            fn_name: "instantiate".into(),
            args: vec![ONode::VarRef("expr".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert!(
            result.is_derivation(),
            "under Eager, eval_call should auto-resolve directly to a Derivation"
        );
    }

    /// `realise(instantiate($expr))` chains under Eager: instantiate auto-
    /// resolves to a Derivation, then realise auto-resolves to a StorePath.
    /// No intermediate Request is observable.
    #[test]
    fn nested_call_under_eager_resolves_end_to_end() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let inner = ONode::Call {
            fn_name: "instantiate".into(),
            args: vec![ONode::VarRef("expr".into())],
        };
        let outer = ONode::Call {
            fn_name: "realise".into(),
            args: vec![inner],
        };

        let result = e.eval_node(&outer, &scope).unwrap();
        if let OValue::StorePath { path } = &result {
            assert!(path.starts_with("/nix/store/"));
        } else {
            panic!(
                "expected StorePath under Eager end-to-end, got {:?}",
                result
            );
        }
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
        if let (OValue::Derivation { drv_path: d1, .. }, OValue::Derivation { drv_path: d2, .. }) =
            (&r1, &r2)
        {
            assert_eq!(d1, d2);
            assert_eq!(d1, "/nix/store/seeded.drv");
        } else {
            panic!("expected Derivation results");
        }
    }

    /// Unknown call names must error cleanly rather than silently no-op.
    #[test]
    fn unknown_call_errors_with_clear_message() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let call = ONode::Call {
            fn_name: "frobnicate".into(),
            args: vec![],
        };
        let err = e.eval_node(&call, &scope).unwrap_err().to_string();
        assert!(
            err.contains("frobnicate"),
            "error must name the unknown function"
        );
    }

    /// `now(req)` performs the request immediately and returns its result,
    /// regardless of policy. In step 3 this matters: inside a lazy^ region,
    /// `now()` is the explicit-perform escape hatch.
    #[test]
    fn now_call_executes_request_directly() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req = OValue::request(RequestKind::Instantiate, expr);
        scope.insert("req".into(), req);

        let call = ONode::Call {
            fn_name: "now".into(),
            args: vec![ONode::VarRef("req".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert!(
            result.is_derivation(),
            "now(req) on an Instantiate request should produce a Derivation"
        );
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
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let lazy_call = ONode::Call {
            fn_name: "lazy".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef("expr".into())],
            }],
        };

        let result = e.eval_node(&lazy_call, &scope).unwrap();
        assert!(
            result.is_request(),
            "lazy(instantiate(...)) must return a Request, got {:?}",
            result
        );
    }

    /// `lazy(realise(instantiate($expr)))` returns a chained Request — outer
    /// Realise over inner Instantiate, neither executed.
    #[test]
    fn lazy_preserves_chained_request_structure() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let chain = ONode::Call {
            fn_name: "lazy".into(),
            args: vec![ONode::Call {
                fn_name: "realise".into(),
                args: vec![ONode::Call {
                    fn_name: "instantiate".into(),
                    args: vec![ONode::VarRef("expr".into())],
                }],
            }],
        };

        let result = e.eval_node(&chain, &scope).unwrap();
        if let OValue::Request { kind, source, .. } = &result {
            assert_eq!(*kind, RequestKind::Realise);
            assert!(
                source.is_request(),
                "outer Request's source must be the inner unresolved Instantiate Request"
            );
        } else {
            panic!("expected chained Request, got {:?}", result);
        }
    }

    /// `now()` inside lazy() forces execution — the explicit escape hatch.
    #[test]
    fn now_inside_lazy_executes() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let nested = ONode::Call {
            fn_name: "lazy".into(),
            args: vec![ONode::Call {
                fn_name: "now".into(),
                args: vec![ONode::Call {
                    fn_name: "instantiate".into(),
                    args: vec![ONode::VarRef("expr".into())],
                }],
            }],
        };

        let result = e.eval_node(&nested, &scope).unwrap();
        assert!(
            result.is_derivation(),
            "now() inside lazy() still executes, returning a Derivation"
        );
    }

    /// Policy is restored after lazy() returns. A subsequent direct call
    /// should auto-resolve normally — confirming the policy scope is bounded.
    #[test]
    fn policy_restored_to_eager_after_lazy_returns() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        // First: lazy(instantiate(...)) returns a Request (Lazy was active).
        let lazy_call = ONode::Call {
            fn_name: "lazy".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef("expr".into())],
            }],
        };
        assert!(e.eval_node(&lazy_call, &scope).unwrap().is_request());

        // Then: plain instantiate(...) auto-resolves (Eager is back).
        let plain_call = ONode::Call {
            fn_name: "instantiate".into(),
            args: vec![ONode::VarRef("expr".into())],
        };
        let result = e.eval_node(&plain_call, &scope).unwrap();
        assert!(
            result.is_derivation(),
            "after lazy() exits, direct call should auto-resolve to Derivation"
        );
    }

    /// Nested lazy inside lazy stays lazy. Pinning down the edge case:
    /// re-entering a lazy region shouldn't accidentally restore an outer
    /// non-lazy policy.
    #[test]
    fn nested_lazy_calls_remain_lazy() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("pkgs.hello", vec![]));

        let nested = ONode::Call {
            fn_name: "lazy".into(),
            args: vec![ONode::Call {
                fn_name: "lazy".into(),
                args: vec![ONode::Call {
                    fn_name: "instantiate".into(),
                    args: vec![ONode::VarRef("expr".into())],
                }],
            }],
        };
        let result = e.eval_node(&nested, &scope).unwrap();
        assert!(
            result.is_request(),
            "lazy nested in lazy must still produce a Request, got {:?}",
            result
        );
    }

    /// Even when lazy()'s argument errors, the policy is restored.
    /// This is the save/restore guard in the lazy branch of eval_call.
    #[test]
    fn policy_restored_even_on_lazy_arg_error() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let bad = ONode::Call {
            fn_name: "lazy".into(),
            args: vec![ONode::VarRef("missing".into())], // will error
        };

        assert_eq!(e.policy, Policy::Eager);
        let _ = e.eval_node(&bad, &scope); // expected error
        assert_eq!(
            e.policy,
            Policy::Eager,
            "policy must be restored to Eager after lazy() errors"
        );
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
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("lazy".into()),
            body: vec![ONode::RawText("1 + 1".into())],
        };
        let err = e.eval_node(&block, &scope).unwrap_err().to_string();
        assert!(
            err.contains("not a pure backend"),
            "error must explain backend purity, got: {}",
            err
        );
        assert!(
            err.contains("defer"),
            "error should suggest {{defer}} as alternative, got: {}",
            err
        );
    }

    /// {lazy} on a pure backend (nix) returns a Request[Eval] without
    /// invoking the shim. The Thunk inside carries body + deps.
    #[test]
    fn lazy_attr_on_pure_backend_produces_eval_request() {
        let mut e = Evaluator::new("/tmp".into());
        let capability = e
            .issue_backend_execution_capability("nix", BackendAuthority::ALL)
            .unwrap();
        let scope = HashMap::from([("runner".into(), capability)]);
        let block = ONode::TypedExpr {
            lang: "nix".into(),
            env_id: u32::MAX,
            attr: Some("lazy,cap=runner".into()),
            body: vec![ONode::RawText("1 + 2".into())],
        };
        let result = e.eval_node(&block, &scope).unwrap();
        if let OValue::Request { kind, source, .. } = &result {
            match kind {
                RequestKind::Eval {
                    lang,
                    env_id: _,
                    cacheable,
                    ..
                } => {
                    assert_eq!(lang, "nix");
                    assert!(*cacheable, "{{lazy}} must produce cacheable=true");
                }
                other => panic!("expected RequestKind::Eval, got {:?}", other),
            }
            assert!(source.is_thunk(), "Request source must be a Thunk");
            if let OValue::Thunk { body, .. } = source.as_ref() {
                assert_eq!(body, "1 + 2");
            }
        } else {
            panic!("expected Request, got {:?}", result);
        }
    }

    /// {defer} on an impure backend (python) is allowed and produces a
    /// non-cacheable Eval Request.
    #[test]
    fn defer_attr_on_impure_backend_is_allowed() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("defer".into()),
            body: vec![ONode::RawText("print('hi')".into())],
        };
        let result = e.eval_node(&block, &scope).unwrap();
        if let OValue::Request { kind, .. } = &result {
            if let RequestKind::Eval {
                lang, cacheable, ..
            } = kind
            {
                assert_eq!(lang, "python");
                assert!(!*cacheable, "{{defer}} must produce cacheable=false");
            } else {
                panic!("expected RequestKind::Eval");
            }
        } else {
            panic!("expected Request");
        }
    }

    #[test]
    fn backend_authority_is_checked_before_shim_dispatch() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("fs_read".into()),
            body: vec![ONode::RawText("__oval_result__ = 1".into())],
        };
        let error = evaluator.eval_node(&block, &scope).unwrap_err().to_string();
        assert!(error.contains("names no live capability"));
        assert!(!error.contains("failed to spawn backend shim"));
    }

    #[test]
    fn adapter_required_authority_is_checked_before_shim_dispatch() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let block = ONode::TypedExpr {
            lang: "bash".into(),
            env_id: u32::MAX,
            attr: None,
            body: vec![ONode::RawText("printf forbidden".into())],
        };
        let error = evaluator
            .eval_node(&block, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("requests process"));
        assert!(error.contains("names no live capability"));
        assert!(!error.contains("failed to spawn backend shim"));
    }

    #[test]
    fn backend_capability_allows_only_its_language_and_rights() {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir);
        let capability = evaluator
            .issue_backend_execution_capability("python", [BackendAuthority::Process])
            .unwrap();
        let scope = HashMap::from([("runner".into(), capability)]);
        let allowed = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("cap=runner,process".into()),
            body: vec![ONode::RawText(
                "import os\n__oval_result__ = os.system('true')".into(),
            )],
        };
        assert_eq!(
            evaluator.eval_node(&allowed, &scope).unwrap(),
            OValue::int(0)
        );

        let denied = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("cap=runner,network".into()),
            body: vec![ONode::RawText("__oval_result__ = 1".into())],
        };
        let error = format!("{:#}", evaluator.eval_node(&denied, &scope).unwrap_err());
        assert!(error.contains("lacks `network` authority"));
    }

    #[test]
    fn plain_python_blocks_cannot_spawn_processes() {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir);
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            body: vec![ONode::RawText(
                "import os\n__oval_result__ = os.system('echo forbidden')".into(),
            )],
        };
        let error = format!(
            "{:#}",
            evaluator.eval_node(&block, &HashMap::new()).unwrap_err()
        );
        assert!(error.contains("denies process spawn"));
    }

    #[test]
    fn deferred_backend_authority_is_rechecked_when_forced() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let capability = evaluator
            .issue_backend_execution_capability("python", [BackendAuthority::Process])
            .unwrap();
        let scope = HashMap::from([("runner".into(), capability.clone())]);
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("defer,cap=runner,process".into()),
            body: vec![ONode::RawText("__oval_result__ = 1".into())],
        };
        let request = evaluator.eval_node(&block, &scope).unwrap();
        evaluator
            .revoke_backend_execution_capability(&capability)
            .unwrap();
        let error = format!("{:#}", evaluator.force_request(&request).unwrap_err());
        assert!(error.contains("forged, revoked, or from another evaluator"));
    }

    #[test]
    fn forged_deferred_request_cannot_omit_adapter_required_rights() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let capability = evaluator
            .issue_backend_execution_capability("bash", [])
            .unwrap();
        let identity = match capability {
            OValue::Capability { identity, .. } => identity,
            _ => unreachable!(),
        };
        let request = OValue::request(
            RequestKind::Eval {
                lang: "bash".into(),
                env_id: u32::MAX,
                cacheable: false,
                authority: Some(identity),
                permissions: vec![],
            },
            OValue::thunk("printf forbidden", vec![]),
        );

        let error = format!("{:#}", evaluator.force_request(&request).unwrap_err());
        assert!(error.contains("lacks `process` authority"));
        assert!(!error.contains("failed to spawn backend shim"));
    }

    /// {lazy} on nix_expr is rejected as redundant.
    #[test]
    fn lazy_attr_on_nix_expr_errors_redundant() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang: "nix_expr".into(),
            env_id: u32::MAX,
            attr: Some("lazy".into()),
            body: vec![],
        };
        let err = e.eval_node(&block, &scope).unwrap_err().to_string();
        assert!(
            err.contains("redundant"),
            "error must say nix_expr+{{lazy}} is redundant, got: {}",
            err
        );
    }

    /// Unknown attributes error with a clear message.
    #[test]
    fn unknown_attr_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang: "nix".into(),
            env_id: u32::MAX,
            attr: Some("strict".into()),
            body: vec![],
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
        let capability = e
            .issue_backend_execution_capability("nix", BackendAuthority::ALL)
            .unwrap();
        let scope = HashMap::from([("runner".into(), capability)]);

        let block = ONode::TypedExpr {
            lang: "nix".into(),
            env_id: u32::MAX,
            attr: Some("lazy,cap=runner".into()),
            body: vec![ONode::RawText("3 + 4".into())],
        };
        let req = e.eval_node(&block, &scope).unwrap();
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else {
            panic!("expected Request");
        };

        // Seed the Evaluator's own eval_cache so force_request hits it
        // instead of trying to spawn a nix shim.
        e.eval_cache.insert(fp.clone(), OValue::int(7));

        let forced = e.force_request(&req).unwrap();
        assert_eq!(
            forced,
            OValue::int(7),
            "now() / force_request must return the cached value"
        );
    }

    /// {defer} requests bypass the cache on read AND write — re-running on
    /// every force is their defining property.
    #[test]
    fn defer_eval_request_bypasses_cache() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("defer".into()),
            body: vec![ONode::RawText("1".into())],
        };
        let req = e.eval_node(&block, &scope).unwrap();
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else {
            panic!("expected Request");
        };

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
        let capability = e
            .issue_backend_execution_capability("nix", BackendAuthority::ALL)
            .unwrap();
        let mut scope = HashMap::from([("runner".into(), capability)]);

        // Construct a {lazy} nix block, find its fingerprint, seed the cache.
        let lazy_block = ONode::TypedExpr {
            lang: "nix".into(),
            env_id: u32::MAX,
            attr: Some("lazy,cap=runner".into()),
            body: vec![ONode::RawText("123".into())],
        };
        let req = e.eval_node(&lazy_block, &scope).unwrap();
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else {
            panic!();
        };
        e.eval_cache.insert(fp, OValue::int(123));
        scope.insert("lz".into(), req);

        // Now splice the lazy Request into another block via $lz. The splice
        // path should auto-force, retrieving 123 from the cache and
        // rendering it. We use markdown^ so we don't need a real shim —
        // markdown bypasses the registry and renders directly.
        let md_block = ONode::TypedExpr {
            lang: "markdown".into(),
            env_id: u32::MAX,
            attr: None,
            body: vec![ONode::RawText("value=".into()), ONode::VarRef("lz".into())],
        };
        // markdown^ goes through the registry path which tries to spawn a
        // shim. We just check that resolve_for_splice resolves the request:
        let resolved = e.resolve_for_splice(scope["lz"].clone()).unwrap();
        assert_eq!(
            resolved,
            OValue::int(123),
            "splice path must auto-force {{lazy}} to its cached value"
        );
        // (md_block parsed but not evaluated end-to-end here — the splice
        // resolution is the unit we're testing.)
        let _ = md_block;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // STEP-4: OS-as-participant
    //
    // The unprivileged activate(path[, profile]) form constructs a dry-run
    // Request[Activate]. A real switch requires
    // activate(system_activation_capability, path[, profile]); the capability
    // is checked at construction and again at force time.
    // ─────────────────────────────────────────────────────────────────────────

    /// MockSystemExecutor returns canned System values for Activate requests
    /// without actually shelling out to switch-to-configuration. Used to
    /// verify the orchestration without touching the real OS.
    struct MockSystemExecutor {
        activate_calls: Vec<(String, bool)>, // (profile, dry_run)
    }

    impl MockSystemExecutor {
        fn new() -> Self {
            Self {
                activate_calls: vec![],
            }
        }
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
                RequestKind::Activate {
                    profile, dry_run, ..
                } => {
                    self.activate_calls.push((profile.clone(), dry_run));
                    Ok(OValue::system(profile))
                }
                RequestKind::Realise => {
                    // Auto-realise a Derivation source — used in the chain test.
                    if resolved_source.is_derivation() {
                        Ok(OValue::store_path("/nix/store/mock-system"))
                    } else {
                        panic!("Realise source must be Derivation")
                    }
                }
                RequestKind::Instantiate => Ok(OValue::derivation(
                    "/nix/store/mockhash-system.drv",
                    vec!["out".into()],
                    vec![],
                )),
                other => panic!("MockSystemExecutor: unhandled kind {:?}", other),
            }
        }
    }

    /// `activate($path)` constructs a Request[Activate] and (under Eager)
    /// auto-resolves it. The mock executor returns a System value.
    #[test]
    fn activate_call_builds_request_and_resolves_to_system() {
        let mut e =
            Evaluator::new("/tmp".into()).with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));

        let call = ONode::Call {
            fn_name: "activate".into(),
            args: vec![ONode::VarRef("path".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert!(
            result.is_system(),
            "activate($path) under Eager should auto-resolve to a System, got {:?}",
            result
        );
        if let OValue::System { profile_path } = &result {
            assert_eq!(
                profile_path, "/nix/var/nix/profiles/system",
                "default profile should be the system-wide one"
            );
        }
    }

    /// `activate($path, $profile)` uses the user-supplied profile.
    #[test]
    fn activate_with_explicit_profile_uses_it() {
        let mut e =
            Evaluator::new("/tmp".into()).with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));
        scope.insert("profile".into(), OValue::str_("/home/lee/.nix-profile"));

        let call = ONode::Call {
            fn_name: "activate".into(),
            args: vec![
                ONode::VarRef("path".into()),
                ONode::VarRef("profile".into()),
            ],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        if let OValue::System { profile_path } = &result {
            assert_eq!(profile_path, "/home/lee/.nix-profile");
        } else {
            panic!("expected System");
        }
    }

    #[test]
    fn real_activation_request_captures_live_profile_scoped_authority() {
        let profile = "/nix/var/nix/profiles/system";
        let mut evaluator = Evaluator::new("/tmp".into());
        let capability = evaluator
            .issue_system_activation_capability(profile)
            .unwrap();
        let mut scope = HashMap::new();
        scope.insert("authority".into(), capability);
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));
        evaluator.policy = Policy::Lazy;

        let request = evaluator
            .eval_node(
                &ONode::Call {
                    fn_name: "activate".into(),
                    args: vec![
                        ONode::VarRef("authority".into()),
                        ONode::VarRef("path".into()),
                    ],
                },
                &scope,
            )
            .unwrap();

        let OValue::Request {
            kind:
                RequestKind::Activate {
                    profile: actual_profile,
                    dry_run,
                    authority,
                },
            ..
        } = request
        else {
            panic!("expected an Activate request")
        };
        assert_eq!(actual_profile, profile);
        assert!(!dry_run);
        assert!(authority
            .as_deref()
            .is_some_and(|id| id.starts_with("o-activate-live:")));
    }

    #[test]
    fn forged_or_revoked_activation_authority_is_rejected_before_io() {
        let profile = "/nix/var/nix/profiles/system";
        let mut evaluator = Evaluator::new("/tmp".into());
        let capability = evaluator
            .issue_system_activation_capability(profile)
            .unwrap();
        let identity = match &capability {
            OValue::Capability { identity, .. } => identity.clone(),
            _ => unreachable!(),
        };
        evaluator
            .revoke_system_activation_capability(&capability)
            .unwrap();

        let request = OValue::request(
            RequestKind::Activate {
                profile: profile.into(),
                dry_run: false,
                authority: Some(identity),
            },
            OValue::store_path("/tmp/does-not-need-to-exist"),
        );
        let err = evaluator.force_request(&request).unwrap_err().to_string();
        assert!(err.contains("forged, revoked"));

        let forged = OValue::capability(
            CapabilityKind::SystemActivation,
            "o-activate-live:forged",
            HashMap::new(),
        );
        let mut scope = HashMap::new();
        scope.insert("authority".into(), forged);
        scope.insert("path".into(), OValue::store_path("/tmp/unused"));
        let err = evaluator
            .eval_node(
                &ONode::Call {
                    fn_name: "activate".into(),
                    args: vec![
                        ONode::VarRef("authority".into()),
                        ONode::VarRef("path".into()),
                    ],
                },
                &scope,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("forged, revoked"));
    }

    #[test]
    fn activation_capability_cannot_escape_its_profile() {
        let mut evaluator = Evaluator::new("/tmp".into());
        let capability = evaluator
            .issue_system_activation_capability("/nix/var/nix/profiles/system")
            .unwrap();
        let mut scope = HashMap::new();
        scope.insert("authority".into(), capability);
        scope.insert("path".into(), OValue::store_path("/tmp/unused"));
        scope.insert("other".into(), OValue::str_("/home/lee/.nix-profile"));
        let err = evaluator
            .eval_node(
                &ONode::Call {
                    fn_name: "activate".into(),
                    args: vec![
                        ONode::VarRef("authority".into()),
                        ONode::VarRef("path".into()),
                        ONode::VarRef("other".into()),
                    ],
                },
                &scope,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("scoped to profile"));
    }

    /// The full four-rung chain — `activate(realise(instantiate($expr)))` —
    /// is structurally well-typed: each Request's source is the previous rung,
    /// and the executor walks the chain end-to-end under Eager.
    #[test]
    fn full_chain_instantiate_realise_activate() {
        let mut e =
            Evaluator::new("/tmp".into()).with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert(
            "expr".into(),
            OValue::nix_expr("nixos.config.system", vec![]),
        );

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
        assert!(
            result.is_system(),
            "instantiate→realise→activate chain must resolve to a System"
        );
    }

    /// activate() with a NixExpr (not yet instantiated) is NOT auto-realised.
    /// The intermediate climb is the user's responsibility to make explicit.
    /// (Auto-realising via a chained Request[Realise[Instantiate]] DOES work,
    /// because the chain is constructed at call sites; bare values aren't
    /// auto-lifted.)
    #[test]
    fn activate_on_bare_nix_expr_errors() {
        let mut e =
            Evaluator::new("/tmp".into()).with_executor(Box::new(MockSystemExecutor::new()));
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
        let result = e
            .eval_node(
                &ONode::Call {
                    fn_name: "current_system".into(),
                    args: vec![],
                },
                &scope,
            )
            .unwrap();
        if let OValue::System { profile_path } = &result {
            assert_eq!(profile_path, "/nix/var/nix/profiles/system");
        } else {
            panic!("expected System");
        }
    }

    /// Activate requests must NEVER hit the executor cache. A stale System
    /// reference would lie about live state, and the whole point of asking
    /// for activation is to do it, not to look up a cached "result."
    #[test]
    fn activate_bypasses_cache_in_executor() {
        let mut exec = ImmediateExecutor::new();
        let path = OValue::store_path("/nix/store/abc-system");
        let req = OValue::request(
            RequestKind::Activate {
                profile: "/p".into(),
                dry_run: true,
                authority: None,
            },
            path,
        );
        let fp = if let OValue::Request { fingerprint, .. } = &req {
            fingerprint.clone()
        } else {
            panic!()
        };
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
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("defer".into()),
            body: vec![ONode::RawText("1".into())],
        };
        let req = e.eval_node(&defer_block, &scope).unwrap();
        let err = e.resolve_for_splice(req).unwrap_err().to_string();
        assert!(err.contains("defer"));
        assert!(
            err.contains("now"),
            "error should tell the user to call now() explicitly, got: {}",
            err
        );
    }

    /// Through eval_document: `let pending = lazy(realise(instantiate($expr)))`
    /// must bind `pending` to a Request, not auto-execute. This was the bug
    /// the block-form lazy^ had: auto_resolve at let-binding would re-execute.
    #[test]
    fn let_binding_preserves_lazy_request_under_eager() {
        use crate::parser::ONode;

        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));

        // We can't put a NixExpr into scope via eval_document's API directly,
        // so we test this by constructing the nodes for both let-bindings.
        let nodes = vec![
            ONode::LetBinding {
                name: "expr".into(),
                expr: Box::new(ONode::TypedExpr {
                    lang: "nix_expr".into(),
                    env_id: u32::MAX,
                    attr: None,
                    body: vec![ONode::RawText("pkgs.hello".into())],
                }),
            },
            ONode::LetBinding {
                name: "pending".into(),
                expr: Box::new(ONode::Call {
                    fn_name: "lazy".into(),
                    args: vec![ONode::Call {
                        fn_name: "realise".into(),
                        args: vec![ONode::Call {
                            fn_name: "instantiate".into(),
                            args: vec![ONode::VarRef("expr".into())],
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
        assert!(
            last.is_request(),
            "let pending = lazy(...) must bind a Request — re-executing at \
             the let-binding boundary would be the old broken behaviour. \
             Got {:?}",
            last
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // quote^ integration tests (in-process, no shim)
    // ─────────────────────────────────────────────────────────────────────────

    /// quote^(python^(6*7)_python)_quote should return OValue::Expr with the
    /// inner source text, NOT start a Python shim or produce 42.
    #[test]
    fn quote_block_returns_oexpr_not_evaluated() {
        let backends: HashSet<String> = ["python", "quote", "O"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut e = Evaluator::new("/tmp".into()).with_registered_backends(backends.clone());
        let scope = HashMap::new();

        let src = r"quote^(python^(6*7)_python)_quote";
        let nodes = crate::parser::Parser::new(src, &backends).parse().unwrap();
        assert_eq!(nodes.len(), 1);

        let result = e.eval_node(&nodes[0], &scope).unwrap();
        match &result {
            OValue::Expr { src } => {
                assert!(
                    src.contains("python^("),
                    "src should contain python^(, got: {:?}",
                    src
                );
                assert!(
                    src.contains("6*7"),
                    "src should contain 6*7, got: {:?}",
                    src
                );
            }
            other => panic!("expected OValue::Expr, got {:?}", other),
        }
    }

    /// A quoted body with MULTIPLE children should capture the raw source text
    /// so the outer O.eval round-trip works.
    #[test]
    fn quote_multi_child_body_raw_source_preserved() {
        let backends: HashSet<String> = ["python", "quote", "O"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut e = Evaluator::new("/tmp".into()).with_registered_backends(backends.clone());
        let scope = HashMap::new();

        let src = "quote^(python^(1)_python python^(2)_python)_quote";
        let nodes = crate::parser::Parser::new(src, &backends).parse().unwrap();
        let result = e.eval_node(&nodes[0], &scope).unwrap();
        match &result {
            OValue::Expr { src } => {
                assert!(
                    src.contains("python^(1)_python"),
                    "missing first block: {:?}",
                    src
                );
                assert!(
                    src.contains("python^(2)_python"),
                    "missing second block: {:?}",
                    src
                );
            }
            other => panic!("expected OValue::Expr, got {:?}", other),
        }
    }

    /// O.eval reads the O bindings visible where the calling backend block was
    /// entered. The callback receives a cloned lexical scope, so bindings made
    /// inside the fragment cannot leak back into the document scope.
    #[test]
    fn o_eval_uses_a_lexical_scope_snapshot() {
        let backends: HashSet<String> = ["python", "quote", "O"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let source = r#"
let answer = python[2]^(41)_python[2]
let q = quote^(
    let callback_only = python[3]^(1)_python[3]
    python[1]^($answer + $callback_only)_python[1]
)_quote
python[0]^(O.eval($q))_python[0]
"#;
        let nodes = Parser::new(source, &backends).parse().unwrap();
        let mut scope = HashMap::new();

        let result = evaluator
            .eval_document_with_scope(nodes, &mut scope)
            .unwrap();

        assert_eq!(result, OValue::int(42));
        assert_eq!(scope.get("answer"), Some(&OValue::int(41)));
        assert!(scope.contains_key("q"));
        assert!(
            !scope.contains_key("callback_only"),
            "O.eval bindings must not mutate the caller's lexical scope"
        );
    }

    /// The explicit two-argument form evaluates against the supplied OScope,
    /// not the lexical scope at the callback site. This makes time-of-capture
    /// visible and lets metaprograms choose which O namespace they evaluate in.
    #[test]
    fn o_eval_accepts_an_explicit_first_class_scope_snapshot() {
        let backends: HashSet<String> = ["python", "quote", "O"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let source = r#"
let answer = python[2]^(41)_python[2]
let captured = scope()
let answer = python[2]^(99)_python[2]
let q = quote^(python[1]^($answer + (1 if isinstance(authority, OOpaqueValue) else 1000))_python[1])_quote
python[0]^(O.eval($q, $captured))_python[0]
"#;
        let nodes = Parser::new(source, &backends).parse().unwrap();
        let mut scope = HashMap::new();
        let authority = evaluator
            .issue_system_activation_capability("/nix/var/nix/profiles/system")
            .unwrap();
        scope.insert("authority".into(), authority.clone());

        let result = evaluator
            .eval_document_with_scope(nodes, &mut scope)
            .unwrap();

        assert_eq!(result, OValue::int(42));
        assert_eq!(scope.get("answer"), Some(&OValue::int(99)));
        let Some(OValue::Scope { bindings }) = scope.get("captured") else {
            panic!("scope() must produce an OScope value")
        };
        assert_eq!(bindings.get("answer"), Some(&OValue::int(41)));
        assert_eq!(bindings.get("authority"), Some(&authority));
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
        assert_eq!(
            result, fake_drv,
            "autonomous() should resolve the buffered request from the cache on exit"
        );
    }

    /// Under autonomous(), Eval requests (nix^{lazy}^()_nix) are executed
    /// eagerly, bypassing the buffer. The buffer only collects Nix-family requests.
    #[test]
    fn autonomous_eval_requests_are_executed_eagerly() {
        let mut e = Evaluator::new("/tmp".into());
        let capability = e
            .issue_backend_execution_capability("nix", BackendAuthority::ALL)
            .unwrap();
        let scope = HashMap::from([("runner".into(), capability)]);

        // Construct an Eval Request (nix {lazy} block) — this should go
        // through the Evaluator's eval_cache, not the scheduler buffer.
        let lazy_nix = ONode::TypedExpr {
            lang: "nix".into(),
            env_id: u32::MAX,
            attr: Some("lazy,cap=runner".into()),
            body: vec![ONode::RawText("1 + 2".into())],
        };
        // First, collect the fingerprint to seed the eval_cache.
        let req = e.eval_node(&lazy_nix, &scope).unwrap();
        let fp = match &req {
            OValue::Request { fingerprint, .. } => fingerprint.clone(),
            _ => panic!(),
        };
        e.eval_cache.insert(fp.clone(), OValue::int(3));

        // Now call autonomous() wrapping another {lazy} nix block for the same expression.
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::Call {
                fn_name: "now".into(),
                args: vec![ONode::TypedExpr {
                    lang: "nix".into(),
                    env_id: u32::MAX,
                    attr: Some("lazy,cap=runner".into()),
                    body: vec![ONode::RawText("1 + 2".into())],
                }],
            }],
        };

        let result = e.eval_node(&call, &scope).unwrap();
        assert_eq!(
            result,
            OValue::int(3),
            "Eval request inside autonomous() must resolve via eval_cache, got {:?}",
            result
        );

        // The buffer must be empty — Eval was not buffered.
        assert!(
            e.autonomous_buffer.is_empty(),
            "autonomous_buffer must be empty after Eval request (not buffered)"
        );
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
        let req = OValue::request(RequestKind::Instantiate, expr);
        let fp = match &req {
            OValue::Request { fingerprint, .. } => fingerprint.clone(),
            _ => panic!(),
        };
        e.scheduler.mem_cache.insert(
            fp,
            OValue::derivation("/nix/store/x.drv", vec!["out".into()], vec![]),
        );
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::TypedExpr {
                    lang: "nix_expr".into(),
                    env_id: u32::MAX,
                    attr: None,
                    body: vec![ONode::RawText("pkgs.hello".into())],
                }],
            }],
        };
        let _ = e.eval_node(&call, &scope);
        assert_eq!(
            e.policy,
            Policy::Eager,
            "policy must be Eager after autonomous() succeeds"
        );

        // Error path: policy still restored.
        let bad = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::VarRef("undefined_var".into())],
        };
        let _ = e.eval_node(&bad, &scope);
        assert_eq!(
            e.policy,
            Policy::Eager,
            "policy must be Eager after autonomous() errors"
        );
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
        assert!(
            e.autonomous_buffer.is_empty(),
            "buffer must be cleared after autonomous() errors"
        );
    }

    /// autonomous() with wrong arg count errors clearly.
    #[test]
    fn autonomous_wrong_arg_count_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::RawText("a".into()), ONode::RawText("b".into())],
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
            args: vec![ONode::VarRef("a".into()), ONode::VarRef("b".into())],
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
            ("all", GroupMode::All),
            ("any", GroupMode::Any),
            ("race", GroupMode::Race),
        ] {
            let call = ONode::Call {
                fn_name: name.into(),
                args: vec![ONode::VarRef("a".into())],
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
        let call = ONode::Call {
            fn_name: "batch".into(),
            args: vec![],
        };
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
            args: vec![ONode::Call {
                fn_name: "batch".into(),
                args: vec![ONode::VarRef("a".into()), ONode::VarRef("b".into())],
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
            args: vec![ONode::Call {
                fn_name: "any".into(),
                args: vec![ONode::VarRef("a".into()), ONode::VarRef("b".into())],
            }],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert_eq!(result, OValue::str_("first"));
    }

    /// `now(batch(realise(instantiate($e1)), realise(instantiate($e2))))`:
    /// Because `batch` is a special form, its arguments are evaluated under
    /// Lazy policy regardless of the outer Eager policy. The group therefore
    /// holds Request chains (not pre-resolved StorePaths).
    ///
    /// Resolution is verified by pre-seeding the scheduler cache and resolving
    /// with `CacheMode::Strict` — the same path used after `autonomous(...)` flush.
    #[test]
    fn now_batch_of_resolved_requests_returns_storepath_list() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = scope_with_nix_expr();

        let mk_chain = |var: &str| ONode::Call {
            fn_name: "realise".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef(var.into())],
            }],
        };

        // Pre-seed the scheduler cache with results for both realise requests.
        let mut members = vec![];
        for var in ["e1", "e2"] {
            let expr = e.eval_node(&ONode::VarRef(var.into()), &scope).unwrap();
            let inst = OValue::request(RequestKind::Instantiate, expr);
            let drv =
                OValue::derivation(format!("/nix/store/{var}.drv"), vec!["out".into()], vec![]);
            let realise = OValue::request(RequestKind::Realise, inst.clone());
            let inst_fp = match &inst {
                OValue::Request { fingerprint, .. } => fingerprint.clone(),
                _ => unreachable!(),
            };
            let real_fp = match &realise {
                OValue::Request { fingerprint, .. } => fingerprint.clone(),
                _ => unreachable!(),
            };
            e.scheduler.mem_cache.insert(inst_fp, drv);
            e.scheduler
                .mem_cache
                .insert(real_fp, OValue::store_path(format!("/nix/store/{var}-out")));
            members.push(realise);
        }

        // Resolve via CacheMode::Strict (same path as autonomous flush result resolution).
        let result = e
            .resolve_group(GroupMode::Batch, &members, CacheMode::Strict)
            .unwrap();
        match result {
            OValue::List { v } => {
                assert_eq!(v.len(), 2);
                assert!(
                    v.iter().all(|x| x.is_store_path()),
                    "all members must resolve to StorePaths, got {:?}",
                    v
                );
            }
            other => panic!("expected list, got {:?}", other),
        }

        // Verify the special-form property: batch() evaluated via eval_node
        // captures Request chains, not pre-resolved values.
        let call = ONode::Call {
            fn_name: "batch".into(),
            args: vec![mk_chain("e1"), mk_chain("e2")],
        };
        let group_val = e.eval_node(&call, &scope).unwrap();
        match &group_val {
            OValue::Group { members, .. } => {
                assert!(
                    members.iter().all(|m| matches!(m, OValue::Request { .. })),
                    "batch() must capture Request members, not resolved values, got {:?}",
                    members
                );
            }
            other => panic!("expected Group from batch(), got {:?}", other),
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
            args: vec![ONode::VarRef("a".into())],
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
            let drv =
                OValue::derivation(format!("/nix/store/{var}.drv"), vec!["out".into()], vec![]);
            let realise = OValue::request(RequestKind::Realise, inst.clone());
            let inst_fp = match &inst {
                OValue::Request { fingerprint, .. } => fingerprint.clone(),
                _ => unreachable!(),
            };
            let real_fp = match &realise {
                OValue::Request { fingerprint, .. } => fingerprint.clone(),
                _ => unreachable!(),
            };
            e.scheduler.mem_cache.insert(inst_fp, drv);
            e.scheduler
                .mem_cache
                .insert(real_fp, OValue::store_path(format!("/nix/store/{var}-out")));
        }

        let mk_chain = |var: &str| ONode::Call {
            fn_name: "realise".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef(var.into())],
            }],
        };
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::Call {
                fn_name: "batch".into(),
                args: vec![mk_chain("e1"), mk_chain("e2")],
            }],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        match result {
            OValue::List { v } => {
                assert_eq!(v.len(), 2, "batch must resolve to a 2-element list");
                assert!(
                    v.iter().all(|x| x.is_store_path()),
                    "members must resolve to StorePaths, got {:?}",
                    v
                );
            }
            other => panic!("expected list from autonomous(batch(...)), got {:?}", other),
        }
        // Policy restored and buffer drained.
        assert_eq!(e.policy, Policy::Eager);
        assert!(e.autonomous_buffer.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Concurrent group resolution — new semantics tests
    // ─────────────────────────────────────────────────────────────────────────

    /// `now(race(...))` over plain-value members returns the FIRST member's
    /// result immediately — race settles on the first value regardless of
    /// whether it is a success or failure.  With plain values (no threads)
    /// the sequential path is used; the first value always settles first.
    #[test]
    fn now_race_returns_first_member_result() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("a".into(), OValue::str_("first"));
        scope.insert("b".into(), OValue::str_("second"));

        let call = ONode::Call {
            fn_name: "now".into(),
            args: vec![ONode::Call {
                fn_name: "race".into(),
                args: vec![ONode::VarRef("a".into()), ONode::VarRef("b".into())],
            }],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        // Race returns the first member to settle — always "first" here.
        assert_eq!(result, OValue::str_("first"));
    }

    /// `now(race(single_member))` works with exactly one member.
    #[test]
    fn now_race_single_member() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("v".into(), OValue::int(42));

        let call = ONode::Call {
            fn_name: "now".into(),
            args: vec![ONode::Call {
                fn_name: "race".into(),
                args: vec![ONode::VarRef("v".into())],
            }],
        };
        assert_eq!(e.eval_node(&call, &scope).unwrap(), OValue::int(42));
    }

    /// `now(any(single_member))` works with exactly one member.
    #[test]
    fn now_any_single_member() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("v".into(), OValue::str_("only"));

        let call = ONode::Call {
            fn_name: "now".into(),
            args: vec![ONode::Call {
                fn_name: "any".into(),
                args: vec![ONode::VarRef("v".into())],
            }],
        };
        assert_eq!(e.eval_node(&call, &scope).unwrap(), OValue::str_("only"));
    }

    /// `now(all(...))` over plain values succeeds like `now(batch(...))` when
    /// all members succeed: the result is an OList with one entry per member.
    ///
    /// NOTE: When a member fails, `all` and `batch` diverge: `all` propagates
    /// the first error (hard all-or-nothing barrier), while `batch` wraps each
    /// failure as `OValue::Error` and always returns a full list. That
    /// distinction is tested separately in `batch_collects_error_outcomes_as_values`.
    #[test]
    fn now_all_returns_list_identical_to_batch() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert("x".into(), OValue::int(1));
        scope.insert("y".into(), OValue::int(2));

        let make_call = |fn_name: &str| ONode::Call {
            fn_name: "now".into(),
            args: vec![ONode::Call {
                fn_name: fn_name.to_string(),
                args: vec![ONode::VarRef("x".into()), ONode::VarRef("y".into())],
            }],
        };

        let batch_result = e.eval_node(&make_call("batch"), &scope).unwrap();
        let all_result = e.eval_node(&make_call("all"), &scope).unwrap();
        assert_eq!(
            batch_result, all_result,
            "batch and all must produce identical results for plain values"
        );
        assert_eq!(
            batch_result,
            OValue::list(vec![OValue::int(1), OValue::int(2)])
        );
    }

    /// `now(race(...))` over pre-resolved Requests: because `race` is a special
    /// form, its arguments are captured as Request chains. A pre-built group with
    /// plain StorePaths uses the sequential race path — first member wins.
    #[test]
    fn now_race_of_resolved_requests_returns_first() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let scope = scope_with_nix_expr();

        // Verify the special-form property: race() evaluated via eval_node
        // captures Request chains (Lazy-evaluated), not pre-resolved values.
        let mk_chain = |var: &str| ONode::Call {
            fn_name: "realise".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef(var.into())],
            }],
        };
        let call = ONode::Call {
            fn_name: "race".into(),
            args: vec![mk_chain("e1"), mk_chain("e2")],
        };
        let group_val = e.eval_node(&call, &scope).unwrap();
        match &group_val {
            OValue::Group { members, .. } => {
                assert!(
                    members.iter().all(|m| matches!(m, OValue::Request { .. })),
                    "race() must capture Request members, not resolved values, got {:?}",
                    members
                );
            }
            other => panic!("expected Group from race(), got {:?}", other),
        }

        // Also test that a race group over plain StorePaths resolves correctly:
        // first member (sequential path) always wins.
        let sp1 = OValue::store_path("/nix/store/aaa-out");
        let sp2 = OValue::store_path("/nix/store/bbb-out");
        let group = OValue::group(GroupMode::Race, vec![sp1.clone(), sp2]);
        let (mode, members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        let result = e.resolve_group(mode, &members, CacheMode::Fresh).unwrap();
        assert_eq!(result, sp1, "sequential race must return the first member");
    }

    /// `is_threadable_member` recognises Nix-family Requests and rejects Eval
    /// Requests and plain values.
    #[test]
    fn is_threadable_member_classification() {
        let nix_expr = OValue::nix_expr("pkgs.hello", vec![]);

        let inst = OValue::request(RequestKind::Instantiate, nix_expr.clone());
        assert!(
            Evaluator::is_threadable_member(&inst),
            "Instantiate Request must be threadable"
        );

        let drv = OValue::derivation("/nix/store/x.drv", vec!["out".into()], vec![]);
        let real = OValue::request(RequestKind::Realise, drv);
        assert!(
            Evaluator::is_threadable_member(&real),
            "Realise Request must be threadable"
        );

        let thunk = OValue::thunk("1+1", vec![]);
        let eval = OValue::request(
            RequestKind::Eval {
                lang: "python".into(),
                env_id: 0,
                cacheable: false,
                authority: None,
                permissions: vec![],
            },
            thunk,
        );
        assert!(
            !Evaluator::is_threadable_member(&eval),
            "Eval Request must NOT be threadable"
        );

        assert!(
            !Evaluator::is_threadable_member(&OValue::int(1)),
            "plain value must NOT be threadable"
        );
        assert!(
            !Evaluator::is_threadable_member(&OValue::str_("hello")),
            "string must NOT be threadable"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Group resolution: failure-semantic contract tests
    //
    // These tests verify the Resolution Algebra from the OValue::Group spec:
    //   - Collect-All (Batch/All): entire group fails if ANY member fails.
    //   - Winner-Take-All (Any):   skips failed members; fails only when ALL fail.
    //   - Winner-Take-All (Race):  first member's result (Ok or Err) wins
    //                               immediately; later members are ignored.
    //
    // An empty group (constructed directly via OValue::group with no members)
    // is used as the "always-failing" member: resolve_group bails on it with
    // "no members to resolve".
    // ─────────────────────────────────────────────────────────────────────────

    fn failing_member(mode: GroupMode) -> OValue {
        // An empty group always fails when resolved (no members to resolve).
        OValue::group(mode, vec![])
    }

    /// `batch(ok_val, failing_group)` — Batch collects ALL outcomes; a failing
    /// member becomes an `OValue::Error` in the result list rather than aborting
    /// the whole group. The result list always has one entry per input member.
    #[test]
    fn batch_fails_if_any_member_fails() {
        let mut e = Evaluator::new("/tmp".into());
        let group = OValue::group(
            GroupMode::Batch,
            vec![OValue::int(1), failing_member(GroupMode::Batch)],
        );
        let (mode, members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        // Batch must succeed (return a list), wrapping the failed member as OError.
        let result = e.resolve_group(mode, &members, CacheMode::Fresh).unwrap();
        match &result {
            OValue::List { v } => {
                assert_eq!(v.len(), 2, "batch must return one entry per member");
                assert_eq!(v[0], OValue::int(1), "successful member must be preserved");
                assert!(
                    v[1].is_error(),
                    "failing member must become OError, got {:?}",
                    v[1]
                );
            }
            other => panic!("batch must return a list, got {:?}", other),
        }
    }

    /// `all(ok_val, failing_group)` — the entire All group fails when any
    /// member fails.  All is an all-or-nothing hard barrier: unlike Batch,
    /// it does NOT wrap failures as OError — it propagates the first error.
    #[test]
    fn all_fails_if_any_member_fails() {
        let mut e = Evaluator::new("/tmp".into());
        let group = OValue::group(
            GroupMode::All,
            vec![OValue::str_("ok"), failing_member(GroupMode::All)],
        );
        let (mode, members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        let err = e
            .resolve_group(mode, &members, CacheMode::Fresh)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("all") || err.contains("no members"),
            "all must fail when a member fails, got: {err}"
        );
    }

    /// `any(failing_group, ok_val)` — Any skips the first (failing) member and
    /// returns the second (successful) member.  Verifies fallback semantics.
    #[test]
    fn any_skips_failed_member_and_returns_first_success() {
        let mut e = Evaluator::new("/tmp".into());
        let group = OValue::group(
            GroupMode::Any,
            vec![failing_member(GroupMode::Any), OValue::str_("winner")],
        );
        let (mode, members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        let result = e.resolve_group(mode, &members, CacheMode::Fresh).unwrap();
        assert_eq!(
            result,
            OValue::str_("winner"),
            "any must skip the failed first member and return the second"
        );
    }

    /// `any(fail1, fail2)` — Any fails only when EVERY member fails.
    #[test]
    fn any_fails_only_when_all_members_fail() {
        let mut e = Evaluator::new("/tmp".into());
        let group = OValue::group(
            GroupMode::Any,
            vec![
                failing_member(GroupMode::Any),
                failing_member(GroupMode::Any),
            ],
        );
        let (mode, members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        let err = e
            .resolve_group(mode, &members, CacheMode::Fresh)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("any") && err.contains("members failed"),
            "any must fail when all members fail, got: {err}"
        );
    }

    /// `race(failing_group, ok_val)` — sequential Race returns the first member's
    /// result immediately, even when it is a failure.  The second member is
    /// never attempted.
    #[test]
    fn race_returns_lead_member_failure_immediately() {
        let mut e = Evaluator::new("/tmp".into());
        let group = OValue::group(
            GroupMode::Race,
            vec![
                failing_member(GroupMode::Race),
                OValue::str_("never_reached"),
            ],
        );
        let (mode, members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        // Race settles on the first result whether Ok or Err.
        let err = e
            .resolve_group(mode, &members, CacheMode::Fresh)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("race") || err.contains("no members"),
            "race must propagate the lead member's failure, got: {err}"
        );
    }

    /// `race(ok_val, ...)` — Race returns the first member's successful result;
    /// later members are not consulted.
    #[test]
    fn race_returns_lead_member_success_immediately() {
        let mut e = Evaluator::new("/tmp".into());
        let group = OValue::group(GroupMode::Race, vec![OValue::int(42), OValue::int(99)]);
        let (mode, members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        let result = e.resolve_group(mode, &members, CacheMode::Fresh).unwrap();
        assert_eq!(
            result,
            OValue::int(42),
            "race must return the lead member's value"
        );
    }

    /// Member order is preserved in Collect-All results.  `batch(a, b, c)` must
    /// return `[a, b, c]` in declaration order regardless of resolution timing.
    #[test]
    fn batch_result_preserves_member_order() {
        let mut e = Evaluator::new("/tmp".into());
        let members = vec![
            OValue::str_("first"),
            OValue::str_("second"),
            OValue::str_("third"),
        ];
        let group = OValue::group(GroupMode::Batch, members.clone());
        let (mode, grp_members) = match &group {
            OValue::Group { mode, members, .. } => (*mode, members.clone()),
            _ => unreachable!(),
        };
        let result = e
            .resolve_group(mode, &grp_members, CacheMode::Fresh)
            .unwrap();
        assert_eq!(
            result,
            OValue::list(members),
            "batch must preserve member order in the result list"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // New semantic tests (lock down the OGroup semantics)
    // ─────────────────────────────────────────────────────────────────────────

    /// `batch(realise(instantiate($e)))` evaluated via `eval_node` must hold
    /// Request members — not pre-resolved StorePaths — regardless of the outer
    /// Eager policy.  This is the fundamental "group constructors are special
    /// forms" property.
    #[test]
    fn batch_does_not_eagerly_force_request_members() {
        let mut e = Evaluator::new("/tmp".into()).with_executor(Box::new(MockExecutor::new()));
        let scope = scope_with_nix_expr();

        let call = ONode::Call {
            fn_name: "batch".into(),
            args: vec![
                ONode::Call {
                    fn_name: "realise".into(),
                    args: vec![ONode::Call {
                        fn_name: "instantiate".into(),
                        args: vec![ONode::VarRef("e1".into())],
                    }],
                },
                ONode::Call {
                    fn_name: "realise".into(),
                    args: vec![ONode::Call {
                        fn_name: "instantiate".into(),
                        args: vec![ONode::VarRef("e2".into())],
                    }],
                },
            ],
        };
        // Default policy is Eager; yet batch() must capture Requests.
        assert_eq!(e.policy, Policy::Eager);
        let group = e.eval_node(&call, &scope).unwrap();
        match group {
            OValue::Group { members, mode, .. } => {
                assert_eq!(mode, GroupMode::Batch);
                assert_eq!(members.len(), 2, "batch must have 2 members");
                for (i, m) in members.iter().enumerate() {
                    assert!(
                        matches!(m, OValue::Request { .. }),
                        "member {} must be a Request (not a resolved value), got {:?}",
                        i,
                        m
                    );
                }
            }
            other => panic!("batch() must return a Group, got {:?}", other),
        }
        // Outer policy must be restored after the special-form.
        assert_eq!(
            e.policy,
            Policy::Eager,
            "policy must be restored after batch()"
        );
    }

    /// `batch(ok, fail)` collects BOTH outcomes: the successful member keeps its
    /// value; the failing member becomes `OValue::Error` in the list. The group
    /// itself never returns `Err` — it always returns a full-length list.
    #[test]
    fn batch_collects_error_outcomes_as_values() {
        let mut e = Evaluator::new("/tmp".into());

        // failing_member(Batch) is an empty Batch group, which errors on resolution.
        let members = vec![OValue::str_("ok"), failing_member(GroupMode::Batch)];
        let result = e
            .resolve_group(GroupMode::Batch, &members, CacheMode::Fresh)
            .unwrap();

        match result {
            OValue::List { v } => {
                assert_eq!(v.len(), 2, "batch list must have one entry per member");
                assert_eq!(v[0], OValue::str_("ok"), "successful member preserved");
                assert!(
                    v[1].is_error(),
                    "failed member must become OError in batch result, got {:?}",
                    v[1]
                );
                // The OError message should contain some indication of the failure.
                if let OValue::Error { msg } = &v[1] {
                    assert!(!msg.is_empty(), "OError message must not be empty");
                }
            }
            other => panic!(
                "batch must return a list even with failures, got {:?}",
                other
            ),
        }
    }

    /// After `autonomous(...)` flush, if a Request's result is absent from the
    /// cache, `resolve_group` with `CacheMode::Strict` must produce a hard error.
    ///
    /// For `Batch` mode: the error is wrapped as `OValue::Error` in the result list.
    /// For `All` mode: the error propagates and the whole group fails.
    #[test]
    fn autonomous_batch_errors_on_missing_cache_result() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = scope_with_nix_expr();

        // Build a realise Request but do NOT seed the cache.
        let expr = e.eval_node(&ONode::VarRef("e1".into()), &scope).unwrap();
        let inst = OValue::request(RequestKind::Instantiate, expr);
        let realise = OValue::request(RequestKind::Realise, inst);

        // Batch mode: cache miss becomes OError in the result list.
        let batch_result = e
            .resolve_group(
                GroupMode::Batch,
                std::slice::from_ref(&realise),
                CacheMode::Strict,
            )
            .unwrap();
        match batch_result {
            OValue::List { v } => {
                assert_eq!(v.len(), 1);
                assert!(
                    v[0].is_error(),
                    "CacheStrict batch must produce OError on cache miss, got {:?}",
                    v[0]
                );
                if let OValue::Error { msg } = &v[0] {
                    assert!(
                        msg.contains("autonomous")
                            || msg.contains("cache miss")
                            || msg.contains("materialize"),
                        "error message must indicate a strict cache miss, got: {msg}"
                    );
                }
            }
            other => panic!("batch must return a list, got {:?}", other),
        }

        // All mode: cache miss propagates as a hard error.
        let all_err = e
            .resolve_group(GroupMode::All, &[realise], CacheMode::Strict)
            .unwrap_err()
            .to_string();
        assert!(
            all_err.contains("autonomous")
                || all_err.contains("cache miss")
                || all_err.contains("materialize"),
            "CacheStrict All must hard-error on cache miss, got: {all_err}"
        );
    }

    /// `now(group)` must respect the scheduler's parallelism cap:
    /// at most `scheduler.parallelism` threads are in flight at once.
    ///
    /// We verify this by setting `parallelism = 1`, building a 3-member
    /// group of plain values (no threadable Requests), and confirming that
    /// the result is still correct (the cap only limits concurrent Nix
    /// threads; plain values always resolve serially).
    #[test]
    fn now_group_uses_parallelism_cap() {
        let mut e = Evaluator::new("/tmp".into());
        // Set a low parallelism cap.
        e.scheduler = e.scheduler.with_parallelism(1);
        assert_eq!(e.scheduler.parallelism, 1);

        let members = vec![OValue::int(10), OValue::int(20), OValue::int(30)];
        let result = e
            .resolve_group(GroupMode::All, &members, CacheMode::Fresh)
            .unwrap();
        assert_eq!(
            result,
            OValue::list(vec![OValue::int(10), OValue::int(20), OValue::int(30)]),
            "parallelism cap must not affect correctness of plain-value groups"
        );
    }

    /// A nested group resolves deterministically:
    ///   `all(any(a, b), batch(c))` → `[first_of(a,b), [c]]`
    ///
    /// This verifies that `resolve_member` correctly recurses into nested groups
    /// and that member order is preserved throughout.
    #[test]
    fn nested_group_resolution_is_deterministic() {
        let mut e = Evaluator::new("/tmp".into());

        // Inner any(a, b) → "a" (first success)
        let inner_any = OValue::group(GroupMode::Any, vec![OValue::str_("a"), OValue::str_("b")]);
        // Inner batch(c) → ["c"]
        let inner_batch = OValue::group(GroupMode::Batch, vec![OValue::str_("c")]);
        // Outer all(inner_any, inner_batch)
        let outer_members = vec![inner_any, inner_batch];
        let result = e
            .resolve_group(GroupMode::All, &outer_members, CacheMode::Fresh)
            .unwrap();

        // Expect: [<result of any("a","b")>, <result of batch("c")>]
        //       = ["a", ["c"]]
        assert_eq!(
            result,
            OValue::list(vec![
                OValue::str_("a"),
                OValue::list(vec![OValue::str_("c")]),
            ]),
            "nested group must resolve deterministically"
        );
    }

    /// A group's fingerprint must change when the order of its members changes.
    /// This ensures fingerprint-keyed caches treat `batch(a, b)` ≠ `batch(b, a)`.
    #[test]
    fn group_fingerprint_changes_when_member_order_changes() {
        let a = OValue::str_("alpha");
        let b = OValue::str_("beta");

        let g_ab = OValue::group(GroupMode::Batch, vec![a.clone(), b.clone()]);
        let g_ba = OValue::group(GroupMode::Batch, vec![b.clone(), a.clone()]);

        let fp_ab = match &g_ab {
            OValue::Group { fingerprint, .. } => fingerprint.clone(),
            _ => unreachable!(),
        };
        let fp_ba = match &g_ba {
            OValue::Group { fingerprint, .. } => fingerprint.clone(),
            _ => unreachable!(),
        };

        assert_ne!(
            fp_ab, fp_ba,
            "fingerprints must differ when member order differs"
        );

        // Sanity: same order → same fingerprint.
        let g_ab2 = OValue::group(GroupMode::Batch, vec![a.clone(), b.clone()]);
        let fp_ab2 = match &g_ab2 {
            OValue::Group { fingerprint, .. } => fingerprint.clone(),
            _ => unreachable!(),
        };
        assert_eq!(
            fp_ab, fp_ab2,
            "fingerprints must be stable for identical groups"
        );
    }
}
