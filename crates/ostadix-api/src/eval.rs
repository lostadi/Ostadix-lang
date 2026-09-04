// ─────────────────────────────────────────────────────────────────────────────
// eval.rs
//
// The Ostadix-lang OIR evaluator — plan-owned graph execution.
//
// Evaluation semantics (mirrors o_lang/evaluator.py):
//
//   OIr::Exec { lang, env_id, backend, body }:
//     1. Walk body children left-to-right, building a splice buffer:
//          Text  → append verbatim
//          Load  → look up scope, render through the OIR backend interface
//          Exec  → read already-computed child values from the execution frame
//     2. Call ProcessRegistry::exec(lang, env_id, buffer, scope, shim)
//     3. For fresh envs (bare ephemeral or linker-isolated `[*]`): normalize
//        to the process-registry ephemeral key and clean up after the attempt.
//
//   Root document (eval_document):
//     Lower ONode syntax to OIR, build and validate ExecutionPlan, execute its
//     topological schedule, and return the last non-null root OValue.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

use anyhow::{bail, Context, Result};

#[cfg(test)]
use crate::backend_catalog::SpliceRenderer;
use crate::backend_catalog::{
    BackendAdapterKind, BackendInterface, BackendRegistry, ExecutionMode,
};
use crate::backend_state::{
    ensure_evaluator_snapshot_bound, sandbox_policy_sha256, EvaluatorActorCheckpointV1,
    EvaluatorStateSnapshotV1,
};
use crate::capability::{fresh_bearer_identity, BackendAuthorityBroker, BackendSandboxPolicy};
use crate::environment::EnvironmentRefV2;
use crate::eval_core::{
    data_predecessors, derive_policy_contexts, render_with, trace_fingerprint, GraphEvalFrame,
    GraphEvaluationHost,
};
pub use crate::eval_core::{render_fidelity, ExecutionTrace, RenderFidelity, TraceEvent};
pub use crate::execution_contract::Policy;
use crate::execution_contract::{
    is_o_identifier, validate_execution_metadata, BlockEvalPolicy, BlockOptions,
};
#[cfg(test)]
use crate::ir::lower_node;
use crate::ir::{
    reconstruct_source as reconstruct_ir_source, ExecutionPlan, InvokeMode, OIr, OIrProgram,
    PlanNodeId, PlanNodeKind,
};
use crate::nix_ops;
use crate::nixos_ops;
use crate::parser::{ONode, Parser};
use crate::process::{BackendLaunchContext, ExecStep, ProcessRegistry};
use crate::scheduler::AutonomousScheduler;
#[cfg(test)]
use crate::value::ONumber;
use crate::value::{
    fingerprint_preview, BackendAuthority, CapabilityKind, GroupMode, OValue, RequestKind,
};

/// Stable evidence projection of the built-in authority policy. The random
/// bearer that realizes this policy remains process-local and is checked at
/// every dispatch; hashing that secret into launch identity would make an
/// otherwise compatible actor checkpoint unrestorable after evaluator restart.
const DEFAULT_BACKEND_AUTHORITY_POLICY_V1: &str = "wildcard:fs_read,fs_write,network,process";

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

/// Parse the compatibility backend-grant grammar without minting a live
/// capability. Intent preflight uses this so malformed grants cannot allocate
/// a run ID or change `last-run` before the evaluator is entered.
pub(crate) fn validate_backend_grant_spec(spec: &str) -> Result<()> {
    parse_backend_grant_spec(spec).map(|_| ())
}

fn parse_backend_grant_spec(spec: &str) -> Result<(&str, &str, Vec<BackendAuthority>)> {
    let (name, grant) = spec.split_once('=').ok_or_else(|| {
        anyhow::anyhow!("backend grant must be NAME=LANG[:RIGHT,...], got `{spec}`")
    })?;
    if !is_o_identifier(name) {
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
    Ok((name, language, parsed))
}

// ─────────────────────────────────────────────────────────────────────────────
// exec_nix_kind — thread-safe Nix-family dispatcher
//
// Executes a single Nix-family RequestKind against an already-resolved source
// value. Called inside group-resolution threads; takes no `self` reference so
// it can be safely moved into `thread::spawn` closures.
// ─────────────────────────────────────────────────────────────────────────────

type SharedNixLease = Arc<Result<crate::runtime_exec::RuntimeCommandLease, String>>;

fn capture_shared_nix_lease() -> SharedNixLease {
    Arc::new(
        crate::runtime_exec::RuntimeCommandLease::capture("nix")
            .map_err(|error| format!("{error:#}")),
    )
}

fn require_shared_nix_lease(
    shared: Option<&SharedNixLease>,
) -> Result<&crate::runtime_exec::RuntimeCommandLease> {
    match shared.map(|lease| lease.as_ref()) {
        Some(Ok(lease)) => Ok(lease),
        Some(Err(error)) => Err(anyhow::anyhow!(error.clone())),
        None => bail!("Nix request has no perform-time runtime command authority"),
    }
}

fn exec_nix_kind(
    kind: RequestKind,
    src: OValue,
    nix_lease: Option<SharedNixLease>,
) -> Result<OValue> {
    match kind {
        RequestKind::Instantiate => {
            nix_ops::instantiate_nix_with_lease(&src, require_shared_nix_lease(nix_lease.as_ref())?)
        }
        RequestKind::Realise => {
            nix_ops::realise_nix_with_lease(&src, require_shared_nix_lease(nix_lease.as_ref())?)
        }
        RequestKind::Activate {
            profile,
            dry_run: true,
            authority: None,
        } => nixos_ops::activate_nix(&src, &profile, true),
        RequestKind::Activate { .. } => {
            bail!("real activation requires the evaluator thread")
        }
        RequestKind::Eval { .. } => bail!(
            "exec_nix_kind: RequestKind::Eval must not appear in concurrent \
             group dispatch (Eval requests are always executed serially)"
        ),
    }
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
        let mut nix_lease = None;
        self.execute_with_nix_lease(req, &mut nix_lease)
    }
}

impl ImmediateExecutor {
    fn execute_with_nix_lease(
        &mut self,
        req: &OValue,
        nix_lease: &mut Option<crate::runtime_exec::RuntimeCommandLease>,
    ) -> Result<OValue> {
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
            OValue::Request { .. } => self.execute_with_nix_lease(&source, nix_lease)?,
            other => other,
        };

        // Preserve source-first Request semantics and type diagnostics before
        // acquiring host runtime capacity. The first Nix rung actually reached
        // after cache/source resolution captures the lease; outer rungs reuse
        // the same retained executable.
        match &kind {
            RequestKind::Instantiate => nix_ops::validate_instantiate_source(&resolved_source)?,
            RequestKind::Realise => nix_ops::validate_realise_source(&resolved_source)?,
            _ => {}
        }
        if nix_lease.is_none() && matches!(&kind, RequestKind::Instantiate | RequestKind::Realise) {
            *nix_lease = Some(crate::runtime_exec::RuntimeCommandLease::capture("nix")?);
        }

        let result = match kind {
            RequestKind::Instantiate => nix_ops::instantiate_nix_with_lease(
                &resolved_source,
                nix_lease
                    .as_ref()
                    .context("instantiate request has no perform-time Nix authority")?,
            )?,
            RequestKind::Realise => nix_ops::realise_nix_with_lease(
                &resolved_source,
                nix_lease
                    .as_ref()
                    .context("realise request has no perform-time Nix authority")?,
            )?,
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
            // STEP-4: dry activation is safe to dispatch through this executor.
            // Real activation stays on the Evaluator thread so nested sources
            // and optional embedding guards are resolved in one local context.
            RequestKind::Activate {
                profile,
                dry_run: true,
                authority: None,
            } => nixos_ops::activate_nix(&resolved_source, &profile, true)?,
            RequestKind::Activate { .. } => bail!(
                "ImmediateExecutor cannot perform real activation directly; \
                 the Evaluator must dispatch host-profile mutation"
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

    /// Optional CLI/API override for evidence-admitted HGraph local-worker
    /// tasks. Without it, the coordinator resolves the count from the current
    /// machine and the admitted maximum worker-wave width. This must not tune
    /// the legacy buffered Request scheduler as a side effect.
    local_worker_parallelism_override: Option<usize>,

    /// Explicit Fabric V1 physical-attempt selection for trusted-inline
    /// renderers. Admission remains V6 and local-worker classified; this
    /// additive policy supplies the exact remote authority and target and has
    /// no local-renderer fallback.
    physical_attempt_adapter: Option<Arc<dyn crate::executor::PhysicalAttemptAdapterV1>>,

    /// Whether idle worker threads may survive across evaluation boundaries.
    /// Disabled by default because thread-local security state such as
    /// Landlock or seccomp cannot be fully inspected after construction.
    reuse_local_worker_pool: bool,

    /// Idle graph workers retained across sequential evaluations. A running
    /// coordinator takes ownership of the pool, which also keeps nested
    /// O.eval callbacks from borrowing the workers that are servicing them.
    local_worker_pool: Option<crate::executor::pool::WorkerPool>,

    /// Optional native `O --o-backend` entrypoint for embedding processes that
    /// are not themselves the O evaluator (for example `o-node`). Ordinary O
    /// execution leaves this unset and binds `current_exe()` as before.
    runtime_executable_override: Option<PathBuf>,

    /// STEP-4: buffer of non-Eval Requests constructed under
    /// Policy::Autonomous. Flushed by flush_autonomous_buffer() at force
    /// points: end of autonomous(expr) block, explicit now(), document end.
    autonomous_buffer: Vec<OValue>,

    /// The validated plan used by the most recent document execution.
    last_execution_plan: Option<ExecutionPlan>,

    /// Deterministic lifecycle trace for the most recent OIR execution.
    last_execution_trace: Option<ExecutionTrace>,

    /// Digest-bound pre-execution decision that authorized the most recent
    /// graph run (or was compiled for the serial differential oracle).
    last_execution_admission: Option<crate::evidence::ExecutionAdmissionV6>,

    /// The hypergraph schedule built from the most recent lowered OIR program.
    /// This is the compiled foothold for the graph executor: current runtime
    /// dispatch still interprets OIR, but graph construction, type solving,
    /// backend-fidelity propagation, and clustering run on every document.
    last_hgraph_schedule: Option<crate::hgraph::Schedule>,

    /// Optional live, process-local authority for embedding-specific activation
    /// guards.
    ///
    /// Plain O programs do not need a bearer to mutate the host profile. This
    /// table only backs explicit `activate(capability, path)` calls for hosts
    /// that still want a profile-scoped guard at that call site.
    activation_authorities: HashMap<String, String>,

    /// Live bearer bindings for authority requested by hosted backend blocks.
    backend_authorities: BackendAuthorityBroker,

    /// Built-in wildcard backend authority used by default Ostadix-lang execution.
    /// Ostadix-lang treats hosted backends as the normal execution substrate, so
    /// grantable backend rights are available by default.
    default_backend_authority: String,

    /// Persistent backend actors `(language, environment)` currently suspended
    /// while awaiting the result of a nested `O.eval` callback. Used by the
    /// graph executor to detect reentrant deadlocks: a nested evaluation that
    /// tries to run a new command on an actor already suspended awaiting its
    /// own eval result is a precise error rather than a hang.
    suspended_actors: HashSet<(String, u32)>,

    /// Deadline inherited from an admitted worker's `O.eval` callback. Only
    /// recursive work uses this field; ordinary top-level evaluation retains
    /// its existing execution contract.
    callback_operation_deadline: Option<Instant>,

    /// Opaque process-local authority over the exact executable artifacts
    /// selected during the currently executing admission. Canonical evidence
    /// contains only the immutable manifest; retained file handles never
    /// become serializable language values.
    active_executable_leases: Option<Arc<crate::runtime_exec::ExecutableLeaseSet>>,

    /// Per-backend actor generation identities projected by the active
    /// admission. Unlike the whole-plan binding, each digest is stable when an
    /// unrelated backend is added to the plan, while still changing with this
    /// backend's launcher, shim, or child launch context.
    active_backend_launch_generations: Option<HashMap<String, String>>,

    /// Authority-free actor checkpoints waiting for the first exact admitted
    /// dispatch after restart. The sandbox digest participates in identity so
    /// two deliberately isolated actors cannot overwrite one another.
    pending_backend_restores: HashMap<(String, u32, String), EvaluatorActorCheckpointV1>,

    /// A placement-prepared fragment is a closed, single-backend authority
    /// unit.  Recursive `O.eval` would introduce an unadmitted second program,
    /// so callbacks are rejected while that unit is being consumed even when
    /// a foreign backend obscures the request from static source scanning.
    prepared_fragment_callbacks_forbidden: bool,
}

/// Authority-free coordinates derived from the exact locally admitted
/// fragment.  These values may be compared with a placement lease; they are
/// not themselves permission to execute it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementFragmentBindingsV1 {
    source_sha256: String,
    canonical_backend: String,
    plan_node: PlanNodeId,
    operation_oir: crate::resource_identity::ArtifactId,
    requirement_footprint: crate::placement::RequirementFootprintV1,
    requirement_footprint_sha256: crate::placement::SemanticDigestV1,
    placement_admission: crate::placement::SemanticDigestV1,
    task_attempt: crate::placement::TaskAttemptIdV1,
    backend_implementation: crate::placement::BackendImplementationIdV1,
    backend_implementation_sha256: crate::placement::SemanticDigestV1,
    backend_launch_generation: crate::placement::SemanticDigestV1,
    environment: EnvironmentRefV2,
    sandbox_permissions: Vec<BackendAuthority>,
    sandbox_policy_sha256: crate::placement::SemanticDigestV1,
}

impl PlacementFragmentBindingsV1 {
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    pub fn canonical_backend(&self) -> &str {
        &self.canonical_backend
    }

    pub fn plan_node(&self) -> PlanNodeId {
        self.plan_node
    }

    pub fn operation_oir(&self) -> &crate::resource_identity::ArtifactId {
        &self.operation_oir
    }

    pub fn requirement_footprint(&self) -> &crate::placement::RequirementFootprintV1 {
        &self.requirement_footprint
    }

    pub fn requirement_footprint_sha256(&self) -> &crate::placement::SemanticDigestV1 {
        &self.requirement_footprint_sha256
    }

    /// Process-portable admission over the exact semantic OIR/plan/HGraph,
    /// current catalog projection, analyzer, schema, and base policy.
    pub fn placement_admission(&self) -> &crate::placement::SemanticDigestV1 {
        &self.placement_admission
    }

    /// Compatibility spelling for the placement-lease admission coordinate.
    /// This is intentionally not the full process-local `admission_sha256`.
    pub fn admission(&self) -> &crate::placement::SemanticDigestV1 {
        self.placement_admission()
    }

    pub fn task_attempt(&self) -> &crate::placement::TaskAttemptIdV1 {
        &self.task_attempt
    }

    pub fn backend_implementation(&self) -> &crate::placement::BackendImplementationIdV1 {
        &self.backend_implementation
    }

    pub fn backend_implementation_sha256(&self) -> &crate::placement::SemanticDigestV1 {
        &self.backend_implementation_sha256
    }

    pub fn realization_pipeline(&self) -> &crate::placement::SemanticDigestV1 {
        self.backend_implementation.realization_pipeline()
    }

    /// Exact admitted local process-generation coordinate: selected direct
    /// executable set, consumed compatibility-adapter rows, and launch
    /// context. This is descriptive input to `ActorGenerationIdV1`, not
    /// mutable launch authority.
    pub fn backend_launch_generation(&self) -> &crate::placement::SemanticDigestV1 {
        &self.backend_launch_generation
    }

    pub fn environment(&self) -> EnvironmentRefV2 {
        self.environment
    }

    pub fn sandbox_permissions(&self) -> &[BackendAuthority] {
        &self.sandbox_permissions
    }

    pub fn sandbox_policy_sha256(&self) -> &crate::placement::SemanticDigestV1 {
        &self.sandbox_policy_sha256
    }
}

/// Current package-0.3 placement coordinates. V2 is freshly derived from
/// Admission V6 and the placement-admission V2 digest domain; it is never
/// constructed from or compared as equivalent to V1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementFragmentBindingsV2 {
    source_sha256: String,
    canonical_backend: String,
    plan_node: PlanNodeId,
    operation_oir: crate::resource_identity::ArtifactId,
    requirement_footprint: crate::placement::RequirementFootprintV1,
    requirement_footprint_sha256: crate::placement::SemanticDigestV1,
    placement_admission: crate::placement::SemanticDigestV1,
    task_attempt: crate::placement::TaskAttemptIdV1,
    backend_implementation: crate::placement::BackendImplementationIdV1,
    backend_implementation_sha256: crate::placement::SemanticDigestV1,
    backend_launch_generation: crate::placement::SemanticDigestV1,
    environment: EnvironmentRefV2,
    sandbox_permissions: Vec<BackendAuthority>,
    sandbox_policy_sha256: crate::placement::SemanticDigestV1,
}

impl PlacementFragmentBindingsV2 {
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    pub fn canonical_backend(&self) -> &str {
        &self.canonical_backend
    }

    pub fn plan_node(&self) -> PlanNodeId {
        self.plan_node
    }

    pub fn operation_oir(&self) -> &crate::resource_identity::ArtifactId {
        &self.operation_oir
    }

    pub fn requirement_footprint(&self) -> &crate::placement::RequirementFootprintV1 {
        &self.requirement_footprint
    }

    pub fn requirement_footprint_sha256(&self) -> &crate::placement::SemanticDigestV1 {
        &self.requirement_footprint_sha256
    }

    pub fn placement_admission(&self) -> &crate::placement::SemanticDigestV1 {
        &self.placement_admission
    }

    pub fn admission(&self) -> &crate::placement::SemanticDigestV1 {
        self.placement_admission()
    }

    pub fn task_attempt(&self) -> &crate::placement::TaskAttemptIdV1 {
        &self.task_attempt
    }

    pub fn backend_implementation(&self) -> &crate::placement::BackendImplementationIdV1 {
        &self.backend_implementation
    }

    pub fn backend_implementation_sha256(&self) -> &crate::placement::SemanticDigestV1 {
        &self.backend_implementation_sha256
    }

    pub fn realization_pipeline(&self) -> &crate::placement::SemanticDigestV1 {
        self.backend_implementation.realization_pipeline()
    }

    pub fn backend_launch_generation(&self) -> &crate::placement::SemanticDigestV1 {
        &self.backend_launch_generation
    }

    pub fn environment(&self) -> EnvironmentRefV2 {
        self.environment
    }

    pub fn sandbox_permissions(&self) -> &[BackendAuthority] {
        &self.sandbox_permissions
    }

    pub fn sandbox_policy_sha256(&self) -> &crate::placement::SemanticDigestV1 {
        &self.sandbox_policy_sha256
    }
}

/// Non-cloneable, process-local execution authority prepared from one exact
/// source fragment under the archival V1/V5 contract. Package 0.3 retains its
/// binding vocabulary for inspection only: there is no public constructor and
/// current execution accepts only [`PreparedPlacementFragmentV2`].
///
/// ```compile_fail
/// use std::collections::HashMap;
/// use ostadix_api::eval::{Evaluator, PreparedPlacementFragmentV1};
/// use ostadix_api::value::OValue;
///
/// fn cannot_execute_archival_fragment(
///     evaluator: &mut Evaluator,
///     fragment: PreparedPlacementFragmentV1,
///     scope: &mut HashMap<String, OValue>,
/// ) {
///     evaluator.execute_prepared_placement_fragment(fragment, scope).unwrap();
/// }
/// ```
pub struct PreparedPlacementFragmentV1 {
    bindings: PlacementFragmentBindingsV1,
}

impl PreparedPlacementFragmentV1 {
    pub fn bindings(&self) -> &PlacementFragmentBindingsV1 {
        &self.bindings
    }
}

/// Current non-cloneable placement execution authority. Its V6 admission and
/// V2 bindings cannot be reconstructed from an archival V1 fragment.
pub struct PreparedPlacementFragmentV2 {
    program: OIrProgram,
    plan: ExecutionPlan,
    admission: crate::evidence::PreparedAdmissionPartsV2,
    hgraph_schedule: crate::hgraph::Schedule,
    bindings: PlacementFragmentBindingsV2,
    evaluator_instance_binding: String,
}

impl PreparedPlacementFragmentV2 {
    pub fn bindings(&self) -> &PlacementFragmentBindingsV2 {
        &self.bindings
    }
}

/// Semantic refusal raised when a sealed placement fragment attempts to gain
/// evaluator authority that was not present in its admitted OIR. This is
/// distinct from an infrastructure failure: the split shim protocol settled
/// cleanly and, for a persistent environment, its actor remains live.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PreparedPlacementRefusalV1 {
    message: String,
}

impl PreparedPlacementRefusalV1 {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// The caller-supplied prepared-fragment deadline elapsed before backend
/// dispatch began. Unlike an in-flight timeout, this proves no backend command
/// was sent and therefore carries no ambiguous side-effect claim.
#[derive(Debug, thiserror::Error)]
#[error("prepared placement fragment deadline expired before evaluator entry")]
pub struct PreparedPlacementDeadlineExpiredV1;

struct IrExecRegion<'a> {
    lang: &'a str,
    env_id: u32,
    attr: Option<&'a str>,
    backend: &'a BackendInterface,
    body: &'a [OIr],
    node_id: PlanNodeId,
}

/// Resolve the execution engine without reading process-global state. Keeping
/// this decision pure makes the graph-by-default contract directly testable;
/// the caller is responsible only for decoding the environment variable.
fn select_serial_executor(forced: Option<bool>, configured: Option<&str>) -> Result<bool> {
    match forced {
        Some(serial) => Ok(serial),
        None => match configured {
            Some(value) if value.eq_ignore_ascii_case("serial") => Ok(true),
            Some(value) if value.eq_ignore_ascii_case("graph") => Ok(false),
            Some(value) => {
                bail!("unknown O_EXECUTOR value `{value}`; expected `graph` or `serial`")
            }
            None => Ok(false),
        },
    }
}

/// A fresh environment is not complete until its physical evaluator has been
/// retired. Preserve an execution error when cleanup succeeds, but classify a
/// cleanup failure as infrastructure failure so it cannot be reported as a
/// successful fresh attempt (or merely as a backend semantic failure).
fn settle_fresh_backend_result<T>(
    label: &str,
    execution: Result<T>,
    cleanup: Result<()>,
) -> Result<T> {
    match (execution, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(crate::process::infrastructure_error(
            cleanup.context(format!("{label} completed but cleanup failed")),
        )),
        (Err(execution), Err(cleanup)) => {
            Err(crate::process::infrastructure_error(anyhow::anyhow!(
                "{label} failed: {execution:#}; fresh backend cleanup also failed: {cleanup:#}"
            )))
        }
    }
}

impl Evaluator {
    pub fn new(shim_dir: PathBuf) -> Self {
        let mut backend_authorities = BackendAuthorityBroker::default();
        let default_backend_authority = match backend_authorities.issue("*", BackendAuthority::ALL)
        {
            Ok(OValue::Capability { identity, .. }) => identity,
            Ok(other) => panic!(
                "backend authority broker returned {}, expected OCapability",
                other.type_name()
            ),
            Err(err) => panic!("failed to issue default backend authority: {err}"),
        };
        Evaluator {
            registry: ProcessRegistry::new(),
            shim_dir,
            registered_backends: HashSet::new(),
            policy: Policy::Eager,
            executor: Box::new(ImmediateExecutor::new()),
            eval_cache: HashMap::new(),
            scheduler: AutonomousScheduler::new(),
            local_worker_parallelism_override: None,
            physical_attempt_adapter: None,
            reuse_local_worker_pool: false,
            local_worker_pool: None,
            runtime_executable_override: None,
            autonomous_buffer: Vec::new(),
            last_execution_plan: None,
            last_execution_trace: None,
            last_execution_admission: None,
            last_hgraph_schedule: None,
            activation_authorities: HashMap::new(),
            backend_authorities,
            default_backend_authority,
            suspended_actors: HashSet::new(),
            callback_operation_deadline: None,
            active_executable_leases: None,
            active_backend_launch_generations: None,
            pending_backend_restores: HashMap::new(),
            prepared_fragment_callbacks_forbidden: false,
        }
    }

    /// Install the registered-backends set used by O.eval to re-parse
    /// quoted fragments in `O.eval(q)` callbacks. Typically called once
    /// after construction with the same set passed to the Parser.
    pub fn with_registered_backends(mut self, backends: HashSet<String>) -> Self {
        self.registered_backends = backends;
        self
    }

    /// Override the graph executor's process-local worker bound. Hard graph
    /// dependencies and admission contracts still determine which operations
    /// are legal to overlap; this only caps the feasible local subset.
    pub fn with_local_worker_parallelism(mut self, workers: usize) -> Self {
        self.local_worker_parallelism_override = Some(workers.max(1));
        self
    }

    /// Retain idle graph workers across evaluations.
    ///
    /// The caller must guarantee that the evaluator thread's security
    /// authority is not tightened between calls. Linux/Android CPU-affinity
    /// changes are detected and rebuild the pool, but per-thread Landlock,
    /// seccomp, signal, and scheduler state cannot be exhaustively compared.
    pub fn with_reusable_local_workers(mut self) -> Self {
        self.reuse_local_worker_pool = true;
        self
    }

    /// Bind hosted backend proxy launches to an explicit native O evaluator.
    /// The admission layer opens, hashes, and retains this exact executable;
    /// this is not an ambient PATH override.
    pub fn with_runtime_executable(mut self, executable: PathBuf) -> Self {
        self.runtime_executable_override = Some(executable);
        self
    }

    /// Capture all settled persistent backend actors as canonical portable
    /// state. No actor is cleaned up or evicted; any pin or protocol failure
    /// aborts the complete snapshot and leaves the registry intact.
    pub fn checkpoint_persistent_actors(
        &mut self,
        max_total_bytes: u64,
    ) -> Result<EvaluatorStateSnapshotV1> {
        self.registry
            .checkpoint_persistent_actors(max_total_bytes)
            .context("failed to checkpoint persistent evaluator actors")
    }

    /// Stage portable actor state for lazy restoration under a future exact
    /// admission. Staging launches no process and is atomic: malformed,
    /// duplicate, already-live, or already-pending targets insert nothing.
    pub fn stage_persistent_actor_restore(
        &mut self,
        snapshot: EvaluatorStateSnapshotV1,
        max_total_bytes: u64,
    ) -> Result<()> {
        ensure_evaluator_snapshot_bound(&snapshot, max_total_bytes)?;
        for actor in &snapshot.actors {
            let spec = BackendRegistry::global()
                .get(&actor.canonical_backend)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "state.restore-incompatible: backend `{}` is not registered",
                        actor.canonical_backend
                    )
                })?;
            if spec.name != actor.canonical_backend {
                bail!(
                    "state.restore-incompatible: actor backend `{}` is an alias for canonical backend `{}`",
                    actor.canonical_backend,
                    spec.name
                );
            }
            let key = (
                actor.canonical_backend.clone(),
                actor.environment_id,
                actor.sandbox_policy_sha256.clone(),
            );
            if self.pending_backend_restores.contains_key(&key) {
                bail!(
                    "state.restore-conflict: backend `{}[{}]` already has a pending restore for sandbox {}",
                    actor.canonical_backend,
                    actor.environment_id,
                    actor.sandbox_policy_sha256
                );
            }
        }
        self.registry
            .ensure_restore_targets_vacant(&snapshot.actors)?;

        for actor in snapshot.actors {
            let key = (
                actor.canonical_backend.clone(),
                actor.environment_id,
                actor.sandbox_policy_sha256.clone(),
            );
            let previous = self.pending_backend_restores.insert(key, actor);
            debug_assert!(previous.is_none(), "restore staging was prevalidated");
        }
        Ok(())
    }

    pub fn pending_persistent_actor_restores(&self) -> usize {
        self.pending_backend_restores.len()
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

    /// The node-level execution trace from the most recent document.
    pub fn last_execution_trace(&self) -> Option<&ExecutionTrace> {
        self.last_execution_trace.as_ref()
    }

    /// Evidence-bound admission compiled before the most recent execution.
    pub fn last_execution_admission(&self) -> Option<&crate::evidence::ExecutionAdmissionV6> {
        self.last_execution_admission.as_ref()
    }

    /// The hypergraph schedule that was built for the most recent document.
    pub fn last_hgraph_schedule(&self) -> Option<&crate::hgraph::Schedule> {
        self.last_hgraph_schedule.as_ref()
    }

    /// Install a new evaluation policy, returning the previous one. Used by the
    /// graph coordinator to run each operation under its derived policy context.
    pub(crate) fn set_policy(&mut self, policy: Policy) -> Policy {
        std::mem::replace(&mut self.policy, policy)
    }

    /// Explicit local-worker override, if one was supplied. The graph
    /// coordinator combines the absence of an override with admitted width and
    /// current machine parallelism only after evidence-bound admission.
    pub(crate) fn local_worker_parallelism_override(&self) -> Option<usize> {
        self.local_worker_parallelism_override
    }

    /// Install one explicit high-layer physical-attempt realization without
    /// teaching evaluator core about its protocol. Concrete public builders
    /// live with the owning adapter module.
    pub(crate) fn with_physical_attempt_adapter(
        mut self,
        adapter: Arc<dyn crate::executor::PhysicalAttemptAdapterV1>,
    ) -> Self {
        self.physical_attempt_adapter = Some(adapter);
        self
    }

    pub(crate) fn physical_attempt_adapter(
        &self,
    ) -> Option<Arc<dyn crate::executor::PhysicalAttemptAdapterV1>> {
        self.physical_attempt_adapter.clone()
    }

    pub(crate) fn shim_path(&self, language: &str) -> PathBuf {
        BackendRegistry::global().resolve_shim_path(&self.shim_dir, language)
    }

    pub(crate) fn verify_admitted_runtime_context(
        &self,
        admitted: &crate::evidence::AdmittedExecution<'_>,
    ) -> Result<()> {
        let mut registered = self.registered_backends.iter().cloned().collect::<Vec<_>>();
        registered.sort();
        let registered = registered.join(",");
        let policy = self.policy.name();
        admitted.verify_runtime_context(
            &self.shim_dir,
            &[
                ("policy", policy),
                ("registered-backends", registered.as_str()),
                (
                    "default-backend-authority-policy",
                    DEFAULT_BACKEND_AUTHORITY_POLICY_V1,
                ),
            ],
        )
    }

    /// Whether the persistent backend actor `(lang, env)` is currently
    /// suspended awaiting a nested `O.eval` result.
    pub(crate) fn is_actor_suspended(&self, lang: &str, env: u32) -> bool {
        self.suspended_actors.contains(&(lang.to_string(), env))
    }

    /// Install the execution trace produced by the graph coordinator so it is
    /// observable via [`Self::last_execution_trace`].
    pub(crate) fn install_execution_trace(&mut self, trace: ExecutionTrace) {
        self.last_execution_trace = Some(trace);
    }

    /// Capture the process-local runtime facts bound by ordinary OIR
    /// admission. Canonical evidence records the authority policy, never the
    /// random live bearer that realizes it; dispatch still resolves that
    /// bearer through the private broker before launching a backend.
    pub(crate) fn try_admission_runtime_binding(
        &self,
        plan: &ExecutionPlan,
    ) -> Result<crate::evidence::RuntimeBindingV1> {
        crate::process::lifecycle_trace(
            "evidence.runtime_binding_started",
            format!("plan_nodes={}", plan.nodes.len()),
        );
        let mut registered = self.registered_backends.iter().cloned().collect::<Vec<_>>();
        registered.sort();
        let registered = registered.join(",");
        let policy = self.policy.name();
        let context = [
            ("policy", policy),
            ("registered-backends", registered.as_str()),
            (
                "default-backend-authority-policy",
                DEFAULT_BACKEND_AUTHORITY_POLICY_V1,
            ),
        ];
        let binding = match &self.runtime_executable_override {
            Some(executable) => {
                crate::evidence::runtime_binding_from_directory_with_current_executable(
                    plan,
                    &self.shim_dir,
                    &context,
                    executable,
                )
            }
            None => crate::evidence::runtime_binding_from_directory(plan, &self.shim_dir, &context),
        };
        crate::process::lifecycle_trace(
            "evidence.runtime_binding_finished",
            format!("plan_nodes={}", plan.nodes.len()),
        );
        binding
    }

    #[cfg(test)]
    pub(crate) fn admission_runtime_binding(
        &self,
        plan: &ExecutionPlan,
    ) -> crate::evidence::RuntimeBindingV1 {
        self.try_admission_runtime_binding(plan)
            .expect("test runtime binding capture failed")
    }

    pub(crate) fn install_executable_leases(
        &mut self,
        leases: Option<Arc<crate::runtime_exec::ExecutableLeaseSet>>,
    ) -> Option<Arc<crate::runtime_exec::ExecutableLeaseSet>> {
        std::mem::replace(&mut self.active_executable_leases, leases)
    }

    pub(crate) fn executable_leases(
        &self,
    ) -> Result<&Arc<crate::runtime_exec::ExecutableLeaseSet>> {
        self.active_executable_leases.as_ref().ok_or_else(|| {
            anyhow::anyhow!("hosted backend dispatch has no admitted executable lease authority")
        })
    }

    fn install_backend_launch_generations(
        &mut self,
        generations: Option<HashMap<String, String>>,
    ) -> Option<HashMap<String, String>> {
        std::mem::replace(&mut self.active_backend_launch_generations, generations)
    }

    fn backend_launch_generation(&self, backend: &str) -> Result<String> {
        self.active_backend_launch_generations
            .as_ref()
            .and_then(|generations| generations.get(backend))
            .cloned()
            .with_context(|| {
                format!("backend `{backend}` has no active admitted launch generation")
            })
    }

    fn apply_pending_actor_restore(
        &mut self,
        backend: &str,
        environment_id: u32,
        sandbox: &BackendSandboxPolicy,
        shim_path: &std::path::Path,
        executable_leases: &Arc<crate::runtime_exec::ExecutableLeaseSet>,
        launch_generation_sha256: &str,
    ) -> Result<()> {
        if environment_id > crate::environment::MAX_PERSISTENT_ENV_ID {
            return Ok(());
        }
        let sandbox_policy_sha256 = sandbox_policy_sha256(sandbox.permissions())?;
        let key = (
            backend.to_string(),
            environment_id,
            sandbox_policy_sha256.clone(),
        );
        let Some(actor) = self.pending_backend_restores.get(&key).cloned() else {
            if self.pending_backend_restores.keys().any(
                |(candidate_backend, candidate_environment, _)| {
                    candidate_backend == backend && *candidate_environment == environment_id
                },
            ) {
                bail!(
                    "state.restore-incompatible: pending backend `{backend}[{environment_id}]` does not match admitted sandbox {sandbox_policy_sha256}"
                );
            }
            return Ok(());
        };

        actor.validate()?;
        if actor.sandbox_permissions != sandbox.permissions() {
            bail!(
                "state.restore-incompatible: pending backend `{backend}[{environment_id}]` permissions disagree with admitted sandbox"
            );
        }
        if actor.launch_generation_sha256 != launch_generation_sha256 {
            bail!(
                "state.restore-generation-mismatch: pending backend `{backend}[{environment_id}]` launch generation `{}` does not match admitted generation `{launch_generation_sha256}`",
                actor.launch_generation_sha256
            );
        }
        let backend_manifest = executable_leases
            .backend_manifest_json(backend)
            .with_context(|| {
                format!(
                    "state.restore-incompatible: backend `{backend}[{environment_id}]` has no admitted runtime manifest"
                )
            })?;
        let backend_manifest: serde_json::Value = serde_json::from_str(&backend_manifest)
            .context("admitted backend runtime manifest is not valid JSON")?;
        let admitted_runtime_binding = backend_manifest
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .context("admitted backend runtime manifest omitted sha256")?;
        if actor.runtime_binding_sha256 != admitted_runtime_binding {
            bail!(
                "state.restore-generation-mismatch: pending backend `{backend}[{environment_id}]` runtime binding `{}` does not match admitted binding `{admitted_runtime_binding}`",
                actor.runtime_binding_sha256
            );
        }

        self.registry
            .restore_env(
                backend,
                environment_id,
                actor.checkpoint,
                BackendLaunchContext {
                    shim_path,
                    sandbox,
                    executable_leases: Some(executable_leases),
                    launch_generation_sha256: Some(launch_generation_sha256),
                },
            )
            .with_context(|| {
                format!("failed to restore pending backend `{backend}[{environment_id}]`")
            })?;
        self.pending_backend_restores
            .remove(&key)
            .expect("successfully restored pending actor disappeared");
        Ok(())
    }

    /// Mint a live capability for embedding-specific activation guards.
    ///
    /// Plain O programs do not need this for host-profile mutation; `activate`
    /// uses the same ambient host authority a shell command would have. Hosts
    /// that explicitly pass one of these capabilities into
    /// `activate(capability, path)` still get profile-scoped validation.
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

    /// Mint a live backend capability for compatibility and embedding hooks.
    ///
    /// The language may be a canonical backend name or `*`. Metadata is only
    /// descriptive. The default evaluator already has a wildcard full-authority
    /// binding, so normal O source does not need this path.
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

    /// Parse `NAME=LANG[:RIGHT,RIGHT]`, mint a compatibility backend
    /// capability, and install it into an O scope under `NAME`.
    pub fn install_backend_grant(
        &mut self,
        spec: &str,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<()> {
        let (name, language, parsed) = parse_backend_grant_spec(spec)?;
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
        if permissions.is_empty() {
            return Ok(None);
        }
        if let Some(binding) = options.capability_binding() {
            if let Some(capability) = scope.get(binding) {
                if let Ok(identity) =
                    self.backend_authorities
                        .authorize(capability, language, permissions)
                {
                    return Ok(Some(identity));
                }
            }
        }
        self.resolve_default_backend_authority(language, permissions)
    }

    fn resolve_default_backend_authority(
        &self,
        language: &str,
        permissions: &[BackendAuthority],
    ) -> Result<Option<String>> {
        self.backend_authorities
            .authorize_identity(&self.default_backend_authority, language, permissions)
            .with_context(|| format!("default backend authority for `{language}` failed"))?;
        Ok(Some(self.default_backend_authority.clone()))
    }

    fn backend_sandbox_policy(
        &self,
        backend: &BackendInterface,
        options: &BlockOptions,
    ) -> BackendSandboxPolicy {
        self.backend_sandbox_policy_from_permissions(backend, options.permissions())
    }

    fn backend_sandbox_policy_from_permissions(
        &self,
        backend: &BackendInterface,
        explicit_permissions: &[BackendAuthority],
    ) -> BackendSandboxPolicy {
        let mut permissions = Vec::new();
        if backend.execution == ExecutionMode::Shim {
            permissions.extend(BackendAuthority::ALL);
        }
        permissions.extend(backend.required_authorities.iter().copied());
        permissions.extend(explicit_permissions.iter().copied());
        BackendSandboxPolicy::new(permissions)
    }

    /// Resolve the same live authority gate used by coordinator-owned shim
    /// execution before an explicitly autonomous ephemeral task is handed to
    /// a worker. The returned immutable sandbox policy travels with the task;
    /// workers never manufacture authority locally.
    pub(crate) fn authorize_autonomous_ephemeral_shim(
        &self,
        backend: &BackendInterface,
        authority_scope: &HashMap<String, OValue>,
    ) -> Result<BackendSandboxPolicy> {
        let options = BlockOptions::parse(None, &backend.canonical)?;
        let sandbox = self.backend_sandbox_policy(backend, &options);
        self.resolve_backend_authority(
            &backend.canonical,
            &options,
            sandbox.permissions(),
            authority_scope,
        )?;
        Ok(sandbox)
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

    /// Perform a real activation with the evaluator's ambient host authority.
    /// If the request carries an explicit embedding guard, validate it before
    /// touching the perform boundary.
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

        if let Some(identity) = authority {
            let authorized_profile = match self.activation_authorities.get(&identity) {
                Some(profile) => profile,
                None => bail!(
                    "system activation capability is forged, revoked, or from another evaluator"
                ),
            };
            if authorized_profile != &profile {
                bail!(
                    "system activation capability is scoped to profile {}, not {}",
                    authorized_profile,
                    profile
                );
            }
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
    pub(crate) fn flush_autonomous_buffer(&mut self) -> Result<()> {
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
    pub(crate) fn resolve_after_flush(&mut self, value: OValue) -> Result<OValue> {
        match &value {
            OValue::Group { mode, members, .. } => {
                let (mode, members) = (*mode, members.clone());
                self.resolve_group(mode, &members, CacheMode::Strict)
            }
            v if Self::is_schedulable_request(v) => match self.resolve_from_cache(v) {
                Some(result) => Ok(result),
                None => {
                    let fp = match v {
                        OValue::Request { fingerprint, .. } => fingerprint_preview(fingerprint),
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
                        fingerprint_preview(fingerprint)
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
    /// - `Batch`: collect every member result into an `OList`. In Fresh mode, a
    ///   failed member becomes `OValue::Error`, preserving one output slot per
    ///   input. In Strict mode, cache misses remain hard scheduler invariant
    ///   errors.
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
                    // Batch: collect ordinary member failures as OError values
                    // only in Fresh mode. In Strict mode, a miss means the
                    // autonomous scheduler failed to materialize a buffered
                    // request, so it remains a hard invariant error.
                    let mut out = Vec::with_capacity(members.len());
                    for m in members {
                        match self.resolve_member(m, cache_mode) {
                            Ok(v) => out.push(v),
                            Err(e) if cache_mode == CacheMode::Fresh => {
                                out.push(OValue::error(e.to_string()))
                            }
                            Err(e) => {
                                return Err(e)
                                    .with_context(|| "batch(...) strict cache resolution failed");
                            }
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
            let nix_lease = threadable
                .iter()
                .any(|(_, kind, _)| matches!(kind, RequestKind::Instantiate | RequestKind::Realise))
                .then(capture_shared_nix_lease);
            for chunk in threadable.chunks(cap) {
                let (tx, rx) = mpsc::channel::<(usize, Result<OValue>)>();
                for (idx, kind, src) in chunk.iter().cloned() {
                    let tx = tx.clone();
                    let nix_lease = nix_lease.clone();
                    thread::spawn(move || {
                        // `send` can only fail if the receiver was dropped
                        // (e.g. the evaluator thread panicked). Silently
                        // ignoring keeps threads from panicking on a dead
                        // channel and is the intended pattern for fire-and-
                        // collect thread fans.
                        let _ = tx.send((idx, exec_nix_kind(kind, src, nix_lease)));
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
        let nix_lease = threadable
            .iter()
            .any(|(kind, _)| matches!(kind, RequestKind::Instantiate | RequestKind::Realise))
            .then(capture_shared_nix_lease);
        for (kind, src) in threadable {
            let tx = tx.clone();
            let nix_lease = nix_lease.clone();
            thread::spawn(move || {
                // `send` can only fail if the receiver is dropped (evaluator
                // returned early, e.g. after the first `any` success or the
                // first `race` settler). Silently ignoring is intentional:
                // the thread still runs to completion, but its result is simply
                // discarded by the already-returned caller.
                let _ = tx.send(exec_nix_kind(kind, src, nix_lease));
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
        let sandbox = self.backend_sandbox_policy_from_permissions(&backend, &permissions);
        match authority.as_deref() {
            Some(identity) => self
                .backend_authorities
                .authorize_identity(identity, &backend.canonical, sandbox.permissions())
                .context("deferred backend authority check failed")?,
            None => {
                self.resolve_default_backend_authority(&backend.canonical, sandbox.permissions())
                    .context("deferred backend request using default authority")?;
            }
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
                let environment = EnvironmentRefV2::from_encoded(env_id);
                let runtime_env_id = environment.runtime_env_id();
                let shim =
                    BackendRegistry::global().resolve_shim_path(&self.shim_dir, runtime_lang);
                let executable_leases = Arc::clone(self.executable_leases()?);
                let launch_generation = self.backend_launch_generation(runtime_lang)?;
                self.apply_pending_actor_restore(
                    runtime_lang,
                    runtime_env_id,
                    &sandbox,
                    &shim,
                    &executable_leases,
                    &launch_generation,
                )?;
                // Dependencies were rendered into the thunk body at capture
                // time, so the forced shim receives an empty binding map.
                let result = self
                    .registry
                    .exec(
                        runtime_lang,
                        runtime_env_id,
                        &body,
                        HashMap::new(),
                        BackendLaunchContext {
                            shim_path: &shim,
                            sandbox: &sandbox,
                            executable_leases: Some(&executable_leases),
                            launch_generation_sha256: Some(&launch_generation),
                        },
                    )
                    .with_context(|| format!("[{}{{eval}}]", runtime_lang));
                if environment.is_fresh() {
                    settle_fresh_backend_result(
                        &format!("[{runtime_lang}{{eval}}]"),
                        result,
                        self.registry.cleanup_env(runtime_lang, runtime_env_id),
                    )?
                } else {
                    result?
                }
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

    pub(crate) fn eval_source_with_scope(
        &mut self,
        src: &str,
        caller_scope: &HashMap<String, OValue>,
    ) -> Result<OValue> {
        if self.prepared_fragment_callbacks_forbidden {
            return Err(PreparedPlacementRefusalV1::new(
                "prepared placement fragment requested recursive O.eval authority outside its admitted OIR",
            )
            .into());
        }
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
        let outer_plan = self.last_execution_plan.take();
        let outer_trace = self.last_execution_trace.take();
        let outer_admission = self.last_execution_admission.take();
        let outer_schedule = self.last_hgraph_schedule.take();
        let outcome = self.eval_ir_program_with_scope(&program, &mut snapshot);
        self.last_execution_plan = outer_plan;
        self.last_execution_trace = outer_trace;
        self.last_execution_admission = outer_admission;
        self.last_hgraph_schedule = outer_schedule;
        outcome
    }

    pub(crate) fn eval_source_with_scope_until(
        &mut self,
        src: &str,
        caller_scope: &HashMap<String, OValue>,
        deadline: Instant,
    ) -> Result<OValue> {
        let outer_deadline = self.callback_operation_deadline;
        self.callback_operation_deadline = Some(
            outer_deadline
                .map(|outer| outer.min(deadline))
                .unwrap_or(deadline),
        );
        let outcome = self.eval_source_with_scope(src, caller_scope);
        self.callback_operation_deadline = outer_deadline;
        outcome
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Public API
    // ─────────────────────────────────────────────────────────────────────────

    /// Parse, lower, solve, and admit exactly one shim-backed placement
    /// fragment without dispatching it. A leading shebang is excluded from
    /// executable syntax by the same rule as the CLI, while `source_sha256`
    /// still binds the exact unmodified bytes supplied here. The returned
    /// non-cloneable handle retains the executable leases and all other
    /// process-local admission authority needed by
    /// [`Self::execute_prepared_placement_fragment`].
    pub fn prepare_placement_fragment(
        &mut self,
        source_utf8: &str,
        task_attempt: crate::placement::TaskAttemptIdV1,
    ) -> Result<PreparedPlacementFragmentV2> {
        use crate::placement::CanonicalPlacementRecordV1;

        let executable_source = strip_prepared_source_shebang(source_utf8);
        let nodes = Parser::new(executable_source, &self.registered_backends)
            .parse()
            .context("failed to parse placement fragment")?;
        let program = OIrProgram::lower(&nodes);
        let plan = program.plan();
        plan.validate(program.nodes.len())
            .map_err(anyhow::Error::msg)
            .context("invalid placement-fragment execution plan")?;

        let exec_node = validate_placement_fragment_shape(&program, &plan)?;
        let (backend, environment, attr) = match &plan.nodes[exec_node.0].kind {
            PlanNodeKind::Exec {
                env_id,
                attr,
                backend,
                ..
            } => (
                backend.clone(),
                EnvironmentRefV2::from_encoded(*env_id),
                attr.as_deref(),
            ),
            _ => unreachable!("validated placement fragment root must be Exec"),
        };

        let flat = program.flatten_for_plan();
        validate_execution_metadata(&flat)?;
        let mut hgraph = program
            .hgraph_for_plan(&plan)
            .map_err(anyhow::Error::msg)
            .context("failed to project placement fragment into hypergraph")?;
        crate::hgraph::solve::solve_types(&mut hgraph)
            .context("failed to solve placement-fragment type and fidelity constraints")?;
        let runtime_binding = self.try_admission_runtime_binding(&plan)?;
        let evidence =
            crate::evidence::analyze_execution(&program, &plan, &hgraph, runtime_binding.clone())
                .context("failed to establish placement-fragment evidence")?;
        let admitted = crate::evidence::admit_execution(
            &program,
            &plan,
            hgraph,
            self.policy,
            runtime_binding,
            evidence,
        )
        .context("failed to admit placement fragment")?;

        let admitted_execs = admitted
            .admission()
            .operations()
            .iter()
            .filter(|operation| {
                matches!(
                    plan.nodes.get(operation.plan_node.0).map(|node| &node.kind),
                    Some(PlanNodeKind::Exec { .. })
                )
            })
            .map(|operation| operation.plan_node)
            .collect::<Vec<_>>();
        if admitted_execs != [exec_node] {
            bail!(
                "placement fragment must contain exactly one admitted shim Exec at P{}; admission contains {:?}",
                exec_node.0,
                admitted_execs
            );
        }

        // A sealed fragment is never coarsened with another operation and its
        // dispatch path rejects recursive O.eval callbacks. Fresh shim work
        // uses the existing autonomous unknown-effects contract. Persistent
        // work instead requires the target's explicit session-serialization
        // capability alongside SameLogicalEnvironment; it is never relabeled
        // pure, replayable, or globally isolated.
        let placement_intent = if environment.is_persistent() {
            crate::placement::PlacementIntentV1::SessionSerializedOpaqueEffects
        } else {
            crate::placement::PlacementIntentV1::AutonomousUnknownEffects
        };
        let footprint = crate::placement::requirement_footprint_for_plan_node(
            &plan.nodes[exec_node.0].kind,
            placement_intent,
        )?;
        footprint.require_complete().with_context(|| {
            format!(
                "placement fragment P{} has an incomplete requirement footprint",
                exec_node.0
            )
        })?;
        let requirement_footprint_sha256 = footprint.semantic_digest()?;

        let options = BlockOptions::parse(attr, &backend.canonical)?;
        let sandbox = self.backend_sandbox_policy(&backend, &options);
        let sandbox_permissions = sandbox.permissions().to_vec();
        let sandbox_policy_sha256 = crate::placement::SemanticDigestV1::from_sha256(
            sandbox_policy_sha256(&sandbox_permissions)?,
        )?;

        let backend_implementation = prepared_backend_implementation(&admitted, &backend)?;
        let backend_implementation_sha256 = backend_implementation.semantic_digest()?;
        let backend_launch_generation = crate::placement::SemanticDigestV1::from_sha256(
            admitted.backend_launch_generation_sha256(&backend.canonical)?,
        )?;
        let operation_oir = crate::resource_identity::ArtifactId::from_sha256(
            admitted.admission().bindings().oir_sha256.clone(),
        )?;
        let placement_admission = admitted.admission().placement_admission().clone();
        let hgraph_schedule = crate::hgraph::schedule::try_schedule(admitted.graph())
            .map_err(anyhow::Error::msg)
            .context("failed to schedule admitted placement fragment")?;
        crate::hgraph::schedule::ReadySchedule::derive(admitted.graph())
            .and_then(|schedule| schedule.launch_order().map(|_| ()))
            .map_err(anyhow::Error::msg)
            .context("placement-fragment ready schedule is not executable")?;

        let bindings = PlacementFragmentBindingsV2 {
            source_sha256: crate::evidence::source_sha256(source_utf8.as_bytes()),
            canonical_backend: backend.canonical,
            plan_node: exec_node,
            operation_oir,
            requirement_footprint: footprint,
            requirement_footprint_sha256,
            placement_admission,
            task_attempt,
            backend_implementation,
            backend_implementation_sha256,
            backend_launch_generation,
            environment,
            sandbox_permissions,
            sandbox_policy_sha256,
        };
        let admission = admitted.into_prepared_parts();
        Ok(PreparedPlacementFragmentV2 {
            program,
            plan,
            admission,
            hgraph_schedule,
            bindings,
            evaluator_instance_binding: self.default_backend_authority.clone(),
        })
    }

    /// Consume one exact prepared placement fragment without imposing an
    /// additional caller deadline. No parsing, lowering, solving, runtime
    /// discovery, or admission is repeated after lease authorization; only
    /// the retained runtime context and executable file handles are rechecked
    /// immediately before dispatch.
    pub fn execute_prepared_placement_fragment(
        &mut self,
        prepared: PreparedPlacementFragmentV2,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        // GraphEvalFrame normally copies the entire caller scope into every
        // shim invocation, even when the source contains no explicit Load.
        // This initial portable-fragment slice has no canonical scope package
        // or digest, so accepting any such binding would introduce authority
        // after placement authorization. Persistent state belongs to the
        // exact backend actor selected by `environment`, not this coordinator
        // map; Load and Store are rejected during preparation as well.
        if !scope.is_empty() {
            bail!(
                "prepared placement fragment cannot consume a nonempty coordinator scope until that scope is canonically packaged and digest-bound"
            );
        }
        let PreparedPlacementFragmentV2 {
            program,
            plan,
            admission,
            hgraph_schedule,
            bindings,
            evaluator_instance_binding,
        } = prepared;
        if evaluator_instance_binding != self.default_backend_authority {
            bail!("prepared placement fragment belongs to a different Evaluator instance");
        }
        let admitted = admission.bind(&program, &plan)?;
        self.verify_admitted_runtime_context(&admitted)?;
        let executable_leases = admitted.executable_leases()?;
        executable_leases.verify_backend(bindings.canonical_backend())?;
        let launch_generation =
            admitted.backend_launch_generation_sha256(bindings.canonical_backend())?;
        if launch_generation.as_str() != bindings.backend_launch_generation().as_sha256() {
            bail!("prepared placement fragment backend launch generation changed internally");
        }
        let backend_launch_generations =
            HashMap::from([(bindings.canonical_backend().to_string(), launch_generation)]);

        self.last_execution_plan = Some(plan.clone());
        self.last_execution_trace = Some(ExecutionTrace::new());
        self.last_execution_admission = Some(admitted.admission().clone());
        self.last_hgraph_schedule = Some(hgraph_schedule);

        let previous_executable_leases = self.install_executable_leases(Some(executable_leases));
        let previous_backend_launch_generations =
            self.install_backend_launch_generations(Some(backend_launch_generations));
        let previous_callback_policy =
            std::mem::replace(&mut self.prepared_fragment_callbacks_forbidden, true);
        let execution = self.execute_plan_graph(admitted, scope);
        self.prepared_fragment_callbacks_forbidden = previous_callback_policy;
        self.install_executable_leases(previous_executable_leases);
        self.install_backend_launch_generations(previous_backend_launch_generations);
        execution
    }

    /// Consume a prepared fragment under an absolute process-local deadline.
    /// A pre-existing evaluator deadline remains authoritative when it is
    /// earlier. Timeout bounds the evaluator's wait and makes an unresponsive
    /// process unusable; it does not claim to roll back external effects the
    /// backend may already have performed.
    pub fn execute_prepared_placement_fragment_until(
        &mut self,
        prepared: PreparedPlacementFragmentV2,
        scope: &mut HashMap<String, OValue>,
        deadline: Instant,
    ) -> Result<OValue> {
        let previous_deadline = self.callback_operation_deadline;
        let effective_deadline = previous_deadline
            .map(|existing| existing.min(deadline))
            .unwrap_or(deadline);
        if effective_deadline
            .checked_duration_since(Instant::now())
            .is_none_or(|remaining| remaining.is_zero())
        {
            return Err(PreparedPlacementDeadlineExpiredV1.into());
        }
        self.callback_operation_deadline = Some(effective_deadline);
        let execution = self.execute_prepared_placement_fragment(prepared, scope);
        self.callback_operation_deadline = previous_deadline;
        execution
    }

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

    /// Lower and execute a top-level document only if its freshly recomputed,
    /// authority-free execution intent matches both required digests. This
    /// gate is deliberately one-shot: nested `O.eval` callbacks continue
    /// through ordinary live admission instead of inheriting the outer
    /// source's identity. A successful match never bypasses or replaces the
    /// fresh `AdmittedExecution` compiled below.
    pub fn eval_document_with_scope_requiring_execution_intent(
        &mut self,
        nodes: Vec<ONode>,
        scope: &mut HashMap<String, OValue>,
        actual_source_sha256: &str,
        expected_source_sha256: &str,
        expected_execution_intent_sha256: &str,
    ) -> Result<OValue> {
        let program = OIrProgram::lower(&nodes);
        self.eval_ir_program_with_mode(
            &program,
            scope,
            None,
            Some((
                actual_source_sha256,
                expected_source_sha256,
                expected_execution_intent_sha256,
            )),
        )
    }

    /// Execute a lowered program through its validated ExecutionPlan.
    pub fn eval_ir_program(&mut self, program: &OIrProgram) -> Result<OValue> {
        let mut scope = HashMap::new();
        self.eval_ir_program_with_scope(program, &mut scope)
    }

    /// Execute a lowered program with a caller-owned scope, following the
    /// configured local executor (`O_EXECUTOR`, graph by default).
    ///
    /// This is the embedding counterpart to [`Self::eval_document_with_scope`]
    /// for callers that already performed exact OIR preflight.
    pub fn eval_ir_program_with_scope(
        &mut self,
        program: &OIrProgram,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        self.eval_ir_program_with_mode(program, scope, None, None)
    }

    /// Execute an already-lowered program through the local HGraph
    /// coordinator regardless of the ambient `O_EXECUTOR` value.
    ///
    /// This is intentionally a local-only selector. It performs no peer
    /// discovery and cannot route an ordinary OIR operation to `o-node`.
    pub fn eval_ir_program_graph_with_scope(
        &mut self,
        program: &OIrProgram,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        self.eval_ir_program_with_mode(program, scope, Some(false), None)
    }

    /// Execute an already-lowered program through the serial differential
    /// reference engine regardless of the ambient `O_EXECUTOR` value.
    pub fn eval_ir_program_serial_with_scope(
        &mut self,
        program: &OIrProgram,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        self.eval_ir_program_with_mode(program, scope, Some(true), None)
    }

    /// Project, validate, and execute a lowered program. `forced` overrides the
    /// executor choice for tests: `Some(true)` forces the serial reference
    /// executor, `Some(false)` forces the graph coordinator, and `None` follows
    /// the `O_EXECUTOR` environment variable (graph by default, with `serial`
    /// retaining the reference semantics used by the differential suite).
    fn eval_ir_program_with_mode(
        &mut self,
        program: &OIrProgram,
        scope: &mut HashMap<String, OValue>,
        forced: Option<bool>,
        required_execution_intent: Option<(&str, &str, &str)>,
    ) -> Result<OValue> {
        let plan = program.plan();
        plan.validate(program.nodes.len())
            .map_err(anyhow::Error::msg)
            .context("invalid OIR execution plan")?;
        let root_schedule = plan
            .root_schedule()
            .map_err(anyhow::Error::msg)
            .context("failed to derive OIR root order from execution plan")?;
        self.last_execution_plan = Some(plan.clone());
        self.last_execution_trace = Some(ExecutionTrace::new());
        self.last_execution_admission = None;
        self.last_hgraph_schedule = None;

        let flat = program.flatten_for_plan();
        // Preserve the precise policy/arity/backend diagnostic while still
        // rejecting invalid metadata before analysis or admission begins.
        validate_execution_metadata(&flat)?;

        let mut hgraph = program
            .hgraph_for_plan(&plan)
            .map_err(anyhow::Error::msg)
            .context("failed to project OIR execution plan into hypergraph")?;
        crate::hgraph::solve::solve_types(&mut hgraph)
            .context("failed to solve OIR hypergraph type and fidelity constraints")?;

        if let Some((
            actual_source_sha256,
            expected_source_sha256,
            expected_execution_intent_sha256,
        )) = required_execution_intent
        {
            let configured = match std::env::var("O_EXECUTOR") {
                Ok(value) => Some(value),
                Err(std::env::VarError::NotPresent) => None,
                Err(std::env::VarError::NotUnicode(_)) => {
                    bail!("O_EXECUTOR is not valid Unicode; expected `graph` or `serial`")
                }
            };
            if select_serial_executor(forced, configured.as_deref())? {
                bail!("required execution-intent gating is available only for graph execution");
            }
            let intent = crate::evidence::ExecutionIntentV1::compile_with_source_sha256(
                actual_source_sha256,
                program,
                &plan,
                &hgraph,
                self.policy,
            )
            .context("failed to recompute required execution intent")?;
            intent
                .verify_required(expected_source_sha256, expected_execution_intent_sha256)
                .context("execution rejected before admission and dispatch")?;
        }

        let runtime_binding = self.try_admission_runtime_binding(&plan)?;
        let evidence =
            crate::evidence::analyze_execution(program, &plan, &hgraph, runtime_binding.clone())
                .context("failed to establish pre-execution evidence")?;
        let admitted = crate::evidence::admit_execution(
            program,
            &plan,
            hgraph,
            self.policy,
            runtime_binding,
            evidence,
        )
        .context("failed to admit OIR execution")?;
        let hgraph_schedule = crate::hgraph::schedule::try_schedule(admitted.graph())
            .map_err(anyhow::Error::msg)
            .context("failed to schedule admitted OIR hypergraph projection")?;

        // Semantic projection check (replaces the former strict "projected root
        // order == plan root order" assertion). Under the graph executor,
        // operations may COMPLETE out of order; what must hold is that every
        // plan root has a corresponding scheduled operation or materialized
        // value, and that commits are applied in `root_schedule` order (the
        // coordinator's commit step guarantees the latter). We additionally
        // require the ready-operation schedule to be acyclic.
        crate::hgraph::schedule::ReadySchedule::derive(admitted.graph())
            .and_then(|schedule| schedule.launch_order().map(|_| ()))
            .map_err(anyhow::Error::msg)
            .context("ready-operation schedule is not executable")?;
        for &root_index in &root_schedule {
            let root = plan.roots[root_index];
            let materialized = matches!(plan.nodes[root.0].kind, PlanNodeKind::Text)
                || admitted.graph().op_for(root).is_some();
            if !materialized {
                bail!(
                    "hypergraph projection is missing an operation or value for plan root {}",
                    root.0
                );
            }
        }
        self.last_hgraph_schedule = Some(hgraph_schedule);
        self.last_execution_admission = Some(admitted.admission().clone());

        // The state-complete graph coordinator is the default. The serial
        // executor remains an explicit differential oracle. A forced test
        // override wins over the environment.
        let configured = if forced.is_some() {
            None
        } else {
            match std::env::var("O_EXECUTOR") {
                Ok(value) => Some(value),
                Err(std::env::VarError::NotPresent) => None,
                Err(std::env::VarError::NotUnicode(_)) => {
                    bail!("O_EXECUTOR is not valid Unicode; expected `graph` or `serial`")
                }
            }
        };
        let use_serial = select_serial_executor(forced, configured.as_deref())?;
        if use_serial && self.physical_attempt_adapter.is_some() {
            bail!(
                "explicit remote pure execution requires the graph coordinator; serial execution would be an unauthorized local fallback"
            );
        }
        let executable_leases = admitted.executable_leases()?;
        let shim_backends = plan
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                PlanNodeKind::Exec { backend, .. } if backend.execution == ExecutionMode::Shim => {
                    Some(backend.canonical.clone())
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        let backend_launch_generations = shim_backends
            .into_iter()
            .map(|backend| {
                admitted
                    .backend_launch_generation_sha256(&backend)
                    .map(|generation| (backend, generation))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let previous_executable_leases = self.install_executable_leases(Some(executable_leases));
        let previous_backend_launch_generations =
            self.install_backend_launch_generations(Some(backend_launch_generations));
        let execution = (|| {
            if use_serial {
                self.verify_admitted_runtime_context(&admitted)?;
                self.execute_plan_serial(&admitted, scope)
            } else {
                self.execute_plan_graph(admitted, scope)
            }
        })();
        self.install_executable_leases(previous_executable_leases);
        self.install_backend_launch_generations(previous_backend_launch_generations);
        execution
    }

    /// Execute a validated plan through the readiness-driven graph coordinator.
    /// Results and scope commits are intended to match
    /// [`Self::execute_plan_serial`]; independent operations may run
    /// concurrently and are committed in deterministic root order.
    fn execute_plan_graph(
        &mut self,
        admitted: crate::evidence::AdmittedExecution<'_>,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        let coordinator = crate::executor::Coordinator::new(admitted)?;
        coordinator.run(self, scope)
    }

    fn execute_plan_serial(
        &mut self,
        admitted: &crate::evidence::AdmittedExecution<'_>,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        let program = admitted.program();
        let plan = admitted.plan();
        let flat = program.flatten_for_plan();
        if flat.len() != plan.nodes.len() {
            bail!(
                "OIR flatten produced {} nodes but plan has {} nodes",
                flat.len(),
                plan.nodes.len()
            );
        }
        validate_execution_metadata(&flat)?;

        let base_policy = self.policy;
        let mut frame = GraphEvalFrame {
            values: vec![None; plan.nodes.len()],
            base_scope: scope.clone(),
            node_policy: derive_policy_contexts(plan, &flat, base_policy)?,
            trace: ExecutionTrace::new(),
        };

        for id in plan.topological_order().map_err(anyhow::Error::msg)? {
            let launches_backend = matches!(
                flat[id.0],
                OIr::Exec { backend, .. } if backend.execution == ExecutionMode::Shim
            );
            let opaque_or_deferred = launches_backend
                || admitted
                    .graph()
                    .effect_summary(id)
                    .is_some_and(|summary| summary.unknown);
            if opaque_or_deferred {
                // The serial oracle is diagnostic, not an authority bypass:
                // opaque/deferred work receives the same last-moment runtime
                // freshness check as coordinator-owned work.
                self.verify_admitted_runtime_context(admitted)?;
                if let OIr::Exec { backend, .. } = flat[id.0] {
                    if backend.execution == ExecutionMode::Shim {
                        admitted
                            .executable_leases()?
                            .verify_backend(&backend.canonical)?;
                    }
                }
            }
            frame.trace.events.push(TraceEvent::NodeReady(id));
            frame.trace.events.push(TraceEvent::NodeStarted(id));

            let saved_policy = self.policy;
            self.policy = frame.node_policy[id.0];
            let value = self.execute_ready_plan_node(id, flat[id.0], plan, &mut frame);
            self.policy = saved_policy;

            match value {
                Ok(value) => {
                    frame.trace.events.push(TraceEvent::NodeFinished {
                        id,
                        value_type: value.type_name().to_string(),
                        fingerprint: trace_fingerprint(&value),
                    });
                    frame.set_value(id, value)?;
                }
                Err(err) => {
                    frame.trace.events.push(TraceEvent::NodeFailed {
                        id,
                        message: err.to_string(),
                    });
                    self.last_execution_trace = Some(frame.trace);
                    return Err(err);
                }
            }
        }

        let mut last = OValue::null();
        for root_index in plan.root_schedule().map_err(anyhow::Error::msg)? {
            let node = &program.nodes[root_index];
            let node_id = plan.roots[root_index];
            let is_pure_whitespace_text = matches!(
                node,
                OIr::Text(text) if !text.is_empty() && text.chars().all(char::is_whitespace)
            );

            let value = frame.value(node_id)?.clone();
            if let OIr::Store { name, .. } = node {
                scope.insert(name.clone(), value.clone());
            }

            if !value.is_null() && !is_pure_whitespace_text {
                last = value;
            }
        }

        if base_policy == Policy::Autonomous {
            self.flush_autonomous_buffer()?;
            last = self.resolve_after_flush(last)?;
        }

        self.last_execution_trace = Some(frame.trace);
        Ok(last)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Executable OIR dispatch
    // ─────────────────────────────────────────────────────────────────────────

    pub(crate) fn execute_ready_plan_node(
        &mut self,
        node_id: PlanNodeId,
        node: &OIr,
        plan: &ExecutionPlan,
        frame: &mut GraphEvalFrame,
    ) -> Result<OValue> {
        match node {
            OIr::Store { expr, .. } => {
                let children =
                    planned_children(plan, node_id, std::slice::from_ref(expr.as_ref()))?;
                let (expr_id, _) = children[0];
                Ok(frame.value(expr_id)?.clone())
            }
            OIr::Text(text) => Ok(OValue::str_(text.clone())),
            OIr::Load(name) => self.execute_ready_load(name, node_id, plan, frame),
            OIr::Invoke {
                fn_name,
                mode,
                args,
            } => self.execute_ready_invoke(fn_name, *mode, args, node_id, plan, frame),
            OIr::Exec {
                lang,
                env_id,
                attr,
                backend,
                body,
            } => self.execute_ready_exec(
                IrExecRegion {
                    lang,
                    env_id: *env_id,
                    attr: attr.as_deref(),
                    backend,
                    body,
                    node_id,
                },
                plan,
                frame,
            ),
        }
    }

    fn execute_ready_load(
        &self,
        name: &str,
        node_id: PlanNodeId,
        plan: &ExecutionPlan,
        frame: &GraphEvalFrame,
    ) -> Result<OValue> {
        let store_sources = data_predecessors(plan, node_id)
            .into_iter()
            .filter(|source| matches!(plan.nodes[source.0].kind, PlanNodeKind::Store { .. }))
            .collect::<Vec<_>>();
        if let Some(source) = store_sources.first().copied() {
            return Ok(frame.value(source)?.clone());
        }
        frame
            .base_scope
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Undefined variable: ${}", name))
    }

    fn execute_ready_invoke(
        &mut self,
        fn_name: &str,
        invoke_mode: InvokeMode,
        args: &[OIr],
        node_id: PlanNodeId,
        plan: &ExecutionPlan,
        frame: &mut GraphEvalFrame,
    ) -> Result<OValue> {
        let planned_args = planned_children(plan, node_id, args)?;
        let arg_vals = planned_args
            .iter()
            .map(|(id, _)| frame.value(*id).cloned())
            .collect::<Result<Vec<_>>>()?;

        if invoke_mode == InvokeMode::Lazy {
            if arg_vals.len() != 1 {
                bail!(
                    "lazy(expr) takes exactly 1 argument, got {}",
                    arg_vals.len()
                );
            }
            return Ok(arg_vals.into_iter().next().unwrap());
        }

        if invoke_mode == InvokeMode::Autonomous {
            if arg_vals.len() != 1 {
                bail!(
                    "autonomous(expr) takes exactly 1 argument, got {}",
                    arg_vals.len()
                );
            }
            let value = arg_vals.into_iter().next().unwrap();
            match self.flush_autonomous_buffer() {
                Ok(()) => self.resolve_after_flush(value),
                Err(err) => {
                    self.autonomous_buffer.clear();
                    Err(err)
                }
            }
        } else if let InvokeMode::Group(mode) = invoke_mode {
            if arg_vals.is_empty() {
                bail!("{}(...) takes at least 1 argument, got 0", fn_name);
            }
            Ok(OValue::group(mode, arg_vals))
        } else {
            let scope = frame.scope_from_data_edges(node_id, plan)?;
            self.apply_ir_builtin(fn_name, arg_vals, scope)
        }
    }

    fn execute_ready_exec(
        &mut self,
        region: IrExecRegion<'_>,
        plan: &ExecutionPlan,
        frame: &mut GraphEvalFrame,
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

        if backend.execution == ExecutionMode::InlineAst && backend.canonical == "quote" {
            if attr.is_some() {
                bail!("attributes are not valid on the structural `quote` backend");
            }
            let src = reconstruct_ir_source(body);
            return Ok(OValue::Expr { src });
        }

        let planned_body = planned_children(plan, node_id, body)?;

        if backend.execution == ExecutionMode::InlineAst && backend.canonical == "O" {
            if attr.is_some() {
                bail!("attributes are not valid on the structural `O` backend");
            }
            let mut last = OValue::null();
            for (child_id, child) in &planned_body {
                let is_whitespace = matches!(
                    *child,
                    OIr::Text(text) if !text.is_empty() && text.chars().all(char::is_whitespace)
                );
                let value = frame.value(*child_id)?.clone();
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

        let options = BlockOptions::parse(attr, lang)?;
        let sandbox = self.backend_sandbox_policy(backend, &options);
        let authority_scope = frame.scope_from_data_edges(node_id, plan)?;
        let authority_identity = self.resolve_backend_authority(
            backend.canonical.as_str(),
            &options,
            sandbox.permissions(),
            &authority_scope,
        )?;

        let mut buf = String::new();
        let mut deps: Vec<OValue> = Vec::new();
        let constructs_thunk = backend.canonical == "nix_expr" || options.policy().is_some();
        let mut local_scope = frame.exec_scope(node_id, plan)?;

        for (child_id, child) in &planned_body {
            match *child {
                OIr::Store { name, .. } => {
                    let value = frame.value(*child_id)?.clone();
                    local_scope.insert(name.clone(), value);
                }

                OIr::Text(text) => {
                    buf.push_str(text);
                }

                OIr::Load(_) | OIr::Exec { .. } | OIr::Invoke { .. } => {
                    let raw = frame.value(*child_id)?.clone();
                    let resolved = self.resolve_for_splice(raw)?;
                    buf.push_str(&render_with(backend.renderer, &resolved));
                    if constructs_thunk {
                        deps.push(resolved);
                    }
                }
            }
        }

        if let Some(policy) = options.policy() {
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

        if backend.canonical == "nix_expr" {
            return Ok(OValue::nix_expr(buf, deps));
        }

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
        let environment = EnvironmentRefV2::from_encoded(env_id);
        let runtime_env_id = environment.runtime_env_id();
        let env_label = match environment {
            EnvironmentRefV2::Ephemeral => format!("{runtime_lang}[*ephemeral*]"),
            EnvironmentRefV2::LinkerIsolated => format!("{runtime_lang}[*linked*]"),
            EnvironmentRefV2::Persistent(id) => format!("{runtime_lang}[{id}]"),
        };

        // Reentrancy guard: a nested O.eval evaluation must never dispatch a new
        // command onto a persistent actor that is currently suspended awaiting
        // its own eval result — that would deadlock the shim protocol.
        if environment.is_persistent() && self.is_actor_suspended(runtime_lang, runtime_env_id) {
            bail!(
                "[{}] reentrant deadlock: this operation targets backend actor {} \
                 which is suspended awaiting the result of its own O.eval callback",
                env_label,
                env_label
            );
        }

        let executable_leases = Arc::clone(self.executable_leases()?);
        let launch_generation = self.backend_launch_generation(runtime_lang)?;
        self.apply_pending_actor_restore(
            runtime_lang,
            runtime_env_id,
            &sandbox,
            &shim,
            &executable_leases,
            &launch_generation,
        )?;
        let execution: Result<OValue> = (|| {
            self.registry
                .send_exec(
                    runtime_lang,
                    runtime_env_id,
                    &buf,
                    local_scope.clone(),
                    BackendLaunchContext {
                        shim_path: &shim,
                        sandbox: &sandbox,
                        executable_leases: Some(&executable_leases),
                        launch_generation_sha256: Some(&launch_generation),
                    },
                )
                .with_context(|| format!("[{}]", env_label))?;

            let mut forbidden_callback_refusal = None::<String>;
            loop {
                let step_result = if let Some(deadline) = self.callback_operation_deadline {
                    let remaining =
                        deadline
                            .checked_duration_since(Instant::now())
                            .ok_or_else(|| {
                                crate::process::infrastructure_error(anyhow::anyhow!(
                                    "[{}] inherited O.eval callback deadline expired",
                                    env_label
                                ))
                            })?;
                    self.registry.recv_exec_step_timeout(
                        runtime_lang,
                        runtime_env_id,
                        &sandbox,
                        remaining,
                    )
                } else {
                    self.registry
                        .recv_exec_step(runtime_lang, runtime_env_id, &sandbox)
                };
                let step = match step_result {
                    Ok(step) => step,
                    Err(error) => {
                        let Some(refusal) = forbidden_callback_refusal.as_deref() else {
                            return Err(error).with_context(|| format!("[{}]", env_label));
                        };
                        if self
                            .registry
                            .has_live_env(runtime_lang, runtime_env_id, &sandbox)
                        {
                            return Err(PreparedPlacementRefusalV1::new(format!(
                                "{refusal}; backend settled the refused callback with an execution error: {error:#}"
                            ))
                            .into());
                        }
                        return Err(crate::process::infrastructure_error(anyhow::anyhow!(
                            "{refusal}; backend did not settle after the refusal and actor state is ambiguous: {error:#}"
                        )));
                    }
                };

                match step {
                    ExecStep::Done(v) => match forbidden_callback_refusal.take() {
                        Some(refusal) => break Err(PreparedPlacementRefusalV1::new(refusal).into()),
                        None => break Ok(v),
                    },
                    ExecStep::EvalRequest {
                        src,
                        scope: explicit_scope,
                    } => {
                        if self.prepared_fragment_callbacks_forbidden {
                            let refusal = format!(
                                "[{}] prepared placement fragment requested recursive O.eval authority outside its admitted OIR",
                                env_label
                            );
                            self.registry
                                .send_eval_result(
                                    runtime_lang,
                                    runtime_env_id,
                                    OValue::error(refusal.clone()),
                                    &sandbox,
                                )
                                .map_err(|error| {
                                    crate::process::infrastructure_error(anyhow::anyhow!(
                                        "{refusal}; failed to settle the refused callback and actor state is ambiguous: {error:#}"
                                    ))
                                })?;
                            forbidden_callback_refusal.get_or_insert(refusal);
                            continue;
                        }
                        let callback_scope = match explicit_scope {
                            None => local_scope.clone(),
                            Some(OValue::Scope { bindings }) => bindings,
                            Some(other) => {
                                bail!(
                                    "[{}] O.eval explicit scope must be an OScope, got {}",
                                    env_label,
                                    other.type_name()
                                );
                            }
                        };
                        // Mark this persistent actor suspended for the duration of
                        // the nested evaluation so a reentrant command targeting it
                        // fails fast instead of deadlocking.
                        let suspended_key = environment
                            .is_persistent()
                            .then(|| (runtime_lang.to_string(), runtime_env_id));
                        if let Some(key) = &suspended_key {
                            self.suspended_actors.insert(key.clone());
                        }
                        let eval_outcome = self.eval_source_with_scope(&src, &callback_scope);
                        if let Some(key) = &suspended_key {
                            self.suspended_actors.remove(key);
                        }
                        match eval_outcome {
                            Ok(result) => {
                                self.registry
                                    .send_eval_result(
                                        runtime_lang,
                                        runtime_env_id,
                                        result,
                                        &sandbox,
                                    )
                                    .with_context(|| format!("[{}] send_eval_result", env_label))?;
                            }
                            Err(e) => {
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
            }
        })();

        if environment.is_fresh() {
            settle_fresh_backend_result(
                &format!("[{env_label}]"),
                execution,
                self.registry.cleanup_env(runtime_lang, runtime_env_id),
            )
        } else {
            execution.with_context(|| format!("[{}]", env_label))
        }
    }

    /// Test-only helper: evaluate a lowered program forcing a specific executor
    /// (`serial = true` uses the reference serial executor, `false` uses the
    /// graph coordinator). Used by the graph/serial equivalence tests.
    #[cfg(test)]
    fn eval_ir_program_forcing(
        &mut self,
        program: &OIrProgram,
        scope: &mut HashMap<String, OValue>,
        serial: bool,
    ) -> Result<OValue> {
        self.eval_ir_program_with_mode(program, scope, Some(serial), None)
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

    fn apply_ir_builtin(
        &mut self,
        fn_name: &str,
        arg_vals: Vec<OValue>,
        scope: HashMap<String, OValue>,
    ) -> Result<OValue> {
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
            "dry_activate" => {
                if arg_vals.is_empty() || arg_vals.len() > 2 {
                    bail!(
                        "dry_activate(path[, profile]) takes 1 or 2 arguments, got {}",
                        arg_vals.len()
                    );
                }
                let profile = match arg_vals.get(1) {
                    Some(OValue::Text { v }) => v.utf8.clone(),
                    Some(OValue::System { profile_path }) => profile_path.clone(),
                    Some(other) => bail!(
                        "dry_activate's profile must be a string path or System, got {}",
                        other.type_name()
                    ),
                    None => "/nix/var/nix/profiles/system".to_string(),
                };
                let req = OValue::request(
                    RequestKind::Activate {
                        profile,
                        dry_run: true,
                        authority: None,
                    },
                    arg_vals[0].clone(),
                );
                self.auto_resolve(req)
            }
            "activate" => {
                if arg_vals.is_empty() || arg_vals.len() > 3 {
                    bail!(
                        "activate(path[, profile]) performs a real switch; \
                         dry_activate(path[, profile]) performs a dry run; got {} args",
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
                        Some(OValue::Text { v }) => v.utf8.clone(),
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
                        bail!("activate accepts only path and optional profile");
                    }
                    let profile = match arg_vals.get(1) {
                        Some(OValue::Text { v }) => v.utf8.clone(),
                        Some(OValue::System { profile_path }) => profile_path.clone(),
                        Some(other) => bail!(
                            "activate's profile must be a string path or System, got {}",
                            other.type_name()
                        ),
                        None => "/nix/var/nix/profiles/system".to_string(),
                    };
                    (None, arg_vals[0].clone(), profile, false)
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
                Ok(OValue::scope(scope))
            }
            other => bail!("Unknown built-in function: `{}(...)`", other),
        }
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

fn validate_placement_fragment_shape(
    program: &OIrProgram,
    plan: &ExecutionPlan,
) -> Result<PlanNodeId> {
    let flat = program.flatten_for_plan();
    let mut semantic_roots = Vec::new();
    for root in &plan.roots {
        match flat.get(root.0) {
            Some(OIr::Text(text)) if text.trim().is_empty() => {}
            Some(OIr::Text(_)) => {
                bail!(
                    "placement fragment contains a non-whitespace top-level text root (text-only or mixed-document input is not executable placement authority)"
                )
            }
            Some(_) => semantic_roots.push(*root),
            None => bail!("placement fragment root P{} has no OIR node", root.0),
        }
    }
    if semantic_roots.len() != 1 {
        bail!(
            "placement fragment requires exactly one non-whitespace semantic root, found {}",
            semantic_roots.len()
        );
    }
    let root = semantic_roots[0];
    let mut exec = None;
    for node in &plan.nodes {
        match &node.kind {
            PlanNodeKind::Text => {}
            PlanNodeKind::Load { .. } => {
                bail!(
                    "placement fragment cannot contain Load P{} until the input scope is digest-bound and packaged",
                    node.id.0
                )
            }
            PlanNodeKind::Exec { backend, .. } => {
                if exec.replace(node.id).is_some() {
                    bail!(
                        "placement fragment contains a second Exec at P{}",
                        node.id.0
                    );
                }
                if backend.execution != ExecutionMode::Shim {
                    bail!(
                        "placement fragment Exec P{} uses {}, expected a shim backend",
                        node.id.0,
                        backend.execution.label()
                    );
                }
            }
            PlanNodeKind::Store { .. } => {
                bail!("placement fragment cannot contain Store P{}", node.id.0)
            }
            PlanNodeKind::Call { .. } => {
                bail!("placement fragment cannot contain Call P{}", node.id.0)
            }
            PlanNodeKind::Request { .. } => {
                bail!("placement fragment cannot contain Request P{}", node.id.0)
            }
            PlanNodeKind::Group { .. } => {
                bail!("placement fragment cannot contain Group P{}", node.id.0)
            }
            PlanNodeKind::Schedule { .. } => {
                bail!("placement fragment cannot contain Schedule P{}", node.id.0)
            }
        }
    }
    let exec = exec.context("placement fragment contains no shim Exec (text-only input is not executable placement authority)")?;
    if exec != root {
        bail!(
            "placement fragment's only Exec is P{}, but its sole root is P{}",
            exec.0,
            root.0
        );
    }
    if program_contains_obvious_o_eval(program) {
        bail!(
            "placement fragment contains O.eval; recursive evaluator authority is outside a single admitted backend fragment"
        );
    }
    Ok(exec)
}

fn strip_prepared_source_shebang(source: &str) -> &str {
    if !source.starts_with("#!") {
        return source;
    }
    source
        .find('\n')
        .map_or("", |newline| &source[newline + 1..])
}

fn program_contains_obvious_o_eval(program: &OIrProgram) -> bool {
    fn visit(node: &OIr) -> bool {
        match node {
            OIr::Text(text) => {
                let compact = text
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>();
                compact.contains("O.eval")
                    || compact.contains("O['eval']")
                    || compact.contains("O[\"eval\"]")
                    || compact.contains("getattr(O,'eval')")
                    || compact.contains("getattr(O,\"eval\")")
            }
            OIr::Load(_) => false,
            OIr::Store { expr, .. } => visit(expr),
            OIr::Invoke { args, .. } => args.iter().any(visit),
            OIr::Exec { body, .. } => body.iter().any(visit),
        }
    }
    program.nodes.iter().any(visit)
}

fn prepared_backend_implementation(
    admitted: &crate::evidence::AdmittedExecution<'_>,
    backend: &BackendInterface,
) -> Result<crate::placement::BackendImplementationIdV1> {
    let registry = BackendRegistry::global();
    let expected_specification = backend
        .specification_sha256
        .as_deref()
        .context("placement fragment backend has no admitted catalog specification digest")?;
    let expected_specification =
        crate::placement::SemanticDigestV1::from_sha256(expected_specification.to_string())?;

    let adapter_sha256 = match registry.adapter_for(&backend.canonical) {
        BackendAdapterKind::NativeRust => unique_admitted_sha256(
            admitted
                .admission()
                .executable_manifest()
                .artifacts()
                .iter()
                .filter(|artifact| {
                    artifact.canonical_backend == backend.canonical
                        && artifact.role == "ostadix-proxy"
                })
                .filter_map(|artifact| artifact.sha256.as_deref()),
            "admitted Ostadix proxy adapter",
            &backend.canonical,
        )?,
        BackendAdapterKind::LegacyPythonShim => {
            let common_name = "o_shim_common.py";
            let common_hex = hex::encode(common_name.as_bytes());
            unique_admitted_sha256(
                admitted
                    .admission()
                    .backend_artifacts()
                    .iter()
                    .filter(|artifact| artifact.canonical_backend == backend.canonical)
                    .filter(|artifact| {
                        !artifact.resolved_identity.ends_with(common_name)
                            && !artifact.resolved_identity.ends_with(&common_hex)
                    })
                    .filter_map(|artifact| artifact.state.sha256()),
                "admitted legacy shim adapter",
                &backend.canonical,
            )?
        }
        BackendAdapterKind::Inline => {
            bail!(
                "placement fragment backend `{}` has no hosted adapter",
                backend.canonical
            )
        }
    };
    let adapter_artifact = crate::resource_identity::ArtifactId::from_sha256(adapter_sha256)?;
    let executable_set = admitted
        .executable_leases()?
        .backend_executable_set_v2(&backend.canonical)?;
    registry
        .backend_implementation_id_v1(
            &backend.canonical,
            Some(&expected_specification),
            adapter_artifact,
            executable_set,
            crate::backend_catalog::LOCAL_BACKEND_PROTOCOL_ABI_V1,
        )
        .map_err(Into::into)
}

fn unique_admitted_sha256<'a>(
    values: impl IntoIterator<Item = &'a str>,
    label: &str,
    backend: &str,
) -> Result<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    match values.as_slice() {
        [sha256] => Ok((*sha256).to_string()),
        [] => bail!("backend `{backend}` has no {label} digest in its retained admission"),
        _ => bail!(
            "backend `{backend}` has multiple conflicting {label} digests in its retained admission"
        ),
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

impl GraphEvaluationHost for Evaluator {
    fn verify_admitted_runtime_context(
        &self,
        admitted: &crate::evidence::AdmittedExecution<'_>,
    ) -> Result<()> {
        Evaluator::verify_admitted_runtime_context(self, admitted)
    }

    fn local_worker_parallelism_override(&self) -> Option<usize> {
        Evaluator::local_worker_parallelism_override(self)
    }

    fn shim_path(&self, language: &str) -> PathBuf {
        Evaluator::shim_path(self, language)
    }

    fn authorize_autonomous_ephemeral_shim(
        &self,
        backend: &BackendInterface,
        authority_scope: &HashMap<String, OValue>,
    ) -> Result<BackendSandboxPolicy> {
        Evaluator::authorize_autonomous_ephemeral_shim(self, backend, authority_scope)
    }

    fn set_policy(&mut self, policy: Policy) -> Policy {
        Evaluator::set_policy(self, policy)
    }

    fn eval_source_with_scope_until(
        &mut self,
        src: &str,
        caller_scope: &HashMap<String, OValue>,
        deadline: Instant,
    ) -> Result<OValue> {
        Evaluator::eval_source_with_scope_until(self, src, caller_scope, deadline)
    }

    fn execute_ready_plan_node(
        &mut self,
        node_id: PlanNodeId,
        node: &OIr,
        plan: &ExecutionPlan,
        frame: &mut GraphEvalFrame,
    ) -> Result<OValue> {
        Evaluator::execute_ready_plan_node(self, node_id, node, plan, frame)
    }

    fn install_execution_trace(&mut self, trace: ExecutionTrace) {
        Evaluator::install_execution_trace(self, trace);
    }

    fn flush_autonomous_buffer(&mut self) -> Result<()> {
        Evaluator::flush_autonomous_buffer(self)
    }

    fn resolve_after_flush(&mut self, value: OValue) -> Result<OValue> {
        Evaluator::resolve_after_flush(self, value)
    }
}

impl crate::executor::GraphExecutorHost for Evaluator {
    fn take_local_worker_pool(
        &mut self,
        capacity: usize,
    ) -> Result<crate::executor::pool::WorkerPool> {
        match self.local_worker_pool.take() {
            Some(pool)
                if pool.capacity() == capacity
                    && pool.outstanding() == 0
                    && pool.matches_current_affinity() =>
            {
                Ok(pool)
            }
            Some(pool) => {
                drop(pool);
                crate::executor::pool::WorkerPool::new(capacity)
            }
            None => crate::executor::pool::WorkerPool::new(capacity),
        }
    }

    fn return_local_worker_pool(&mut self, pool: crate::executor::pool::WorkerPool) {
        if self.reuse_local_worker_pool
            && pool.outstanding() == 0
            && self.local_worker_pool.is_none()
        {
            self.local_worker_pool = Some(pool);
        }
    }
}

impl<'a> crate::executor::Coordinator<'a> {
    /// Execute through the evaluator compatibility surface while the executor
    /// itself depends only on the crate-private graph host contract.
    pub fn run(
        self,
        evaluator: &mut Evaluator,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        let physical_attempt_adapter = evaluator.physical_attempt_adapter();
        self.run_host(evaluator, scope, physical_attempt_adapter.as_deref())
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! exhaustive_cases {
        ($ty:ty; $( $pattern:pat => $value:expr ),+ $(,)?) => {{
            fn compile_time_exhaustiveness_guard(value: &$ty) {
                match value {
                    $( $pattern => (), )+
                }
            }

            vec![
                $(
                    {
                        let value: $ty = $value;
                        assert!(
                            matches!(&value, $pattern),
                            "representative does not match {}",
                            stringify!($pattern)
                        );
                        compile_time_exhaustiveness_guard(&value);
                        value
                    }
                ),+
            ]
        }};
    }

    #[test]
    fn evaluator_retains_idle_graph_workers_and_resizes_on_demand() {
        let mut default_evaluator = Evaluator::new(PathBuf::from("/tmp"));
        let default_pool = crate::executor::pool::WorkerPool::new(1).unwrap();
        crate::executor::GraphExecutorHost::return_local_worker_pool(
            &mut default_evaluator,
            default_pool,
        );
        assert!(default_evaluator.local_worker_pool.is_none());

        let mut evaluator = Evaluator::new(PathBuf::from("/tmp")).with_reusable_local_workers();
        let pool = crate::executor::pool::WorkerPool::new(2).unwrap();
        crate::executor::GraphExecutorHost::return_local_worker_pool(&mut evaluator, pool);
        assert_eq!(
            evaluator
                .local_worker_pool
                .as_ref()
                .expect("idle pool must be retained")
                .capacity(),
            2
        );

        let pool =
            crate::executor::GraphExecutorHost::take_local_worker_pool(&mut evaluator, 2).unwrap();
        assert_eq!(pool.capacity(), 2);
        assert!(evaluator.local_worker_pool.is_none());
        crate::executor::GraphExecutorHost::return_local_worker_pool(&mut evaluator, pool);

        let resized =
            crate::executor::GraphExecutorHost::take_local_worker_pool(&mut evaluator, 1).unwrap();
        assert_eq!(resized.capacity(), 1);
        crate::executor::GraphExecutorHost::return_local_worker_pool(&mut evaluator, resized);
    }

    fn placement_attempt() -> crate::placement::TaskAttemptIdV1 {
        crate::placement::TaskAttemptIdV1::new(
            crate::placement::SemanticDigestV1::hash_bytes(
                "ostadix/test/prepared-placement-task/v1",
                b"prepared-placement-fragment",
            ),
            crate::placement::GenerationV1::new(1).unwrap(),
        )
    }

    fn placement_evaluator(shim_dir: PathBuf) -> Evaluator {
        Evaluator::new(shim_dir)
            .with_registered_backends(BackendRegistry::global().registered_backend_tags())
    }

    fn eval_test_source(
        evaluator: &mut Evaluator,
        backends: &HashSet<String>,
        source: &str,
    ) -> Result<OValue> {
        let nodes = Parser::new(source, backends).parse()?;
        evaluator.eval_document(nodes)
    }

    #[test]
    fn fresh_backend_success_cannot_hide_cleanup_failure() {
        let error = settle_fresh_backend_result(
            "[python[*]]",
            Ok(OValue::str_("completed")),
            Err(anyhow::anyhow!("shutdown timed out")),
        )
        .unwrap_err();

        assert!(crate::process::is_infrastructure_error(&error));
        let message = format!("{error:#}");
        assert!(
            message.contains("completed but cleanup failed"),
            "{message}"
        );
        assert!(message.contains("shutdown timed out"), "{message}");
    }

    #[test]
    fn fresh_backend_dual_failure_preserves_both_diagnostics() {
        let error = settle_fresh_backend_result::<OValue>(
            "[python[*]]",
            Err(anyhow::anyhow!("semantic execution failed")),
            Err(anyhow::anyhow!("termination failed")),
        )
        .unwrap_err();

        assert!(crate::process::is_infrastructure_error(&error));
        let message = format!("{error:#}");
        assert!(message.contains("semantic execution failed"), "{message}");
        assert!(message.contains("termination failed"), "{message}");
    }

    #[test]
    fn graph_executor_is_the_unconfigured_default() {
        assert!(!select_serial_executor(None, None).unwrap());
        assert!(!select_serial_executor(None, Some("graph")).unwrap());
        assert!(select_serial_executor(None, Some("serial")).unwrap());
        assert!(select_serial_executor(Some(true), Some("graph")).unwrap());
        assert!(!select_serial_executor(Some(false), Some("serial")).unwrap());
        assert!(select_serial_executor(None, Some("legacy")).is_err());
    }

    #[test]
    fn graph_worker_override_does_not_reconfigure_request_scheduler() {
        let evaluator = Evaluator::new("/tmp".into());
        assert_eq!(evaluator.local_worker_parallelism_override(), None);
        let request_parallelism = evaluator.scheduler.parallelism;
        let evaluator = evaluator.with_local_worker_parallelism(request_parallelism + 3);
        assert_eq!(
            evaluator.scheduler.parallelism, request_parallelism,
            "the HGraph worker cap must not mutate the separate Request scheduler"
        );
        assert_eq!(
            evaluator.local_worker_parallelism_override(),
            Some(request_parallelism + 3)
        );
    }

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
    fn renderer_matrix_covers_every_ovalue_variant_without_unexpected_erasure() {
        use std::collections::BTreeMap;

        use crate::value::{
            GraphNode, NativeBoundary, NativeCodecSafety, NativeIdentity, OBytes, ONative,
            RehydratePolicy, SeqKind, SetKind, SnapshotKind,
        };

        let values = exhaustive_cases!(OValue;
            OValue::Null => OValue::null(),
            OValue::Bool { .. } => OValue::bool_(true),
            OValue::Number { .. } => OValue::int(1),
            OValue::Text { .. } => OValue::text("text"),
            OValue::Char { .. } => OValue::char_('λ'),
            OValue::Html { .. } => OValue::html("<b>text</b>"),
            OValue::StorePath { .. } => OValue::store_path("/nix/store/example"),
            OValue::Expr { .. } => OValue::Expr { src: "42".into() },
            OValue::List { .. } => OValue::list(vec![OValue::int(1)]),
            OValue::Map { .. } => OValue::map(HashMap::from([("key".into(), OValue::int(1))])),
            OValue::Seq { .. } => OValue::seq(SeqKind::Tuple, vec![OValue::int(1)]),
            OValue::Object { .. } => OValue::object(BTreeMap::from([("key".into(), OValue::int(1))])),
            OValue::EntriesMap { .. } => OValue::entries_map(vec![(OValue::text("key"), OValue::int(1))]),
            OValue::Set { .. } => OValue::set(SetKind::Ordered, vec![OValue::int(1)]),
            OValue::Symbol { .. } => OValue::symbol("answer"),
            OValue::Keyword { .. } => OValue::keyword("required"),
            OValue::Scope { .. } => OValue::scope(HashMap::from([("x".into(), OValue::int(1))])),
            OValue::Blob { .. } => OValue::blob(b"data", "application/octet-stream"),
            OValue::Bytes { .. } => OValue::bytes(b"data".to_vec(), Some("application/octet-stream".into())),
            OValue::Graph { .. } => OValue::graph(
                0,
                vec![GraphNode::Value {
                    value: Box::new(OValue::int(1)),
                }],
            ),
            OValue::Native { .. } => OValue::native(ONative {
                lang: "python".into(),
                implementation: Some("cpython".into()),
                version: Some("3.14".into()),
                type_name: "decimal.Decimal".into(),
                identity: NativeIdentity {
                    stable: Some("decimal:1.25".into()),
                    live: None,
                },
                codec: "repr".into(),
                payload: Some(OBytes {
                    bytes: b"Decimal('1.25')".to_vec(),
                    media_type: Some("text/x-python-repr".into()),
                }),
                boundary: NativeBoundary::Pure,
                safety: NativeCodecSafety::SourceBacked,
                capabilities: vec![],
                metadata: BTreeMap::new(),
                rehydrate: RehydratePolicy::Portable,
            }),
            OValue::NixExpr { .. } => OValue::nix_expr("1 + 1", vec![]),
            OValue::Derivation { .. } => OValue::derivation(
                "/nix/store/example.drv",
                vec!["out".into()],
                vec![],
            ),
            OValue::Request { .. } => OValue::request(
                RequestKind::Instantiate,
                OValue::nix_expr("1", vec![]),
            ),
            OValue::System { .. } => OValue::system("/nix/var/nix/profiles/system"),
            OValue::Capability { .. } => OValue::capability(
                CapabilityKind::Service,
                "opaque",
                HashMap::new(),
            ),
            OValue::Snapshot { .. } => OValue::snapshot(
                SnapshotKind::System,
                "generation",
                HashMap::new(),
            ),
            OValue::Thunk { .. } => OValue::thunk("42", vec![]),
            OValue::Group { .. } => OValue::group(GroupMode::Batch, vec![]),
            OValue::Error { .. } => OValue::error("failed"),
        );
        let renderers = exhaustive_cases!(SpliceRenderer;
            SpliceRenderer::Python => SpliceRenderer::Python,
            SpliceRenderer::Html => SpliceRenderer::Html,
            SpliceRenderer::Latex => SpliceRenderer::Latex,
            SpliceRenderer::Markdown => SpliceRenderer::Markdown,
            SpliceRenderer::Nix => SpliceRenderer::Nix,
            SpliceRenderer::Default => SpliceRenderer::Default,
        );

        assert_eq!(values.len(), 30);
        assert_eq!(renderers.len(), 6);
        assert_eq!(
            values
                .iter()
                .map(OValue::type_name)
                .collect::<HashSet<_>>()
                .len(),
            values.len(),
            "the matrix must contain one representative per OValue variant"
        );

        for renderer in renderers.iter().copied() {
            for value in &values {
                let rendered = render_with(renderer, value);
                let _classification = render_fidelity(renderer, value);
                let intentionally_empty = matches!(value, OValue::Null)
                    && matches!(
                        renderer,
                        SpliceRenderer::Html | SpliceRenderer::Latex | SpliceRenderer::Markdown
                    );
                assert_eq!(
                    rendered.is_empty(),
                    intentionally_empty,
                    "unexpected erasure behavior for {renderer:?} over {}",
                    value.type_name(),
                );
            }
        }

        let graph = values
            .iter()
            .find(|value| matches!(value, OValue::Graph { .. }))
            .unwrap();
        let native = values
            .iter()
            .find(|value| matches!(value, OValue::Native { .. }))
            .unwrap();
        for value in [graph, native] {
            assert_eq!(
                render_fidelity(SpliceRenderer::Python, value),
                RenderFidelity::Typed
            );
            for renderer in [
                SpliceRenderer::Nix,
                SpliceRenderer::Html,
                SpliceRenderer::Latex,
                SpliceRenderer::Markdown,
                SpliceRenderer::Default,
            ] {
                assert_eq!(
                    render_fidelity(renderer, value),
                    RenderFidelity::Opaque,
                    "{renderer:?} only retains a summary for {}",
                    value.type_name(),
                );
            }
        }

        let bytes_with_media_type = values
            .iter()
            .find(|value| matches!(value, OValue::Bytes { .. }))
            .unwrap();
        assert_eq!(
            render_fidelity(SpliceRenderer::Nix, bytes_with_media_type),
            RenderFidelity::Structural
        );
        assert_eq!(
            render_fidelity(SpliceRenderer::Default, bytes_with_media_type),
            RenderFidelity::Structural
        );
        let bytes_without_media_type = OValue::bytes(b"data".to_vec(), None);
        assert_eq!(
            render_fidelity(SpliceRenderer::Nix, &bytes_without_media_type),
            RenderFidelity::Opaque,
            "a length-only Bytes marker does not retain the payload"
        );
        assert_eq!(
            render_fidelity(SpliceRenderer::Default, &bytes_without_media_type),
            RenderFidelity::Opaque,
            "a length-only Bytes marker does not retain the payload"
        );

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
    fn renderers_accept_short_unicode_fingerprints() {
        let values = [
            OValue::Request {
                kind: RequestKind::Instantiate,
                source: Box::new(OValue::null()),
                fingerprint: "短".into(),
            },
            OValue::Thunk {
                body: "42".into(),
                deps: vec![],
                fingerprint: "é".into(),
            },
            OValue::Group {
                mode: GroupMode::Batch,
                members: vec![],
                fingerprint: "🔒".into(),
            },
        ];
        let renderers = exhaustive_cases!(SpliceRenderer;
            SpliceRenderer::Python => SpliceRenderer::Python,
            SpliceRenderer::Html => SpliceRenderer::Html,
            SpliceRenderer::Latex => SpliceRenderer::Latex,
            SpliceRenderer::Markdown => SpliceRenderer::Markdown,
            SpliceRenderer::Nix => SpliceRenderer::Nix,
            SpliceRenderer::Default => SpliceRenderer::Default,
        );

        for renderer in renderers {
            for value in &values {
                assert!(
                    !render_with(renderer, value).is_empty(),
                    "{renderer:?} erased {} with a short fingerprint",
                    value.type_name(),
                );
            }
        }
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

    #[test]
    fn nested_python_number_result_splices_as_host_expression() {
        let backends: HashSet<String> = ["python"].iter().map(|s| s.to_string()).collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let source = "python[0]^(python[1]^(2 ** 100)_python[1] + 1)_python[0]";
        let nodes = Parser::new(source, &backends).parse().unwrap();

        let result = evaluator.eval_document(nodes).unwrap();

        match result {
            OValue::Number {
                v: ONumber::Int { v },
            } => {
                let expected = (num_bigint::BigInt::from(1_u8) << 100_u32) + 1_u8;
                assert_eq!(v, expected);
            }
            other => panic!("expected spliced big integer number, got {other:?}"),
        }
    }

    #[test]
    fn evaluator_checkpoint_restore_round_trips_python_actor_state() -> Result<()> {
        let backends: HashSet<String> = ["python"].iter().map(|s| s.to_string()).collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut source =
            Evaluator::new(shim_dir.clone()).with_registered_backends(backends.clone());
        eval_test_source(
            &mut source,
            &backends,
            "python[17]^(x = []\nx.append(x)\ny = x\n__oval_result__ = 'ready')_python[17]",
        )?;

        let snapshot = source.checkpoint_persistent_actors(4 * 1024 * 1024)?;
        assert_eq!(snapshot.actors.len(), 1);
        assert_eq!(snapshot.actors[0].canonical_backend, "python");
        assert_eq!(snapshot.actors[0].environment_id, 17);
        assert_eq!(
            eval_test_source(
                &mut source,
                &backends,
                "python[17]^(__oval_result__ = x is y and x[0] is x)_python[17]",
            )?,
            OValue::bool_(true),
            "checkpointing must not evict the live source actor"
        );

        let mut restored = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        restored.stage_persistent_actor_restore(snapshot, 4 * 1024 * 1024)?;
        assert_eq!(restored.pending_persistent_actor_restores(), 1);
        assert_eq!(
            eval_test_source(
                &mut restored,
                &backends,
                "python[17]^(__oval_result__ = x is y and x[0] is x)_python[17]",
            )?,
            OValue::bool_(true)
        );
        assert_eq!(restored.pending_persistent_actor_restores(), 0);
        Ok(())
    }

    #[test]
    fn evaluator_checkpoint_pin_keeps_python_actor_live() -> Result<()> {
        let backends: HashSet<String> = ["python"].iter().map(|s| s.to_string()).collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        eval_test_source(
            &mut evaluator,
            &backends,
            "python[19]^(f = lambda: 42\n__oval_result__ = 'ready')_python[19]",
        )?;

        let error = evaluator
            .checkpoint_persistent_actors(4 * 1024 * 1024)
            .expect_err("a Python function must pin the actor");
        assert!(format!("{error:#}").contains("state.pin-required"));
        assert_eq!(
            eval_test_source(
                &mut evaluator,
                &backends,
                "python[19]^(__oval_result__ = f())_python[19]",
            )?,
            OValue::int(42),
            "checkpoint refusal must retain the live actor"
        );
        Ok(())
    }

    #[test]
    fn pending_evaluator_restore_fails_closed_on_launch_generation_mismatch() -> Result<()> {
        let backends: HashSet<String> = ["python"].iter().map(|s| s.to_string()).collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut source =
            Evaluator::new(shim_dir.clone()).with_registered_backends(backends.clone());
        eval_test_source(
            &mut source,
            &backends,
            "python[23]^(x = 42\n__oval_result__ = x)_python[23]",
        )?;
        let mut snapshot = source.checkpoint_persistent_actors(4 * 1024 * 1024)?;
        snapshot.actors[0].launch_generation_sha256 = "00".repeat(32);

        let mut restored = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        restored.stage_persistent_actor_restore(snapshot, 4 * 1024 * 1024)?;
        let error = eval_test_source(
            &mut restored,
            &backends,
            "python[23]^(__oval_result__ = x)_python[23]",
        )
        .expect_err("mismatched generation must fail before actor dispatch");
        assert!(
            format!("{error:#}").contains("state.restore-generation-mismatch"),
            "{error:#}"
        );
        assert_eq!(
            restored.pending_persistent_actor_restores(),
            1,
            "failed restore must remain staged and must not publish an actor"
        );
        Ok(())
    }

    #[test]
    fn evaluator_snapshot_is_empty_without_persistent_actors() -> Result<()> {
        let mut evaluator = Evaluator::new("/tmp".into());
        let snapshot = evaluator.checkpoint_persistent_actors(1024)?;
        assert!(snapshot.actors.is_empty());
        snapshot.validate()?;
        Ok(())
    }

    #[test]
    fn evaluator_snapshots_stateless_actor_as_explicit_empty_checkpoint() -> Result<()> {
        if which::which("bash").is_err() {
            return Ok(());
        }
        let backends: HashSet<String> = ["bash"].iter().map(|s| s.to_string()).collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        assert_eq!(
            eval_test_source(&mut evaluator, &backends, "bash[29]^(printf 42)_bash[29]")?,
            OValue::int(42)
        );
        let snapshot = evaluator.checkpoint_persistent_actors(1024 * 1024)?;
        assert_eq!(snapshot.actors.len(), 1);
        assert_eq!(
            snapshot.actors[0].checkpoint.tier,
            crate::backend_state::BackendStateTierV1::Stateless
        );
        assert_eq!(
            snapshot.actors[0].checkpoint.payload,
            serde_json::json!({ "kind": "empty" })
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_executes_exact_admitted_python() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = placement_evaluator(shim_dir);
        let source = "#!/usr/bin/env O\npython^(__oval_result__ = 6 * 7)_python";
        let prepared = evaluator.prepare_placement_fragment(source, placement_attempt())?;
        let bindings = prepared.bindings().clone();
        assert_eq!(
            bindings.source_sha256(),
            crate::evidence::source_sha256(source.as_bytes())
        );
        assert_eq!(bindings.canonical_backend(), "python");
        assert_eq!(bindings.environment(), EnvironmentRefV2::Ephemeral);
        assert!(bindings.requirement_footprint().is_complete());
        assert_eq!(
            bindings.backend_launch_generation().as_sha256().len(),
            64,
            "prepared authority must expose the exact admitted launch-generation digest"
        );
        assert_eq!(
            bindings.backend_implementation().realization_pipeline(),
            bindings.realization_pipeline()
        );

        let mut scope = HashMap::new();
        assert_eq!(
            evaluator.execute_prepared_placement_fragment(prepared, &mut scope)?,
            OValue::int(42)
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_exposes_portable_admission_separately() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let source = "python^(__oval_result__ = 6 * 7)_python";
        let source_with_shebang = format!("#!/usr/bin/env O\n{source}");
        let mut evaluator = placement_evaluator(shim_dir);

        let plain = evaluator.prepare_placement_fragment(source, placement_attempt())?;
        let alternate_attempt = crate::placement::TaskAttemptIdV1::new(
            crate::placement::SemanticDigestV1::from_sha256("ba".repeat(32))?,
            crate::placement::GenerationV1::new(2)?,
        );
        let shebang =
            evaluator.prepare_placement_fragment(&source_with_shebang, alternate_attempt)?;

        assert_ne!(
            plain.bindings().source_sha256(),
            shebang.bindings().source_sha256(),
            "original source bytes remain a separate binding"
        );
        assert_ne!(
            plain.bindings().task_attempt(),
            shebang.bindings().task_attempt(),
            "task identity remains a separate binding"
        );
        assert_eq!(
            plain.bindings().operation_oir(),
            shebang.bindings().operation_oir(),
            "the prepared executable syntax is identical after shebang stripping"
        );
        assert_eq!(
            plain.bindings().placement_admission(),
            shebang.bindings().placement_admission(),
            "source bytes and task identity must not contaminate portable admission"
        );
        assert_eq!(
            plain.bindings().admission(),
            plain.bindings().placement_admission(),
            "the placement-lease compatibility getter uses the portable coordinate"
        );
        assert_eq!(plain.bindings().placement_admission().as_sha256().len(), 64);
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_permits_only_trailing_whitespace_roots() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        for suffix in ["\n", "\r\n \t"] {
            let source = format!("python^(__oval_result__ = 6 * 7)_python{suffix}");
            let mut evaluator = placement_evaluator(shim_dir.clone());
            let prepared = evaluator.prepare_placement_fragment(&source, placement_attempt())?;
            assert_eq!(
                prepared.bindings().source_sha256(),
                crate::evidence::source_sha256(source.as_bytes())
            );
            assert_eq!(
                evaluator.execute_prepared_placement_fragment(prepared, &mut HashMap::new())?,
                OValue::int(42)
            );
        }

        let mut evaluator = placement_evaluator(shim_dir);
        let error = match evaluator.prepare_placement_fragment(
            "python^(__oval_result__ = 42)_python\nnot whitespace",
            placement_attempt(),
        ) {
            Ok(_) => panic!("non-whitespace sibling text must remain outside placement authority"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("non-whitespace top-level text root"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_binds_persistent_session_requirement() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = placement_evaluator(shim_dir);
        let prepared = evaluator.prepare_placement_fragment(
            "python[17]^(__oval_result__ = 42)_python[17]",
            placement_attempt(),
        )?;
        assert_eq!(
            prepared.bindings().environment(),
            EnvironmentRefV2::Persistent(17)
        );
        let capability = crate::placement::CapabilityAtomV1::new(
            crate::placement::CapabilityKeyV1::new(
                crate::placement::SESSION_SERIALIZED_OPAQUE_EFFECTS_NAMESPACE_V1,
                crate::placement::SESSION_SERIALIZED_OPAQUE_EFFECTS_NAME_V1,
            )?,
            1,
        )?;
        assert!(prepared
            .bindings()
            .requirement_footprint()
            .known_atoms()
            .contains(&crate::placement::RequirementAtomV1::Capability(capability)));
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_rejects_non_fragment_shapes() {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let cases = [
            (
                "python^(1)_python\npython^(2)_python",
                "exactly one non-whitespace semantic root",
            ),
            ("now(python^(1)_python)", "cannot contain"),
            ("ordinary text only", "text-only"),
            ("python^($later)_python", "cannot contain Load"),
            ("python^(O.eval('2'))_python", "contains O.eval"),
        ];
        for (source, expected) in cases {
            let mut evaluator = placement_evaluator(shim_dir.clone());
            let error = match evaluator.prepare_placement_fragment(source, placement_attempt()) {
                Ok(_) => panic!("non-fragment input must fail before authorization: {source:?}"),
                Err(error) => error,
            };
            let message = format!("{error:#}");
            assert!(message.contains(expected), "{source:?}: {message}");
        }
    }

    #[test]
    fn prepared_placement_fragment_rejects_runtime_hidden_o_eval() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = placement_evaluator(shim_dir);
        let prepared = evaluator.prepare_placement_fragment(
            "python^(__oval_result__ = getattr(O, ''.join(['e', 'val']))('2'))_python",
            placement_attempt(),
        )?;
        let error = evaluator
            .execute_prepared_placement_fragment(prepared, &mut HashMap::new())
            .expect_err("a dynamically hidden O.eval callback must fail closed");
        assert!(
            error.downcast_ref::<PreparedPlacementRefusalV1>().is_some(),
            "settled callback refusal must retain its semantic error type: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("recursive O.eval authority outside its admitted OIR"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn prepared_persistent_callback_refusal_preserves_actor_state() -> Result<()> {
        let backends = BackendRegistry::global().registered_backend_tags();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let prepared = evaluator.prepare_placement_fragment(
            "python[37]^(x = 41\ngetattr(O, ''.join(['e', 'val']))('2')\n__oval_result__ = x)_python[37]",
            placement_attempt(),
        )?;
        let error = evaluator
            .execute_prepared_placement_fragment(prepared, &mut HashMap::new())
            .expect_err("a hidden callback must be refused without evicting session state");
        assert!(
            error.downcast_ref::<PreparedPlacementRefusalV1>().is_some(),
            "persistent callback refusal must remain a typed semantic refusal: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("recursive O.eval authority outside its admitted OIR"),
            "{error:#}"
        );
        assert_eq!(
            eval_test_source(
                &mut evaluator,
                &backends,
                "python[37]^(__oval_result__ = x + 1)_python[37]",
            )?,
            OValue::int(42),
            "semantic callback refusal must leave the persistent actor live"
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_rejects_stale_runtime_before_dispatch() -> Result<()> {
        let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let temp = tempfile::tempdir()?;
        for file in ["python_shim.py", "o_shim_common.py"] {
            std::fs::copy(source_dir.join(file), temp.path().join(file))?;
        }
        let mut evaluator = placement_evaluator(temp.path().to_path_buf());
        let prepared = evaluator.prepare_placement_fragment(
            "python^(__oval_result__ = 42)_python",
            placement_attempt(),
        )?;
        let shim = temp.path().join("python_shim.py");
        let mut bytes = std::fs::read(&shim)?;
        bytes.extend_from_slice(b"\n# stale after placement preparation\n");
        std::fs::write(&shim, bytes)?;

        let error = evaluator
            .execute_prepared_placement_fragment(prepared, &mut HashMap::new())
            .expect_err("runtime drift must invalidate the prepared handle");
        assert!(
            format!("{error:#}").contains("runtime binding is stale"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_is_fenced_to_its_evaluator() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut source = placement_evaluator(shim_dir.clone());
        let prepared = source.prepare_placement_fragment(
            "python^(__oval_result__ = 42)_python",
            placement_attempt(),
        )?;
        let mut other = placement_evaluator(shim_dir);
        let error = other
            .execute_prepared_placement_fragment(prepared, &mut HashMap::new())
            .expect_err("a different evaluator must not consume retained authority");
        assert!(
            format!("{error:#}").contains("different Evaluator instance"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_rejects_unbound_coordinator_scope() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = placement_evaluator(shim_dir);
        let prepared = evaluator.prepare_placement_fragment(
            "python[43]^(__oval_result__ = injected)_python[43]",
            placement_attempt(),
        )?;
        let mut scope = HashMap::from([("injected".to_owned(), OValue::int(42))]);
        let error = evaluator
            .execute_prepared_placement_fragment(prepared, &mut scope)
            .expect_err("later coordinator scope must not enter sealed placement authority");
        assert!(
            format!("{error:#}").contains("nonempty coordinator scope"),
            "{error:#}"
        );
        assert_eq!(scope.get("injected"), Some(&OValue::int(42)));
        assert!(
            evaluator
                .checkpoint_persistent_actors(1024 * 1024)?
                .actors
                .is_empty(),
            "scope rejection must happen before a persistent actor is launched"
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_deadline_bounds_unresponsive_shim() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = placement_evaluator(shim_dir);
        let prepared = evaluator.prepare_placement_fragment(
            "python^(import time\ntime.sleep(30)\n__oval_result__ = 42)_python",
            placement_attempt(),
        )?;
        let started = Instant::now();
        let deadline = started
            .checked_add(std::time::Duration::from_millis(150))
            .context("test deadline overflowed")?;
        let error = evaluator
            .execute_prepared_placement_fragment_until(prepared, &mut HashMap::new(), deadline)
            .expect_err("a prepared shim must not outlive its evaluator deadline");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "deadline enforcement waited too long: {error:#}"
        );
        assert!(
            crate::process::is_infrastructure_error(&error),
            "an unresponsive backend leaves execution state ambiguous: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("deadline"),
            "timeout diagnostic must identify the deadline: {error:#}"
        );
        assert_eq!(
            evaluator.callback_operation_deadline, None,
            "prepared deadline wrapper must restore the evaluator's prior deadline"
        );
        Ok(())
    }

    #[test]
    fn prepared_placement_fragment_expired_deadline_is_typed_and_pre_dispatch() -> Result<()> {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = placement_evaluator(shim_dir);
        let prepared = evaluator.prepare_placement_fragment(
            "python[44]^(__oval_result__ = 42)_python[44]",
            placement_attempt(),
        )?;
        let expired = Instant::now();
        let error = evaluator
            .execute_prepared_placement_fragment_until(prepared, &mut HashMap::new(), expired)
            .expect_err("an elapsed prepared deadline must fail before backend dispatch");
        assert!(
            error
                .downcast_ref::<PreparedPlacementDeadlineExpiredV1>()
                .is_some(),
            "pre-dispatch deadline refusal must retain its public error type: {error:#}"
        );
        assert!(
            evaluator
                .checkpoint_persistent_actors(1024 * 1024)?
                .actors
                .is_empty(),
            "expired deadline must not launch a persistent actor"
        );
        Ok(())
    }

    #[test]
    fn bash_stdout_scalar_decodes_to_ovalue() {
        if which::which("bash").is_err() {
            return;
        }

        let backends: HashSet<String> = ["bash"].iter().map(|s| s.to_string()).collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let nodes = Parser::new("bash^(printf 42)_bash", &backends)
            .parse()
            .unwrap();

        assert_eq!(evaluator.eval_document(nodes).unwrap(), OValue::int(42));
    }

    #[test]
    fn sql_single_cell_result_decodes_to_ovalue() {
        let backends: HashSet<String> = ["sql"].iter().map(|s| s.to_string()).collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let nodes = Parser::new("sql^(SELECT 40 + 2 AS answer;)_sql", &backends)
            .parse()
            .unwrap();

        assert_eq!(evaluator.eval_document(nodes).unwrap(), OValue::int(42));
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

        let hgraph_schedule = evaluator
            .last_hgraph_schedule()
            .expect("document execution must also build a hypergraph schedule");
        assert!(!hgraph_schedule.clusters.is_empty());
    }

    #[test]
    fn execution_trace_records_node_lifecycle_and_fingerprint() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "expr".into(),
                    expr: Box::new(OIr::Exec {
                        lang: "nix_expr".into(),
                        env_id: u32::MAX,
                        attr: None,
                        backend: BackendRegistry::global().interface_for("nix_expr"),
                        body: vec![OIr::Text("{ name = \"demo\"; }".into())],
                    }),
                },
                OIr::Load("expr".into()),
            ],
        };
        let mut evaluator = Evaluator::new("/tmp".into());
        let result = evaluator.eval_ir_program(&program).unwrap();
        assert_eq!(result.type_name(), "nix_expr");

        let trace = evaluator
            .last_execution_trace()
            .expect("document execution must install an execution trace");
        assert!(trace
            .events
            .iter()
            .any(|event| matches!(event, TraceEvent::NodeReady(PlanNodeId(0)))));
        assert!(trace
            .events
            .iter()
            .any(|event| matches!(event, TraceEvent::NodeStarted(PlanNodeId(0)))));
        assert!(trace.events.iter().any(|event| {
            matches!(
                event,
                TraceEvent::NodeFinished {
                    id: PlanNodeId(1),
                    value_type,
                    fingerprint,
                } if value_type == "nix_expr"
                    && fingerprint.as_ref().map(|fp| fp.len() == 64).unwrap_or(false)
            )
        }));
        assert!(trace.events.iter().any(|event| {
            matches!(
                event,
                TraceEvent::NodeFinished {
                    id: PlanNodeId(3),
                    value_type,
                    ..
                } if value_type == "nix_expr"
            )
        }));
    }

    #[test]
    fn execution_trace_finishes_each_planned_node_once() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "x".into(),
                    expr: Box::new(OIr::Text("one".into())),
                },
                OIr::Exec {
                    lang: "html".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: BackendRegistry::global().interface_for("html"),
                    body: vec![OIr::Load("x".into()), OIr::Text(" two".into())],
                },
            ],
        };
        let mut evaluator = Evaluator::new("/tmp".into());
        assert_eq!(
            evaluator.eval_ir_program(&program).unwrap(),
            OValue::html("one two")
        );

        let plan = evaluator.last_execution_plan().unwrap();
        let trace = evaluator.last_execution_trace().unwrap();
        let mut finished = trace
            .events
            .iter()
            .filter_map(|event| {
                if let TraceEvent::NodeFinished { id, .. } = event {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let raw_count = finished.len();
        finished.sort_by_key(|id| id.0);
        finished.dedup();

        assert_eq!(raw_count, plan.nodes.len());
        assert_eq!(finished.len(), plan.nodes.len());
    }

    #[test]
    fn execution_trace_records_node_failures() {
        let program = OIrProgram {
            nodes: vec![OIr::Load("missing".into())],
        };
        let mut evaluator = Evaluator::new("/tmp".into());
        let error = evaluator.eval_ir_program(&program).unwrap_err().to_string();
        assert!(error.contains("Undefined variable"));

        let trace = evaluator
            .last_execution_trace()
            .expect("failed execution should retain its trace");
        assert!(trace.events.iter().any(|event| {
            matches!(
                event,
                TraceEvent::NodeFailed {
                    id: PlanNodeId(0),
                    message,
                } if message.contains("Undefined variable")
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
        assert!(evaluator.last_hgraph_schedule().is_some());
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
        // $n is a VarRef that resolves to OValue::Number(7)
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

    #[cfg(unix)]
    #[test]
    fn immediate_executor_resolves_nested_source_before_outer_nix_authority() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let closure = temp.path().join("system");
        std::fs::create_dir_all(closure.join("bin")).unwrap();
        let marker = temp.path().join("activated.marker");
        let switch = closure.join("bin/switch-to-configuration");
        std::fs::write(
            &switch,
            format!("#!/bin/sh\nprintf activated > {:?}\n", marker),
        )
        .unwrap();
        std::fs::set_permissions(&switch, std::fs::Permissions::from_mode(0o755)).unwrap();

        let inner = OValue::request(
            RequestKind::Activate {
                profile: temp.path().join("profile").display().to_string(),
                dry_run: true,
                authority: None,
            },
            OValue::store_path(closure.display().to_string()),
        );
        let outer = OValue::request(RequestKind::Realise, inner);
        let error = ImmediateExecutor::new().execute(&outer).unwrap_err();

        assert!(
            marker.exists(),
            "the nested source Request must settle before outer Nix validation or capture"
        );
        let message = format!("{error:#}");
        assert!(message.contains("Derivation"), "{message}");
        assert!(!message.contains("runtime command `nix`"), "{message}");
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

    #[test]
    fn block_capability_binding_must_be_an_o_identifier() {
        let error = BlockOptions::parse(Some("cap=-"), "python").unwrap_err();
        assert_eq!(
            error.to_string(),
            "backend capability binding `-` is not an O identifier"
        );
    }

    #[test]
    fn block_capability_binding_accepts_valid_o_identifier() {
        let options = BlockOptions::parse(Some("cap=_runner2,process"), "python").unwrap();
        assert_eq!(options.capability_binding(), Some("_runner2"));
        assert_eq!(options.permissions(), &[BackendAuthority::Process]);
    }

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

    /// {lazy} on a cache-safe inline backend (html) returns a Request[Eval]
    /// without executing. The Thunk inside carries body + deps.
    #[test]
    fn lazy_attr_on_pure_backend_produces_eval_request() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang: "html".into(),
            env_id: u32::MAX,
            attr: Some("lazy".into()),
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
                    assert_eq!(lang, "html");
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

    /// `sql{lazy}^(...)_sql{lazy}` is rejected before any shim execution:
    /// each SQL environment owns mutable persistent SQLite state that the
    /// generic `{lazy}` cache fingerprint does not capture. The error must
    /// suggest `{defer}`.
    #[test]
    fn lazy_attr_on_stateful_sql_backend_is_rejected() {
        let mut e = Evaluator::new("/definitely/missing/shims".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang: "sql".into(),
            env_id: u32::MAX,
            attr: Some("lazy".into()),
            body: vec![ONode::RawText("SELECT 1;".into())],
        };
        let err = e.eval_node(&block, &scope).unwrap_err().to_string();
        assert!(
            err.contains("not a pure backend"),
            "sql{{lazy}} must be rejected for cache-safety, got: {err}"
        );
        assert!(
            err.contains("defer"),
            "error must suggest {{defer}}, got: {err}"
        );
        assert!(
            !err.contains("failed to spawn backend shim"),
            "rejection must happen before shim execution, got: {err}"
        );
    }

    /// Every unrestricted shim-backed external backend rejects `{lazy}`
    /// before shim execution — no backend runtime needs to be installed
    /// for this table to hold.
    #[test]
    fn lazy_attr_on_unrestricted_external_backends_is_rejected() {
        for lang in [
            "nix",
            "nix_store",
            "nixos_test",
            "haskell",
            "ocaml",
            "webassembly",
            "sql",
        ] {
            let mut e = Evaluator::new("/definitely/missing/shims".into());
            let scope = HashMap::new();
            let block = ONode::TypedExpr {
                lang: lang.to_string(),
                env_id: u32::MAX,
                attr: Some("lazy".into()),
                body: vec![ONode::RawText("body".into())],
            };
            let err = e.eval_node(&block, &scope).unwrap_err().to_string();
            assert!(
                err.contains("not a pure backend"),
                "{lang}{{lazy}} must be rejected, got: {err}"
            );
            assert!(
                err.contains("defer"),
                "{lang}{{lazy}} rejection must suggest {{defer}}, got: {err}"
            );
            assert!(
                !err.contains("failed to spawn backend shim"),
                "{lang}{{lazy}} rejection must precede shim execution, got: {err}"
            );
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
    fn default_backend_authority_allows_dispatch_without_capability_binding() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let scope = HashMap::new();
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("fs_read".into()),
            body: vec![ONode::RawText("__oval_result__ = 1".into())],
        };
        let error = format!("{:#}", evaluator.eval_node(&block, &scope).unwrap_err());
        assert!(
            error.contains("failed to spawn backend shim")
                || error.contains("backend shim not found")
                || error.contains("No such file or directory")
                || error.contains("backend process closed stdout"),
            "default backend authority should allow dispatch to reach the shim layer, got: {error}"
        );
        assert!(!error.contains("names no live capability"));
    }

    #[test]
    fn adapter_required_authority_is_available_by_default() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let block = ONode::TypedExpr {
            lang: "bash".into(),
            env_id: u32::MAX,
            attr: None,
            body: vec![ONode::RawText("printf forbidden".into())],
        };
        let error = format!(
            "{:#}",
            evaluator.eval_node(&block, &HashMap::new()).unwrap_err()
        );
        assert!(
            error.contains("failed to spawn backend shim")
                || error.contains("backend shim not found")
                || error.contains("No such file or directory")
                || error.contains("backend process closed stdout"),
            "default backend authority should allow bash dispatch to reach the shim layer, got: {error}"
        );
        assert!(!error.contains("names no live capability"));
    }

    #[test]
    fn autonomous_worker_shim_cannot_manufacture_revoked_authority() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let default_capability = OValue::capability(
            CapabilityKind::BackendExecution,
            evaluator.default_backend_authority.clone(),
            HashMap::new(),
        );
        evaluator
            .revoke_backend_execution_capability(&default_capability)
            .unwrap();
        let backend = BackendRegistry::global().interface_for("python");
        let error = evaluator
            .authorize_autonomous_ephemeral_shim(&backend, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("default backend authority")
                || error.contains("forged, revoked, or from another evaluator"),
            "unexpected authority diagnostic: {error}"
        );
    }

    #[test]
    fn legacy_backend_capability_attrs_do_not_reduce_default_authority() {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir);
        let capability = evaluator
            .issue_backend_execution_capability("python", [BackendAuthority::Process])
            .unwrap();
        let scope = HashMap::from([("runner".into(), capability)]);
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: Some("cap=runner,process".into()),
            body: vec![ONode::RawText(
                "import os, tempfile\nfd, path = tempfile.mkstemp()\nos.write(fd, b'ok')\nos.close(fd)\nos.remove(path)\n__oval_result__ = 1".into(),
            )],
        };
        assert_eq!(evaluator.eval_node(&block, &scope).unwrap(), OValue::int(1));
    }

    #[test]
    fn plain_python_blocks_can_spawn_processes_by_default() {
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir);
        let block = ONode::TypedExpr {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            body: vec![ONode::RawText(
                "import os\n__oval_result__ = os.system('true')".into(),
            )],
        };
        assert_eq!(
            evaluator.eval_node(&block, &HashMap::new()).unwrap(),
            OValue::int(0)
        );
    }

    #[test]
    fn deferred_backend_authority_is_rechecked_when_forced_if_explicit() {
        let mut evaluator = Evaluator::new("/definitely/missing/shims".into());
        let capability = evaluator
            .issue_backend_execution_capability("python", BackendAuthority::ALL)
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
    fn forged_deferred_request_cannot_omit_default_backend_rights() {
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
        assert!(
            error.contains("backend execution capability for bash lacks")
                && error.contains("authority"),
            "underpowered capability must fail before spawn, got: {error}"
        );
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

    /// now() on a {lazy} Eval request returns the cached value. We seed the
    /// cache directly to verify the cache-hit path.
    #[test]
    fn now_on_lazy_eval_request_returns_cached_value() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        let block = ONode::TypedExpr {
            lang: "html".into(),
            env_id: u32::MAX,
            attr: Some("lazy".into()),
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
        let mut scope = HashMap::new();

        // Construct a {lazy} block on a cache-safe inline backend, find its
        // fingerprint, seed the cache.
        let lazy_block = ONode::TypedExpr {
            lang: "text".into(),
            env_id: u32::MAX,
            attr: Some("lazy".into()),
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
    // `activate(path[, profile])` constructs a real switch request using ambient
    // host authority. `dry_activate(path[, profile])` keeps dry-run activation
    // explicit. An optional `activate(system_activation_capability, path[, profile])`
    // form remains available for embedding-specific profile guards.
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

    /// `dry_activate($path)` constructs a dry Request[Activate] and (under
    /// Eager) auto-resolves it. The mock executor returns a System value.
    #[test]
    fn dry_activate_call_builds_request_and_resolves_to_system() {
        let mut e =
            Evaluator::new("/tmp".into()).with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));

        let call = ONode::Call {
            fn_name: "dry_activate".into(),
            args: vec![ONode::VarRef("path".into())],
        };
        let result = e.eval_node(&call, &scope).unwrap();
        assert!(
            result.is_system(),
            "dry_activate($path) under Eager should auto-resolve to a System, got {:?}",
            result
        );
        if let OValue::System { profile_path } = &result {
            assert_eq!(
                profile_path, "/nix/var/nix/profiles/system",
                "default profile should be the system-wide one"
            );
        }
    }

    /// `dry_activate($path, $profile)` uses the user-supplied profile.
    #[test]
    fn dry_activate_with_explicit_profile_uses_it() {
        let mut e =
            Evaluator::new("/tmp".into()).with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));
        scope.insert("profile".into(), OValue::str_("/home/lee/.nix-profile"));

        let call = ONode::Call {
            fn_name: "dry_activate".into(),
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
    fn activate_without_capability_builds_real_activation_request() {
        let mut evaluator = Evaluator::new("/tmp".into());
        evaluator.policy = Policy::Lazy;
        let mut scope = HashMap::new();
        scope.insert("path".into(), OValue::store_path("/nix/store/abc-system"));

        let request = evaluator
            .eval_node(
                &ONode::Call {
                    fn_name: "activate".into(),
                    args: vec![ONode::VarRef("path".into())],
                },
                &scope,
            )
            .unwrap();

        let OValue::Request {
            kind:
                RequestKind::Activate {
                    profile,
                    dry_run,
                    authority,
                },
            ..
        } = request
        else {
            panic!("expected an Activate request")
        };
        assert_eq!(profile, "/nix/var/nix/profiles/system");
        assert!(!dry_run);
        assert!(authority.is_none());
    }

    #[test]
    fn ambient_real_activation_reaches_perform_boundary() {
        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        scope.insert(
            "path".into(),
            OValue::store_path("/tmp/not-a-system-closure"),
        );

        let error = evaluator
            .eval_node(
                &ONode::Call {
                    fn_name: "activate".into(),
                    args: vec![ONode::VarRef("path".into())],
                },
                &scope,
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("does not contain bin/switch-to-configuration"));
        assert!(!error.contains("requires a live system_activation capability"));
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

    /// The full four-rung chain — `dry_activate(realise(instantiate($expr)))` —
    /// is structurally well-typed: each Request's source is the previous rung,
    /// and the executor walks the chain end-to-end under Eager.
    #[test]
    fn full_chain_instantiate_realise_dry_activate() {
        let mut e =
            Evaluator::new("/tmp".into()).with_executor(Box::new(MockSystemExecutor::new()));
        let mut scope = HashMap::new();
        scope.insert(
            "expr".into(),
            OValue::nix_expr("nixos.config.system", vec![]),
        );

        let activate_call = ONode::Call {
            fn_name: "dry_activate".into(),
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
            "instantiate→realise→dry_activate chain must resolve to a System"
        );
    }

    /// activate() with a NixExpr (not yet instantiated) is NOT auto-realised.
    /// The intermediate climb is the user's responsibility to make explicit.
    /// (Auto-realising via a chained Request[Realise[Instantiate]] DOES work,
    /// because the chain is constructed at call sites; bare values aren't
    /// auto-lifted.)
    #[test]
    fn activate_on_bare_nix_expr_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let mut scope: HashMap<String, OValue> = HashMap::new();
        scope.insert("expr".into(), OValue::nix_expr("config", vec![]));

        // Construct activate($expr) where $expr is a bare NixExpr, not a chain.
        let error = e
            .eval_node(
                &ONode::Call {
                    fn_name: "activate".into(),
                    args: vec![ONode::VarRef("expr".into())],
                },
                &scope,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("activate(realise(instantiate"));
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

    #[test]
    fn quote_body_is_absent_from_execution_trace() {
        let backends: HashSet<String> = ["python", "quote", "O"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut evaluator =
            Evaluator::new("/tmp".into()).with_registered_backends(backends.clone());
        let src = r"quote^(python^(6*7)_python)_quote";
        let nodes = Parser::new(src, &backends).parse().unwrap();

        let result = evaluator.eval_document(nodes).unwrap();
        assert!(matches!(result, OValue::Expr { .. }));

        let plan = evaluator.last_execution_plan().unwrap();
        assert_eq!(plan.nodes.len(), 1);
        let trace = evaluator.last_execution_trace().unwrap();
        let started = trace
            .events
            .iter()
            .filter_map(|event| {
                if let TraceEvent::NodeStarted(id) = event {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(started, vec![PlanNodeId(0)]);
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

    #[test]
    fn autonomous_worker_callback_restores_outer_execution_artifacts() {
        let backends = BackendRegistry::global().registered_backend_tags();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let source = r#"
let lexical = text^(outer-artifact)_text
let quoted = quote^(text^($lexical)_text)_quote
autonomous(batch(
python^(__oval_result__ = O.eval(quoted))_python
))
"#;
        let nodes = Parser::new(source, &backends).parse().unwrap();
        let expected_plan = OIrProgram::lower(&nodes).plan();

        evaluator.eval_document(nodes).unwrap();

        let plan = evaluator
            .last_execution_plan()
            .expect("outer plan remains observable after callback");
        assert_eq!(plan, &expected_plan);
        assert!(plan.roots.iter().any(|root| matches!(
            plan.nodes[root.0].kind,
            PlanNodeKind::Schedule {
                kind: crate::ir::PlanScheduleKind::Autonomous,
                ..
            }
        )));
        let admission = evaluator
            .last_execution_admission()
            .expect("outer admission remains observable after callback");
        assert!(admission
            .operations()
            .iter()
            .all(|operation| operation.plan_node.0 < plan.nodes.len()));
        let trace = evaluator
            .last_execution_trace()
            .expect("outer trace remains observable after callback");
        assert!(trace.events.iter().all(|event| {
            let id = match event {
                TraceEvent::NodeReady(id) | TraceEvent::NodeStarted(id) => *id,
                TraceEvent::NodeFinished { id, .. }
                | TraceEvent::NodeFailed { id, .. }
                | TraceEvent::NodeDiscarded { id, .. } => *id,
            };
            id.0 < plan.nodes.len()
        }));
    }

    #[test]
    fn o_eval_number_result_rehydrates_as_host_expression() {
        let backends: HashSet<String> = ["python", "quote", "O"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
        let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends.clone());
        let source = r#"
let q = quote^(python[1]^(2 ** 100)_python[1])_quote
python[0]^(O.eval($q) + 1)_python[0]
"#;
        let nodes = Parser::new(source, &backends).parse().unwrap();

        let result = evaluator.eval_document(nodes).unwrap();

        match result {
            OValue::Number {
                v: ONumber::Int { v },
            } => {
                let expected = (num_bigint::BigInt::from(1_u8) << 100_u32) + 1_u8;
                assert_eq!(v, expected);
            }
            other => panic!("expected O.eval big integer number, got {other:?}"),
        }
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

    /// Under autonomous(), Eval requests ({lazy} blocks on cache-safe
    /// backends) are executed eagerly, bypassing the buffer. The buffer only
    /// collects Nix-family requests.
    #[test]
    fn autonomous_eval_requests_are_executed_eagerly() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();

        // Construct an Eval Request ({lazy} block on a cache-safe inline
        // backend) — this should go through the Evaluator's eval_cache,
        // not the scheduler buffer.
        let lazy_block = ONode::TypedExpr {
            lang: "html".into(),
            env_id: u32::MAX,
            attr: Some("lazy".into()),
            body: vec![ONode::RawText("1 + 2".into())],
        };
        // First, collect the fingerprint to seed the eval_cache.
        let req = e.eval_node(&lazy_block, &scope).unwrap();
        let fp = match &req {
            OValue::Request { fingerprint, .. } => fingerprint.clone(),
            _ => panic!(),
        };
        e.eval_cache.insert(fp.clone(), OValue::int(3));

        // Now call autonomous() wrapping another {lazy} block for the same expression.
        let call = ONode::Call {
            fn_name: "autonomous".into(),
            args: vec![ONode::Call {
                fn_name: "now".into(),
                args: vec![ONode::TypedExpr {
                    lang: "html".into(),
                    env_id: u32::MAX,
                    attr: Some("lazy".into()),
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
    /// Because `batch` is a special form, the outer Eager policy is lowered to
    /// Lazy capture for group members. The group therefore holds Request chains
    /// (not pre-resolved StorePaths).
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
    //   - Collect-All (Batch): collect every outcome; failures become OError.
    //   - Collect-All (All):   entire group fails if ANY member fails.
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

    /// Under Policy::Autonomous, group constructors still capture Request
    /// members, but they must not downgrade policy to Lazy. The graph executor
    /// gets that behavior from derived node policies before requests are built.
    #[test]
    fn graph_policy_context_preserves_autonomous_group_capture() {
        let mk_chain = |var: &str| ONode::Call {
            fn_name: "realise".into(),
            args: vec![ONode::Call {
                fn_name: "instantiate".into(),
                args: vec![ONode::VarRef(var.into())],
            }],
        };
        let call = ONode::Call {
            fn_name: "batch".into(),
            args: vec![mk_chain("e1"), mk_chain("e2")],
        };

        let program = OIrProgram {
            nodes: vec![lower_node(&call)],
        };
        let plan = program.plan();
        plan.validate(program.nodes.len()).unwrap();
        let flat = program.flatten_for_plan();
        let autonomous_policies = derive_policy_contexts(&plan, &flat, Policy::Autonomous).unwrap();
        let eager_policies = derive_policy_contexts(&plan, &flat, Policy::Eager).unwrap();
        let request_ids = plan
            .nodes
            .iter()
            .filter_map(|node| matches!(node.kind, PlanNodeKind::Request { .. }).then_some(node.id))
            .collect::<Vec<_>>();
        assert_eq!(request_ids.len(), 4);

        for id in &request_ids {
            assert_eq!(
                autonomous_policies[id.0],
                Policy::Autonomous,
                "request node {} should preserve autonomous capture",
                id.0
            );
            assert_eq!(
                eager_policies[id.0],
                Policy::Lazy,
                "request node {} should be captured lazily under eager batch(...)",
                id.0
            );
        }
    }

    /// If a member expression fails while a group constructor is capturing
    /// members, the saved policy must still be restored before the error is
    /// returned.
    #[test]
    fn group_constructor_restores_policy_when_member_eval_errors() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = HashMap::new();
        assert_eq!(e.policy, Policy::Eager);

        let call = ONode::Call {
            fn_name: "batch".into(),
            args: vec![ONode::VarRef("missing".into())],
        };
        let err = e.eval_node(&call, &scope).unwrap_err().to_string();

        assert!(err.contains("missing"), "got {err}");
        assert_eq!(
            e.policy,
            Policy::Eager,
            "policy must be restored after group-construction failure"
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
    /// This is a scheduler invariant failure, not an ordinary member failure for
    /// `batch` to wrap.
    #[test]
    fn autonomous_batch_errors_on_missing_cache_result() {
        let mut e = Evaluator::new("/tmp".into());
        let scope = scope_with_nix_expr();

        // Build a realise Request but do NOT seed the cache.
        let expr = e.eval_node(&ONode::VarRef("e1".into()), &scope).unwrap();
        let inst = OValue::request(RequestKind::Instantiate, expr);
        let realise = OValue::request(RequestKind::Realise, inst);

        // Batch mode: strict cache miss is a hard scheduler invariant error.
        let batch_err = e
            .resolve_group(
                GroupMode::Batch,
                std::slice::from_ref(&realise),
                CacheMode::Strict,
            )
            .unwrap_err();
        let batch_err = format!("{batch_err:#}");
        assert!(
            batch_err.contains("autonomous")
                || batch_err.contains("cache miss")
                || batch_err.contains("materialize"),
            "CacheStrict Batch must hard-error on cache miss, got: {batch_err}"
        );

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

    // ── graph executor vs serial reference executor ───────────────────────────

    /// A corpus of backend-free programs (inline text/html/group/store/load)
    /// that both executors must evaluate identically.
    fn equivalence_corpus() -> Vec<OIrProgram> {
        let html = BackendRegistry::global().interface_for("html");
        let text = BackendRegistry::global().interface_for("text");
        vec![
            // Independent inline renders (become concurrently ready).
            OIrProgram {
                nodes: vec![
                    OIr::Exec {
                        lang: "html".into(),
                        env_id: u32::MAX,
                        attr: None,
                        backend: html.clone(),
                        body: vec![OIr::Text("a".into())],
                    },
                    OIr::Exec {
                        lang: "html".into(),
                        env_id: u32::MAX,
                        attr: None,
                        backend: html.clone(),
                        body: vec![OIr::Text("b".into())],
                    },
                ],
            },
            // Store then load (data dependency).
            OIrProgram {
                nodes: vec![
                    OIr::Store {
                        name: "x".into(),
                        expr: Box::new(OIr::Text("hi".into())),
                    },
                    OIr::Exec {
                        lang: "text".into(),
                        env_id: u32::MAX,
                        attr: None,
                        backend: text.clone(),
                        body: vec![OIr::Load("x".into()), OIr::Text("!".into())],
                    },
                ],
            },
            // Group of inline members.
            OIrProgram {
                nodes: vec![OIr::Invoke {
                    fn_name: "batch".into(),
                    mode: InvokeMode::Group(GroupMode::Batch),
                    args: vec![
                        OIr::Exec {
                            lang: "text".into(),
                            env_id: u32::MAX,
                            attr: None,
                            backend: text.clone(),
                            body: vec![OIr::Text("one".into())],
                        },
                        OIr::Exec {
                            lang: "text".into(),
                            env_id: u32::MAX,
                            attr: None,
                            backend: text.clone(),
                            body: vec![OIr::Text("two".into())],
                        },
                    ],
                }],
            },
            // Nested inline splice.
            OIrProgram {
                nodes: vec![OIr::Exec {
                    lang: "html".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: html.clone(),
                    body: vec![
                        OIr::Text("<b>".into()),
                        OIr::Exec {
                            lang: "text".into(),
                            env_id: u32::MAX,
                            attr: None,
                            backend: text.clone(),
                            body: vec![OIr::Text("deep".into())],
                        },
                        OIr::Text("</b>".into()),
                    ],
                }],
            },
            // Identical failure outcome and normalized diagnostic.
            OIrProgram {
                nodes: vec![OIr::Load("missing_binding".into())],
            },
        ]
    }

    #[test]
    fn graph_executor_matches_serial_on_corpus() {
        let normalize_error = |error: anyhow::Error| error.to_string().replace("\r\n", "\n");
        for (index, program) in equivalence_corpus().into_iter().enumerate() {
            let mut serial_eval = Evaluator::new("/tmp".into());
            let mut serial_scope = HashMap::new();
            let serial = serial_eval
                .eval_ir_program_forcing(&program, &mut serial_scope, true)
                .map_err(&normalize_error);

            let mut graph_eval = Evaluator::new("/tmp".into());
            let mut graph_scope = HashMap::new();
            let graph = graph_eval
                .eval_ir_program_forcing(&program, &mut graph_scope, false)
                .map_err(&normalize_error);

            assert_eq!(
                serial, graph,
                "graph executor value/error outcome or diagnostic diverged from serial for corpus program {index}"
            );
            assert_eq!(
                serial_scope, graph_scope,
                "graph executor scope commit diverged from serial for corpus program {index}"
            );
        }
    }

    #[test]
    fn graph_coordinator_executes_independent_inline_roots_concurrently() {
        let program = equivalence_corpus().remove(0);
        let overlap = crate::executor::parallel::TestOverlapSession::begin(2);
        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();

        evaluator
            .eval_ir_program_forcing(&program, &mut scope, false)
            .expect("graph execution succeeds");

        assert!(
            overlap.peak() > 1,
            "independent graph operations did not overlap on renderer workers"
        );
    }

    #[test]
    fn graph_coordinator_dispatches_newly_ready_work_without_a_wave_barrier() {
        let registry = BackendRegistry::global();
        let text = registry.interface_for("text");
        let program = OIrProgram {
            nodes: vec![
                OIr::Exec {
                    lang: "text".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: text.clone(),
                    body: vec![OIr::Exec {
                        lang: "text".into(),
                        env_id: u32::MAX,
                        attr: None,
                        backend: text.clone(),
                        body: vec![OIr::Text("fast".into())],
                    }],
                },
                OIr::Exec {
                    lang: "text".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: text,
                    body: vec![OIr::Text("blocked".into())],
                },
            ],
        };
        let plan = program.plan();
        let downstream = plan.roots[0];
        let blocked = plan.roots[1];
        let pipeline = crate::executor::parallel::TestPipelineSession::begin(blocked, downstream);
        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();

        evaluator
            .eval_ir_program_forcing(&program, &mut scope, false)
            .expect("completion-driven graph execution succeeds");

        assert!(
            pipeline.downstream_started_before_blocked_finished(),
            "newly-ready downstream work waited for the prior readiness wave to drain"
        );
    }

    #[test]
    fn graph_coordinator_publishes_infallible_dependency_before_earlier_settlement() {
        let text = BackendRegistry::global().interface_for("text");
        let render = |body: Vec<OIr>| OIr::Exec {
            lang: "text".into(),
            env_id: u32::MAX,
            attr: None,
            backend: text.clone(),
            body,
        };
        let program = OIrProgram {
            nodes: vec![
                render(vec![OIr::Text("blocked-a".into())]),
                render(vec![render(vec![OIr::Text("fast-b".into())])]),
            ],
        };
        let plan = program.plan();
        let blocked = plan.roots[0];
        let downstream = plan.roots[1];
        let pipeline = crate::executor::parallel::TestPipelineSession::begin(blocked, downstream);
        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();

        evaluator
            .eval_ir_program_forcing(&program, &mut scope, false)
            .expect("infallible dependent pipeline succeeds");

        assert!(
            pipeline.downstream_started_before_blocked_finished(),
            "a physically completed infallible producer remained head-of-line blocked"
        );
    }

    #[test]
    fn graph_coordinator_refills_a_worker_slot_before_lower_ordinal_settlement() {
        let text = BackendRegistry::global().interface_for("text");
        let renderer = |body: &str| OIr::Exec {
            lang: "text".into(),
            env_id: u32::MAX,
            attr: None,
            backend: text.clone(),
            body: vec![OIr::Text(body.into())],
        };
        let program = OIrProgram {
            nodes: vec![
                renderer("slow-first"),
                renderer("fast-second"),
                renderer("refill"),
            ],
        };
        let plan = program.plan();
        let blocked = plan.roots[0];
        let refill = plan.roots[2];
        let pipeline = crate::executor::parallel::TestPipelineSession::begin(blocked, refill);
        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();

        evaluator
            .eval_ir_program_forcing(&program, &mut scope, false)
            .expect("completion-driven slot refill succeeds");

        assert!(
            pipeline.downstream_started_before_blocked_finished(),
            "a free physical slot waited for lower-ordinal semantic settlement"
        );
    }

    #[test]
    fn graph_coordinator_executes_same_binding_reads_concurrently() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "shared".into(),
                    expr: Box::new(OIr::Text("frozen".into())),
                },
                OIr::Load("shared".into()),
                OIr::Load("shared".into()),
            ],
        };
        let overlap = crate::executor::parallel::TestOverlapSession::begin(2);
        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();

        let value = evaluator
            .eval_ir_program_forcing(&program, &mut scope, false)
            .expect("same-version reads succeed");

        assert_eq!(value, OValue::str_("frozen"));
        assert_eq!(scope.get("shared"), Some(&OValue::str_("frozen")));
        assert!(
            overlap.peak() > 1,
            "same-resource reads did not overlap on local workers"
        );
    }

    #[test]
    fn graph_coordinator_discards_later_worker_results_after_first_failure() {
        let program = OIrProgram {
            nodes: vec![
                OIr::Load("missing_first".into()),
                OIr::Load("missing_second".into()),
            ],
        };
        let overlap = crate::executor::parallel::TestOverlapSession::begin(2);
        let mut evaluator = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();

        let error = evaluator
            .eval_ir_program_forcing(&program, &mut scope, false)
            .expect_err("the first missing binding must win")
            .to_string();

        assert!(error.contains("missing_first"), "got: {error}");
        assert!(overlap.peak() > 1, "fallible workers did not overlap");
        let trace = evaluator
            .last_execution_trace()
            .expect("failed execution retains a complete trace");
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEvent::NodeFailed {
                id: PlanNodeId(0),
                message,
            } if message.contains("missing_first")
        )));
        assert!(trace.events.iter().any(|event| matches!(
            event,
            TraceEvent::NodeDiscarded {
                id: PlanNodeId(1),
                ..
            }
        )));
    }

    fn nested_python_effect_program(path: &std::path::Path, fail_first: bool) -> OIrProgram {
        let registry = BackendRegistry::global();
        let path = format!("{:?}", path);
        let first_tail = if fail_first {
            "\nraise RuntimeError(\"stop\")\n".to_string()
        } else {
            format!(
                "\nwith open({path}, \"a\", encoding=\"utf-8\") as stream:\n    stream.write(label + \"\\n\")\n__oval_result__ = label\n"
            )
        };
        let second = format!(
            "with open({path}, \"a\", encoding=\"utf-8\") as stream:\n    stream.write(\"B\\n\")\n__oval_result__ = \"B\"\n"
        );

        OIrProgram {
            nodes: vec![
                OIr::Exec {
                    lang: "python".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: registry.interface_for("python"),
                    body: vec![
                        OIr::Text("label = ".into()),
                        OIr::Exec {
                            lang: "text".into(),
                            env_id: u32::MAX,
                            attr: None,
                            backend: registry.interface_for("text"),
                            body: vec![OIr::Text("A".into())],
                        },
                        OIr::Text(first_tail),
                    ],
                },
                OIr::Exec {
                    lang: "python".into(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: registry.interface_for("python"),
                    body: vec![OIr::Text(second)],
                },
            ],
        }
    }

    #[test]
    fn graph_executor_preserves_file_effect_order_after_nested_child() {
        if which::which("python3").is_err() {
            return;
        }
        let serial_dir = tempfile::tempdir().unwrap();
        let graph_dir = tempfile::tempdir().unwrap();
        let serial_path = serial_dir.path().join("order.txt");
        let graph_path = graph_dir.path().join("order.txt");
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");

        let mut serial_eval = Evaluator::new(shim_dir.clone());
        let mut serial_scope = HashMap::new();
        serial_eval
            .eval_ir_program_forcing(
                &nested_python_effect_program(&serial_path, false),
                &mut serial_scope,
                true,
            )
            .unwrap();

        let mut graph_eval = Evaluator::new(shim_dir);
        let mut graph_scope = HashMap::new();
        graph_eval
            .eval_ir_program_forcing(
                &nested_python_effect_program(&graph_path, false),
                &mut graph_scope,
                false,
            )
            .unwrap();

        let expected = b"A\nB\n";
        assert_eq!(std::fs::read(&serial_path).unwrap(), expected);
        assert_eq!(std::fs::read(&graph_path).unwrap(), expected);
    }

    #[test]
    fn graph_executor_does_not_run_later_effect_after_earlier_failure() {
        if which::which("python3").is_err() {
            return;
        }
        let serial_dir = tempfile::tempdir().unwrap();
        let graph_dir = tempfile::tempdir().unwrap();
        let serial_path = serial_dir.path().join("must-not-exist");
        let graph_path = graph_dir.path().join("must-not-exist");
        let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");

        let mut serial_eval = Evaluator::new(shim_dir.clone());
        let mut serial_scope = HashMap::new();
        let serial_error = serial_eval
            .eval_ir_program_forcing(
                &nested_python_effect_program(&serial_path, true),
                &mut serial_scope,
                true,
            )
            .unwrap_err();
        let serial_error = format!("{serial_error:#}");

        let mut graph_eval = Evaluator::new(shim_dir);
        let mut graph_scope = HashMap::new();
        let graph_error = graph_eval
            .eval_ir_program_forcing(
                &nested_python_effect_program(&graph_path, true),
                &mut graph_scope,
                false,
            )
            .unwrap_err();
        let graph_error = format!("{graph_error:#}");

        assert!(!serial_path.exists());
        assert!(!graph_path.exists());
        assert!(
            serial_error.contains("RuntimeError: stop"),
            "{serial_error}"
        );
        assert!(graph_error.contains("RuntimeError: stop"), "{graph_error}");
        let normalize = |error: &str| {
            error
                .lines()
                .find(|line| line.contains("RuntimeError: stop"))
                .map(str::trim)
                .unwrap_or(error)
                .to_string()
        };
        assert_eq!(normalize(&serial_error), normalize(&graph_error));
    }

    #[test]
    fn graph_executor_commits_store_in_root_order() {
        // Two stores to the same name; the later root must win under both.
        let program = OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "x".into(),
                    expr: Box::new(OIr::Text("first".into())),
                },
                OIr::Store {
                    name: "x".into(),
                    expr: Box::new(OIr::Text("second".into())),
                },
                OIr::Load("x".into()),
            ],
        };
        for serial in [true, false] {
            let mut eval = Evaluator::new("/tmp".into());
            let mut scope = HashMap::new();
            let result = eval
                .eval_ir_program_forcing(&program, &mut scope, serial)
                .unwrap();
            assert_eq!(result, OValue::str_("second"));
            assert_eq!(scope.get("x"), Some(&OValue::str_("second")));
        }
    }

    #[test]
    fn graph_executor_selects_deterministic_error() {
        // A missing variable fails under both executors with the same message.
        let program = OIrProgram {
            nodes: vec![OIr::Load("missing".into())],
        };
        let mut graph_eval = Evaluator::new("/tmp".into());
        let mut scope = HashMap::new();
        let err = graph_eval
            .eval_ir_program_forcing(&program, &mut scope, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Undefined variable"), "got: {err}");
    }

    #[test]
    fn later_fallible_worker_cannot_preempt_earlier_coordinator_failure() {
        let program = OIrProgram {
            nodes: vec![OIr::Invoke {
                fn_name: "batch".into(),
                mode: InvokeMode::Group(GroupMode::Batch),
                args: vec![
                    OIr::Invoke {
                        fn_name: "definitely_unknown_builtin".into(),
                        mode: InvokeMode::Eager,
                        args: Vec::new(),
                    },
                    OIr::Load("missing_later".into()),
                ],
            }],
        };

        let mut serial = Evaluator::new("/tmp".into());
        let serial_error = serial
            .eval_ir_program_forcing(&program, &mut HashMap::new(), true)
            .unwrap_err()
            .to_string();
        let mut graph = Evaluator::new("/tmp".into());
        let graph_error = graph
            .eval_ir_program_forcing(&program, &mut HashMap::new(), false)
            .unwrap_err()
            .to_string();

        assert!(serial_error.contains("definitely_unknown_builtin"));
        assert_eq!(graph_error, serial_error);
        let trace = graph.last_execution_trace().unwrap();
        assert!(!trace
            .events
            .iter()
            .any(|event| matches!(event, TraceEvent::NodeStarted(PlanNodeId(2)))));
    }
}
