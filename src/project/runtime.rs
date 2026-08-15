//! The route runtime: build prerequisites and run entrypoints natively.
//!
//! Executing one route materializes a single isolated workspace, runs the
//! route's `prerequisites` first (in that same workspace, so build outputs are
//! visible to the runner), evaluates guards, spawns the command, captures
//! stdout/stderr/exit code/duration, decodes JSON results, and collects
//! declared output globs as artifacts.
//!
//! Route sets are executed under their policy. Alternatives never all execute
//! by default — only the selected policy activates them, and `Default` requires
//! an unambiguous default route or an explicit selection. The parallel
//! policies (`race_success`, `race_settle`, `verify_equivalent`,
//! `benchmark_and_select`) run every alternative concurrently, each in its own
//! isolated workspace, and cancel losers cooperatively where the policy
//! permits it. Selection is deterministic: when several alternatives settle
//! successfully, the one earliest in declaration order wins.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::executor::CancellationToken;
#[cfg(target_os = "linux")]
use crate::process::linux_process_observation_disappeared;

use super::materialize::{materialize_isolated, Workspace};
use super::model::{
    Artifact, ArtifactCaptureFailure, ArtifactCaptureStatus, ExecutionProvenance, OExecutionResult,
    OutputCapture, ProjectBundle, ResultCodec, RouteExecutionDisposition, RouteGuard, RoutePolicy,
    RouteSpec,
};

/// How unmet guards are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardBehavior {
    /// Fail if a guard is not satisfied.
    Enforce,
    /// Skip the route (return a synthetic no-op result) if a guard fails.
    Skip,
}

/// Which ambient variables are visible to a route before its declared
/// environment overlay is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentPolicy {
    /// Preserve the historical behavior explicitly: inherit every host value.
    InheritAll,
    /// Start from an empty environment.
    Clear,
    /// Inherit only the named variables when they exist on the host.
    AllowList(BTreeSet<String>),
}

/// Which OS process identity the coordinator owns and makes terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTreePolicy {
    /// On Unix, own the fresh process group created for the route. Descendants
    /// that deliberately leave that group are outside this policy's boundary.
    OwnedProcessGroup,
    /// Own only the directly spawned process. This is the explicit fallback on
    /// platforms without the Unix process-group implementation.
    LeaderOnly,
}

/// Coherent resource and host-interaction limits for one spawned route.
///
/// The defaults retain the historical environment behavior while making time,
/// output, artifacts, and owned-process lifetime finite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionLimits {
    pub wall_clock_timeout: Duration,
    pub termination_grace_period: Duration,
    pub max_retained_stdout_bytes: usize,
    pub max_retained_stderr_bytes: usize,
    pub max_routes_per_selection: usize,
    pub max_selection_retained_output_bytes: u64,
    pub max_artifact_count: usize,
    pub max_artifact_scan_entries: usize,
    pub max_aggregate_artifact_bytes: u64,
    pub max_single_artifact_bytes: u64,
    pub environment_policy: EnvironmentPolicy,
    pub process_tree_policy: ProcessTreePolicy,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            wall_clock_timeout: Duration::from_secs(30 * 60),
            // Allow a successfully force-signaled descendant to reach an
            // inert kernel state even when the host test/build load delays
            // scheduling. This remains a strict, configured upper bound.
            termination_grace_period: Duration::from_secs(2),
            max_retained_stdout_bytes: 16 * 1024 * 1024,
            max_retained_stderr_bytes: 16 * 1024 * 1024,
            max_routes_per_selection: 64,
            max_selection_retained_output_bytes: 512 * 1024 * 1024,
            max_artifact_count: 4_096,
            max_artifact_scan_entries: 1_000_000,
            max_aggregate_artifact_bytes: 4 * 1024 * 1024 * 1024,
            max_single_artifact_bytes: 1024 * 1024 * 1024,
            environment_policy: EnvironmentPolicy::InheritAll,
            process_tree_policy: if cfg!(unix) {
                ProcessTreePolicy::OwnedProcessGroup
            } else {
                ProcessTreePolicy::LeaderOnly
            },
        }
    }
}

impl ExecutionLimits {
    fn validate(&self) -> std::result::Result<(), RouteExecutionError> {
        if self.wall_clock_timeout.is_zero() {
            return Err(RouteExecutionError::Configuration {
                detail: "wall-clock timeout must be greater than zero".to_string(),
            });
        }
        if self.termination_grace_period.is_zero() {
            return Err(RouteExecutionError::Configuration {
                detail: "termination grace period must be greater than zero".to_string(),
            });
        }
        if self.max_single_artifact_bytes > self.max_aggregate_artifact_bytes {
            return Err(RouteExecutionError::Configuration {
                detail: "single-artifact limit exceeds aggregate-artifact limit".to_string(),
            });
        }
        if self.max_routes_per_selection == 0 {
            return Err(RouteExecutionError::Configuration {
                detail: "route-selection limit must be greater than zero".to_string(),
            });
        }
        if self.max_artifact_count > self.max_artifact_scan_entries {
            return Err(RouteExecutionError::Configuration {
                detail: "artifact count limit exceeds artifact scan-entry limit".to_string(),
            });
        }
        #[cfg(not(unix))]
        if self.process_tree_policy == ProcessTreePolicy::OwnedProcessGroup {
            return Err(RouteExecutionError::Configuration {
                detail: "owned-process-group policy is unsupported on this platform".to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn validate_route_execution_set(
        &self,
        potential_route_executions: usize,
    ) -> std::result::Result<(), RouteExecutionError> {
        self.validate()?;
        if potential_route_executions > self.max_routes_per_selection {
            return Err(RouteExecutionError::Configuration {
                detail: format!(
                    "route selection could execute {potential_route_executions} routes including prerequisites; configured maximum is {}",
                    self.max_routes_per_selection
                ),
            });
        }
        let per_route = (self.max_retained_stdout_bytes as u128)
            .checked_add(self.max_retained_stderr_bytes as u128)
            .ok_or_else(|| RouteExecutionError::Configuration {
                detail: "per-route retained-output bound overflowed".to_string(),
            })?;
        let selection_bound = per_route
            .checked_mul(potential_route_executions as u128)
            .ok_or_else(|| RouteExecutionError::Configuration {
                detail: "route-selection retained-output bound overflowed".to_string(),
            })?;
        if selection_bound > u128::from(self.max_selection_retained_output_bytes) {
            return Err(RouteExecutionError::Configuration {
                detail: format!(
                    "route selection could retain {selection_bound} output bytes; configured maximum is {}",
                    self.max_selection_retained_output_bytes
                ),
            });
        }
        Ok(())
    }
}

/// Options controlling route execution.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// How to treat unmet guards.
    pub guard_behavior: GuardBehavior,
    /// Bounded execution and explicit host-interaction policy.
    pub limits: ExecutionLimits,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            guard_behavior: GuardBehavior::Enforce,
            limits: ExecutionLimits::default(),
        }
    }
}

/// Typed infrastructure failures from one route process.
#[derive(Debug, Error)]
pub enum RouteExecutionError {
    #[error("route execution configuration is invalid: {detail}")]
    Configuration { detail: String },
    #[error("route `{route_id}` was canceled")]
    Cancelled { route_id: String },
    #[error("route `{route_id}` exceeded its wall-clock timeout of {timeout:?}")]
    DeadlineExceeded { route_id: String, timeout: Duration },
    #[error("failed to spawn `{command}`")]
    Spawn {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("failed waiting on `{command}`")]
    Wait {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("route `{route_id}` {stream} capture failed: {detail}")]
    OutputCapture {
        route_id: String,
        stream: &'static str,
        detail: String,
    },
    #[error("route `{route_id}` process terminality failed: {detail}")]
    ProcessTreeTermination { route_id: String, detail: String },
}

/// Fail-closed outcomes while capturing declared route artifacts.
///
/// An [`Artifact`] is constructed only for the `Captured` case represented by
/// `Ok`. Every other state remains a typed error, so missing or uncertain
/// evidence cannot be confused with a legitimate empty file.
#[derive(Debug, Error)]
pub enum ArtifactCaptureError {
    #[error("declared artifact `{requirement}` is missing")]
    Missing { requirement: String },
    #[error("cannot {operation} artifact `{path}`")]
    Unreadable {
        path: String,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("artifact `{path}` changed during capture: {detail}")]
    ChangedDuringCapture { path: String, detail: String },
    #[error("artifact `{path}` has an unsupported file type")]
    UnsupportedFileType { path: String },
    #[error("artifact path `{path}` is not valid UTF-8 and cannot be represented canonically")]
    UnsupportedPathEncoding {
        path: PathBuf,
        relative_path_sha256: String,
    },
    #[error("artifact path `{path}` resolves outside the isolated workspace")]
    OutsideArtifactRoot { path: String },
    #[error(
        "artifact scan exceeded configured entry limit {limit} (observed at least {observed_at_least})"
    )]
    ArtifactScanLimit {
        limit: usize,
        observed_at_least: usize,
    },
    #[error(
        "artifact count exceeded configured limit {limit} (observed at least {observed_at_least})"
    )]
    ArtifactCountLimit {
        limit: usize,
        observed_at_least: usize,
    },
    #[error(
        "artifact `{path}` exceeded configured single-artifact limit {limit} bytes (observed at least {observed_at_least})"
    )]
    SingleArtifactLimit {
        path: String,
        limit: u64,
        observed_at_least: u64,
    },
    #[error(
        "artifact `{path}` would exceed aggregate limit {limit} bytes after {captured_before} bytes (artifact bytes {artifact_bytes})"
    )]
    AggregateArtifactLimit {
        path: String,
        limit: u64,
        captured_before: u64,
        artifact_bytes: u64,
    },
    #[error("artifact capture was canceled")]
    CaptureCancelled,
    #[error("artifact capture exceeded the route wall-clock deadline")]
    CaptureDeadlineExceeded,
}

impl ArtifactCaptureError {
    fn evidence(&self) -> ArtifactCaptureFailure {
        match self {
            Self::Missing { requirement } => ArtifactCaptureFailure::Missing {
                requirement: requirement.clone(),
            },
            Self::Unreadable {
                path,
                operation,
                source,
            } => ArtifactCaptureFailure::Unreadable {
                path: path.clone(),
                operation: (*operation).to_string(),
                error_kind: io_error_kind_token(source.kind()).to_string(),
            },
            Self::ChangedDuringCapture { path, detail } => {
                ArtifactCaptureFailure::ChangedDuringCapture {
                    path: path.clone(),
                    detail: detail.clone(),
                }
            }
            Self::UnsupportedFileType { path } => {
                ArtifactCaptureFailure::UnsupportedFileType { path: path.clone() }
            }
            Self::UnsupportedPathEncoding {
                relative_path_sha256,
                ..
            } => ArtifactCaptureFailure::UnsupportedPathEncoding {
                path_sha256: relative_path_sha256.clone(),
            },
            Self::OutsideArtifactRoot { path } => {
                ArtifactCaptureFailure::OutsideArtifactRoot { path: path.clone() }
            }
            Self::ArtifactScanLimit {
                limit,
                observed_at_least,
            } => ArtifactCaptureFailure::ArtifactScanLimit {
                limit: *limit,
                observed_at_least: *observed_at_least,
            },
            Self::ArtifactCountLimit {
                limit,
                observed_at_least,
            } => ArtifactCaptureFailure::ArtifactCountLimit {
                limit: *limit,
                observed_at_least: *observed_at_least,
            },
            Self::SingleArtifactLimit {
                path,
                limit,
                observed_at_least,
            } => ArtifactCaptureFailure::SingleArtifactLimit {
                path: path.clone(),
                limit: *limit,
                observed_at_least: *observed_at_least,
            },
            Self::AggregateArtifactLimit {
                path,
                limit,
                captured_before,
                artifact_bytes,
            } => ArtifactCaptureFailure::AggregateArtifactLimit {
                path: path.clone(),
                limit: *limit,
                captured_before: *captured_before,
                artifact_bytes: *artifact_bytes,
            },
            Self::CaptureCancelled | Self::CaptureDeadlineExceeded => {
                ArtifactCaptureFailure::NotAttempted {
                    reason: "artifact_capture_interrupted".to_string(),
                }
            }
        }
    }
}

fn io_error_kind_token(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::AlreadyExists => "already_exists",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::WriteZero => "write_zero",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::OutOfMemory => "out_of_memory",
        _ => "other",
    }
}

/// Marker embedded in the stderr of a guard-skipped result.
const SKIP_MARKER: &str = "[olang:route-skipped]";

// ─────────────────────────────────────────────────────────────────────────────
// Guards
// ─────────────────────────────────────────────────────────────────────────────

/// Return the first unmet guard's description, if any.
pub fn unmet_guard(route: &RouteSpec) -> Option<String> {
    unmet_guard_with_environment(route, &EnvironmentPolicy::InheritAll)
}

fn unmet_guard_with_environment(
    route: &RouteSpec,
    environment_policy: &EnvironmentPolicy,
) -> Option<String> {
    for guard in &route.guards {
        match guard {
            RouteGuard::PlatformOs(os) => {
                if !std::env::consts::OS.eq_ignore_ascii_case(os) {
                    return Some(format!(
                        "requires OS `{os}` (host is `{}`)",
                        std::env::consts::OS
                    ));
                }
            }
            RouteGuard::CommandAvailable(cmd) => {
                if !command_on_path(cmd, route, environment_policy) {
                    return Some(format!("requires command `{cmd}` on PATH"));
                }
            }
            RouteGuard::EnvVarSet(var) => {
                if effective_environment_value(route, environment_policy, var)
                    .is_none_or(|value| value.is_empty())
                {
                    return Some(format!("requires environment variable `{var}` to be set"));
                }
            }
        }
    }
    None
}

/// Resolve a command name against `PATH` (or accept an explicit path).
fn command_on_path(cmd: &str, route: &RouteSpec, environment_policy: &EnvironmentPolicy) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).exists();
    }
    let Some(path) = effective_environment_value(route, environment_policy, "PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(cmd);
        candidate.is_file() && is_executable(&candidate)
    })
}

fn effective_environment_value(
    route: &RouteSpec,
    environment_policy: &EnvironmentPolicy,
    key: &str,
) -> Option<std::ffi::OsString> {
    if let Some(value) = route.environment.get(key) {
        return Some(value.into());
    }
    match environment_policy {
        EnvironmentPolicy::InheritAll => std::env::var_os(key),
        EnvironmentPolicy::Clear => None,
        EnvironmentPolicy::AllowList(keys) if keys.contains(key) => std::env::var_os(key),
        EnvironmentPolicy::AllowList(_) => None,
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

// ─────────────────────────────────────────────────────────────────────────────
// Single-route execution
// ─────────────────────────────────────────────────────────────────────────────

struct RunCtx<'a> {
    bundle: &'a ProjectBundle,
    opts: &'a RunOptions,
    cancel: CancellationToken,
    done: HashMap<String, OExecutionResult>,
    skipped: HashSet<String>,
    stack: Vec<String>,
}

/// Run a single route (and its prerequisite chain) in a fresh isolated
/// workspace.
pub fn run_route(
    bundle: &ProjectBundle,
    route_id: &str,
    opts: &RunOptions,
) -> Result<OExecutionResult> {
    run_route_cancellable(bundle, route_id, opts, CancellationToken::new())
}

/// Like [`run_route`], but observes a cooperative cancellation token: when the
/// token trips, the running child process is killed and a structured
/// "canceled" error is returned. Used by the parallel route policies to stop
/// losing alternatives.
pub fn run_route_cancellable(
    bundle: &ProjectBundle,
    route_id: &str,
    opts: &RunOptions,
    cancel: CancellationToken,
) -> Result<OExecutionResult> {
    if cancel.is_cancelled() {
        return Err(RouteExecutionError::Cancelled {
            route_id: route_id.to_string(),
        }
        .into());
    }
    if bundle.route(route_id).is_none() {
        bail!("no route named `{route_id}`");
    }
    let selected = [route_id.to_string()];
    let potential_route_executions = potential_route_execution_count(bundle, &selected)?;
    opts.limits
        .validate_route_execution_set(potential_route_executions)?;
    let workspace =
        materialize_isolated(bundle).context("failed to materialize an isolated workspace")?;
    let mut ctx = RunCtx {
        bundle,
        opts,
        cancel,
        done: HashMap::new(),
        skipped: HashSet::new(),
        stack: Vec::new(),
    };
    execute_in_workspace(&mut ctx, route_id, &workspace)
}

fn execute_in_workspace(
    ctx: &mut RunCtx<'_>,
    route_id: &str,
    workspace: &Workspace,
) -> Result<OExecutionResult> {
    if let Some(existing) = ctx.done.get(route_id) {
        return Ok(existing.clone());
    }
    if ctx.stack.iter().any(|id| id == route_id) {
        let mut cycle = ctx.stack.clone();
        cycle.push(route_id.to_string());
        bail!("prerequisite cycle detected: {}", cycle.join(" -> "));
    }

    let route = ctx
        .bundle
        .route(route_id)
        .cloned()
        .with_context(|| format!("prerequisite route `{route_id}` not found"))?;

    ctx.stack.push(route_id.to_string());

    // ── Prerequisites (shared workspace) ────────────────────────────────────
    for prereq in &route.prerequisites {
        let result = execute_in_workspace(ctx, prereq, workspace)?;
        if !result.succeeded() && !ctx.skipped.contains(prereq) {
            ctx.stack.pop();
            bail!(
                "prerequisite `{prereq}` of `{route_id}` failed (exit {:?})",
                result.exit_code
            );
        }
    }

    // ── Execute this route (including guards) ─────────────────────────────
    let result = execute_route_in_workspace(&route, workspace, ctx.opts, &ctx.cancel)?;
    ctx.stack.pop();
    if is_skipped_result(&result) {
        ctx.skipped.insert(route_id.to_string());
    }
    ctx.done.insert(route_id.to_string(), result.clone());
    Ok(result)
}

/// Execute exactly one route in an already-materialized workspace.
///
/// This primitive evaluates the route's guards, runs its command, decodes its
/// value, and collects its declared artifacts. It deliberately does not
/// allocate a workspace, visit prerequisites, select alternatives, or invoke
/// [`run_route`] or [`run_selection`]. Those concerns remain with the caller.
pub(crate) fn execute_route_in_workspace(
    route: &RouteSpec,
    workspace: &Workspace,
    opts: &RunOptions,
    cancel: &CancellationToken,
) -> Result<OExecutionResult> {
    opts.limits.validate()?;
    if let Some(reason) = unmet_guard_with_environment(route, &opts.limits.environment_policy) {
        return match opts.guard_behavior {
            GuardBehavior::Enforce => {
                bail!("route `{}` guard not satisfied: {reason}", route.id)
            }
            GuardBehavior::Skip => Ok(skipped_result(route, workspace, &reason)),
        };
    }

    spawn_route(route, workspace, opts, cancel)
}

pub(crate) fn is_skipped_result(result: &OExecutionResult) -> bool {
    result.was_guard_skipped()
}

fn skipped_result(route: &RouteSpec, workspace: &Workspace, reason: &str) -> OExecutionResult {
    let stdout = Vec::new();
    let stderr = format!("{SKIP_MARKER} {reason}\n").into_bytes();
    let artifact_capture = if route.outputs.is_empty() {
        ArtifactCaptureStatus::Complete
    } else {
        ArtifactCaptureStatus::Incomplete {
            failure: Box::new(ArtifactCaptureFailure::NotAttempted {
                reason: "route_guard_skipped".to_string(),
            }),
        }
    };
    OExecutionResult {
        route_id: route.id.clone(),
        exit_code: None,
        stdout_capture: OutputCapture::complete(&stdout),
        stdout,
        stderr_capture: OutputCapture::complete(&stderr),
        stderr,
        value: None,
        artifacts: Vec::new(),
        artifact_requirements: route.outputs.clone(),
        artifact_capture,
        disposition: RouteExecutionDisposition::GuardSkipped,
        duration_ns: 0,
        provenance: ExecutionProvenance {
            workspace: workspace.root.clone(),
            command: route.full_command(),
            cwd: workspace.root.clone(),
        },
    }
}

/// Whether an error came from cooperative route cancellation.
pub fn is_cancellation_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RouteExecutionError>()
        .is_some_and(|error| matches!(error, RouteExecutionError::Cancelled { .. }))
}

/// Whether an error came from the route's wall-clock deadline.
pub fn is_timeout_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RouteExecutionError>()
        .is_some_and(|error| matches!(error, RouteExecutionError::DeadlineExceeded { .. }))
}

fn spawn_route(
    route: &RouteSpec,
    workspace: &Workspace,
    opts: &RunOptions,
    cancel: &CancellationToken,
) -> Result<OExecutionResult> {
    let command = route.full_command();
    if command.is_empty() {
        bail!("route `{}` has an empty command", route.id);
    }
    if cancel.is_cancelled() {
        return Err(RouteExecutionError::Cancelled {
            route_id: route.id.clone(),
        }
        .into());
    }

    let cwd = resolve_cwd(&workspace.root, &route.working_directory)?;

    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    cmd.current_dir(&cwd);
    configure_child_environment(&mut cmd, route, &opts.limits.environment_policy);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // The owned-group policy is explicit. A descendant that deliberately
    // creates another process group/session is outside this POSIX boundary.
    #[cfg(unix)]
    if opts.limits.process_tree_policy == ProcessTreePolicy::OwnedProcessGroup {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let start = Instant::now();
    let deadline = start
        .checked_add(opts.limits.wall_clock_timeout)
        .ok_or_else(|| RouteExecutionError::Configuration {
            detail: "wall-clock timeout overflows the monotonic clock".to_string(),
        })?;
    let child = cmd.spawn().map_err(|source| RouteExecutionError::Spawn {
        command: command.join(" "),
        source,
    })?;
    let mut process = OwnedRouteProcess::new(child, opts.limits.process_tree_policy);

    // Drain pipes on helper threads so the child never blocks on a full pipe
    // while the coordinator polls for exit, timeout, or cancellation. Unix
    // read ends are nonblocking so an escaped pipe holder cannot trap a helper
    // thread forever after the owned group has been terminated.
    let stdout_pipe = match process.child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            terminate_after_coordinator_failure(
                &mut process,
                route,
                opts.limits.termination_grace_period,
            )?;
            return Err(RouteExecutionError::OutputCapture {
                route_id: route.id.clone(),
                stream: "stdout",
                detail: "spawned child has no piped stdout".to_string(),
            }
            .into());
        }
    };
    let stderr_pipe = match process.child.stderr.take() {
        Some(pipe) => pipe,
        None => {
            terminate_after_coordinator_failure(
                &mut process,
                route,
                opts.limits.termination_grace_period,
            )?;
            return Err(RouteExecutionError::OutputCapture {
                route_id: route.id.clone(),
                stream: "stderr",
                detail: "spawned child has no piped stderr".to_string(),
            }
            .into());
        }
    };
    let terminal = Arc::new(AtomicBool::new(false));
    let stdout_task = match spawn_pipe_drain(
        stdout_pipe,
        opts.limits.max_retained_stdout_bytes,
        Arc::clone(&terminal),
        opts.limits.termination_grace_period,
        "stdout",
    ) {
        Ok(task) => task,
        Err(error) => {
            terminate_after_coordinator_failure(
                &mut process,
                route,
                opts.limits.termination_grace_period,
            )?;
            return Err(RouteExecutionError::OutputCapture {
                route_id: route.id.clone(),
                stream: "stdout",
                detail: error.to_string(),
            }
            .into());
        }
    };
    let stderr_task = match spawn_pipe_drain(
        stderr_pipe,
        opts.limits.max_retained_stderr_bytes,
        Arc::clone(&terminal),
        opts.limits.termination_grace_period,
        "stderr",
    ) {
        Ok(task) => task,
        Err(error) => {
            let cleanup = terminate_after_coordinator_failure(
                &mut process,
                route,
                opts.limits.termination_grace_period,
            );
            terminal.store(true, Ordering::Release);
            let stdout = finish_pipe_drain(stdout_task, route, drain_finish_deadline(&opts.limits));
            cleanup?;
            stdout?;
            return Err(RouteExecutionError::OutputCapture {
                route_id: route.id.clone(),
                stream: "stderr",
                detail: error.to_string(),
            }
            .into());
        }
    };

    let stop = loop {
        if cancel.is_cancelled() {
            break RouteStop::Cancelled;
        }
        if Instant::now() >= deadline {
            break RouteStop::DeadlineExceeded;
        }
        match process.exited_without_reaping() {
            Ok(true) => break RouteStop::LeaderExited,
            Ok(false) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(source) => {
                let cleanup = terminate_after_coordinator_failure(
                    &mut process,
                    route,
                    opts.limits.termination_grace_period,
                );
                terminal.store(true, Ordering::Release);
                let finish_deadline = drain_finish_deadline(&opts.limits);
                let stdout = finish_pipe_drain(stdout_task, route, finish_deadline);
                let stderr = finish_pipe_drain(stderr_task, route, finish_deadline);
                cleanup?;
                stdout?;
                stderr?;
                return Err(RouteExecutionError::Wait {
                    command: command.join(" "),
                    source,
                }
                .into());
            }
        }
    };

    let terminal_result = match stop {
        RouteStop::LeaderExited => {
            process.finish_after_leader_exit(route, opts.limits.termination_grace_period)
        }
        RouteStop::Cancelled | RouteStop::DeadlineExceeded => {
            process.terminate_with_grace(route, opts.limits.termination_grace_period)
        }
    };
    terminal.store(true, Ordering::Release);
    let finish_deadline = drain_finish_deadline(&opts.limits);
    let stdout = finish_pipe_drain(stdout_task, route, finish_deadline);
    let stderr = finish_pipe_drain(stderr_task, route, finish_deadline);
    let status = terminal_result?;
    let stdout = stdout?;
    let stderr = stderr?;

    match stop {
        RouteStop::Cancelled => {
            return Err(RouteExecutionError::Cancelled {
                route_id: route.id.clone(),
            }
            .into())
        }
        RouteStop::DeadlineExceeded => {
            return Err(RouteExecutionError::DeadlineExceeded {
                route_id: route.id.clone(),
                timeout: opts.limits.wall_clock_timeout,
            }
            .into())
        }
        RouteStop::LeaderExited => {}
    }

    let value = match route.result_codec {
        ResultCodec::Json if !stdout.capture.truncated => {
            serde_json::from_slice::<serde_json::Value>(&stdout.retained).ok()
        }
        _ => None,
    };

    let artifact_budget = ArtifactCaptureBudget { deadline, cancel };
    let (artifacts, artifact_capture) = match collect_artifacts(
        &workspace.root,
        &route.outputs,
        &opts.limits,
        &artifact_budget,
    ) {
        Ok(artifacts) => (artifacts, ArtifactCaptureStatus::Complete),
        Err(ArtifactCaptureError::CaptureCancelled) => {
            return Err(RouteExecutionError::Cancelled {
                route_id: route.id.clone(),
            }
            .into())
        }
        Err(ArtifactCaptureError::CaptureDeadlineExceeded) => {
            return Err(RouteExecutionError::DeadlineExceeded {
                route_id: route.id.clone(),
                timeout: opts.limits.wall_clock_timeout,
            }
            .into())
        }
        Err(error) if status.success() => return Err(error.into()),
        Err(error) => (
            Vec::new(),
            ArtifactCaptureStatus::Incomplete {
                failure: Box::new(error.evidence()),
            },
        ),
    };
    let duration_ns = start.elapsed().as_nanos();

    Ok(OExecutionResult {
        route_id: route.id.clone(),
        exit_code: status.code(),
        stdout: stdout.retained,
        stdout_capture: stdout.capture,
        stderr: stderr.retained,
        stderr_capture: stderr.capture,
        value,
        artifacts,
        artifact_requirements: route.outputs.clone(),
        artifact_capture,
        disposition: RouteExecutionDisposition::Executed,
        duration_ns,
        provenance: ExecutionProvenance {
            workspace: workspace.root.clone(),
            command,
            cwd,
        },
    })
}

fn terminate_after_coordinator_failure(
    process: &mut OwnedRouteProcess,
    route: &RouteSpec,
    grace: Duration,
) -> std::result::Result<(), RouteExecutionError> {
    process.terminate_with_grace(route, grace).map(drop)
}

fn configure_child_environment(
    command: &mut std::process::Command,
    route: &RouteSpec,
    policy: &EnvironmentPolicy,
) {
    match policy {
        EnvironmentPolicy::InheritAll => {}
        EnvironmentPolicy::Clear => {
            command.env_clear();
        }
        EnvironmentPolicy::AllowList(keys) => {
            command.env_clear();
            for key in keys {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
        }
    }
    command.envs(&route.environment);
}

#[derive(Clone, Copy)]
enum RouteStop {
    LeaderExited,
    Cancelled,
    DeadlineExceeded,
}

struct DrainedOutput {
    retained: Vec<u8>,
    capture: OutputCapture,
    reached_eof: bool,
}

struct PipeDrainTask {
    stream: &'static str,
    receiver: mpsc::Receiver<io::Result<DrainedOutput>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
fn spawn_pipe_drain<R>(
    pipe: R,
    retained_limit: usize,
    terminal: Arc<AtomicBool>,
    close_grace: Duration,
    stream: &'static str,
) -> io::Result<PipeDrainTask>
where
    R: Read + std::os::fd::AsRawFd + Send + 'static,
{
    set_nonblocking(&pipe)?;
    spawn_pipe_drain_inner(pipe, retained_limit, terminal, close_grace, stream)
}

#[cfg(not(unix))]
fn spawn_pipe_drain<R>(
    pipe: R,
    retained_limit: usize,
    terminal: Arc<AtomicBool>,
    close_grace: Duration,
    stream: &'static str,
) -> io::Result<PipeDrainTask>
where
    R: Read + Send + 'static,
{
    spawn_pipe_drain_inner(pipe, retained_limit, terminal, close_grace, stream)
}

fn spawn_pipe_drain_inner<R>(
    pipe: R,
    retained_limit: usize,
    terminal: Arc<AtomicBool>,
    close_grace: Duration,
    stream: &'static str,
) -> io::Result<PipeDrainTask>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name(format!("olang-route-{stream}"))
        .spawn(move || {
            let result = drain_pipe(pipe, retained_limit, &terminal, close_grace);
            let _ = sender.send(result);
        })?;
    Ok(PipeDrainTask {
        stream,
        receiver,
        handle: Some(handle),
    })
}

fn drain_pipe(
    mut pipe: impl Read,
    retained_limit: usize,
    terminal: &AtomicBool,
    close_grace: Duration,
) -> io::Result<DrainedOutput> {
    let mut retained = Vec::with_capacity(retained_limit.min(64 * 1024));
    let mut total_observed_bytes = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut terminal_observed_at = None;

    let reached_eof = loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break true,
            Ok(count) => {
                total_observed_bytes = total_observed_bytes
                    .checked_add(count as u64)
                    .ok_or_else(|| io::Error::other("observed output length overflowed u64"))?;
                hasher.update(&buffer[..count]);
                let remaining = retained_limit.saturating_sub(retained.len());
                retained.extend_from_slice(&buffer[..count.min(remaining)]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if terminal.load(Ordering::Acquire) {
                    let observed = terminal_observed_at.get_or_insert_with(Instant::now);
                    if observed.elapsed() >= close_grace {
                        break false;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    };

    let retained_bytes = u64::try_from(retained.len())
        .map_err(|_| io::Error::other("retained output length does not fit u64"))?;
    Ok(DrainedOutput {
        capture: OutputCapture {
            total_observed_bytes,
            retained_bytes,
            truncated: total_observed_bytes > retained_bytes,
            sha256: hex::encode(hasher.finalize()),
        },
        retained,
        reached_eof,
    })
}

fn finish_pipe_drain(
    mut task: PipeDrainTask,
    route: &RouteSpec,
    deadline: Instant,
) -> std::result::Result<DrainedOutput, RouteExecutionError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let received = task.receiver.recv_timeout(remaining);
    let joined = match task.handle.take() {
        Some(handle) if !matches!(received, Err(mpsc::RecvTimeoutError::Timeout)) => {
            Some(handle.join())
        }
        _ => None,
    };
    if joined.is_some_and(|result| result.is_err()) {
        return Err(RouteExecutionError::OutputCapture {
            route_id: route.id.clone(),
            stream: task.stream,
            detail: "drain thread panicked".to_string(),
        });
    }
    let output = match received {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(RouteExecutionError::OutputCapture {
                route_id: route.id.clone(),
                stream: task.stream,
                detail: error.to_string(),
            })
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(RouteExecutionError::ProcessTreeTermination {
                route_id: route.id.clone(),
                detail: format!(
                    "{} pipe remained open after the owned process became terminal",
                    task.stream
                ),
            })
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(RouteExecutionError::OutputCapture {
                route_id: route.id.clone(),
                stream: task.stream,
                detail: "drain thread disconnected without a result".to_string(),
            })
        }
    };
    if !output.reached_eof {
        return Err(RouteExecutionError::ProcessTreeTermination {
            route_id: route.id.clone(),
            detail: format!(
                "{} pipe did not reach EOF after the owned process group was terminated",
                task.stream
            ),
        });
    }
    output
        .capture
        .validate_for_retained(&output.retained)
        .map_err(|detail| RouteExecutionError::OutputCapture {
            route_id: route.id.clone(),
            stream: task.stream,
            detail,
        })?;
    Ok(output)
}

fn drain_finish_deadline(limits: &ExecutionLimits) -> Instant {
    Instant::now()
        .checked_add(
            limits
                .termination_grace_period
                .saturating_add(Duration::from_millis(250)),
        )
        .unwrap_or_else(Instant::now)
}

#[cfg(unix)]
fn set_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    let descriptor = pipe.as_raw_fd();
    // SAFETY: `descriptor` belongs to the live pipe value and both fcntl calls
    // only inspect/update its file-status flags.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct OwnedRouteProcess {
    child: Child,
    policy: ProcessTreePolicy,
    reaped_status: Option<ExitStatus>,
}

struct ForcedTermination {
    owned_group: Option<io::Result<()>>,
    leader: io::Result<()>,
}

impl ForcedTermination {
    fn into_policy_result(self, policy: ProcessTreePolicy) -> io::Result<()> {
        match policy {
            ProcessTreePolicy::OwnedProcessGroup => {
                let group = self.owned_group.ok_or_else(|| {
                    io::Error::other("owned process-group termination result is missing")
                })?;
                match (group, self.leader) {
                    (Ok(()), _) => Ok(()),
                    (Err(group_error), Ok(())) => Err(group_error),
                    (Err(group_error), Err(leader_error)) => Err(io::Error::other(format!(
                        "owned process-group SIGKILL failed: {group_error}; direct leader SIGKILL also failed: {leader_error}"
                    ))),
                }
            }
            ProcessTreePolicy::LeaderOnly => self.leader,
        }
    }
}

impl OwnedRouteProcess {
    fn new(child: Child, policy: ProcessTreePolicy) -> Self {
        Self {
            child,
            policy,
            reaped_status: None,
        }
    }

    #[cfg(unix)]
    fn exited_without_reaping(&mut self) -> io::Result<bool> {
        loop {
            // SAFETY: waitid initializes siginfo for this exact direct child.
            // WNOWAIT observes exit without releasing the PID that anchors the
            // owned process-group identity.
            let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let status = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    &mut information,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if status == 0 {
                // SAFETY: successful waitid initialized `information`; si_pid
                // is zero when WNOHANG found no waitable state.
                return Ok(unsafe { information.si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    #[cfg(not(unix))]
    fn exited_without_reaping(&mut self) -> io::Result<bool> {
        if self.reaped_status.is_some() {
            return Ok(true);
        }
        if let Some(status) = self.child.try_wait()? {
            self.reaped_status = Some(status);
            return Ok(true);
        }
        Ok(false)
    }

    fn finish_after_leader_exit(
        &mut self,
        route: &RouteSpec,
        grace: Duration,
    ) -> std::result::Result<ExitStatus, RouteExecutionError> {
        let mut cleanup = if self.policy == ProcessTreePolicy::OwnedProcessGroup {
            let forced = self.force_terminate();
            #[cfg(target_os = "macos")]
            let forced = self.normalize_darwin_forced_group_error(forced);
            forced.into_policy_result(self.policy)
        } else {
            Ok(())
        };
        if cleanup.is_ok() {
            cleanup = self.wait_for_owned_group_quiescence(grace);
        }
        let status = self.wait().map_err(|source| RouteExecutionError::Wait {
            command: route.full_command().join(" "),
            source,
        })?;
        cleanup.map_err(|error| RouteExecutionError::ProcessTreeTermination {
            route_id: route.id.clone(),
            detail: error.to_string(),
        })?;
        Ok(status)
    }

    fn terminate_with_grace(
        &mut self,
        route: &RouteSpec,
        grace: Duration,
    ) -> std::result::Result<ExitStatus, RouteExecutionError> {
        let graceful_error = self.signal_graceful().err();
        if !grace.is_zero() {
            std::thread::sleep(grace);
        }
        let leader_exited_after_grace = self.exited_without_reaping();
        let forced = self.force_terminate();
        #[cfg(target_os = "macos")]
        let forced = if leader_exited_after_grace
            .as_ref()
            .is_ok_and(|exited| *exited)
            && self.policy == ProcessTreePolicy::OwnedProcessGroup
        {
            self.normalize_darwin_forced_group_error(forced)
        } else {
            forced
        };
        let force_result = forced.into_policy_result(self.policy);
        let quiescence_result = if force_result.is_ok() {
            self.wait_for_owned_group_quiescence(grace)
        } else {
            Ok(())
        };
        #[cfg(target_os = "macos")]
        let graceful_error = if leader_exited_after_grace
            .as_ref()
            .is_ok_and(|exited| *exited)
            && graceful_error
                .as_ref()
                .is_some_and(|error| error.raw_os_error() == Some(libc::EPERM))
            && force_result.is_ok()
            && quiescence_result.is_ok()
            && self.policy == ProcessTreePolicy::OwnedProcessGroup
        {
            // Darwin may report EPERM when a cancellation/deadline races a
            // naturally exited, zombie-only group. A successful force signal
            // plus explicit group enumeration proves that this stale graceful
            // error does not represent a live descendant.
            None
        } else {
            graceful_error
        };
        let leader_terminal_result = self.wait_for_leader_exit(grace);
        if let Err(error) = leader_terminal_result {
            let force_detail = force_result
                .as_ref()
                .err()
                .map_or_else(String::new, |force| format!("; SIGKILL error: {force}"));
            return Err(RouteExecutionError::ProcessTreeTermination {
                route_id: route.id.clone(),
                detail: format!(
                    "route leader did not become waitable after termination: {error}{force_detail}"
                ),
            });
        }
        let force_error = force_result.err();
        let status = self.wait().map_err(|source| RouteExecutionError::Wait {
            command: route.full_command().join(" "),
            source,
        })?;
        leader_exited_after_grace.map_err(|source| RouteExecutionError::Wait {
            command: route.full_command().join(" "),
            source,
        })?;
        quiescence_result.map_err(|error| RouteExecutionError::ProcessTreeTermination {
            route_id: route.id.clone(),
            detail: error.to_string(),
        })?;
        if let Some(error) = graceful_error.or(force_error) {
            return Err(RouteExecutionError::ProcessTreeTermination {
                route_id: route.id.clone(),
                detail: error.to_string(),
            });
        }
        Ok(status)
    }

    fn wait_for_leader_exit(&mut self, grace: Duration) -> io::Result<()> {
        let deadline = Instant::now()
            .checked_add(grace)
            .ok_or_else(|| io::Error::other("leader-exit grace deadline overflowed"))?;
        loop {
            if self.exited_without_reaping()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "leader remained active after SIGKILL",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_for_owned_group_quiescence(&self, grace: Duration) -> io::Result<()> {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = grace;
        if self.policy != ProcessTreePolicy::OwnedProcessGroup {
            return Ok(());
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let deadline = Instant::now()
                .checked_add(grace)
                .ok_or_else(|| io::Error::other("process-group grace deadline overflowed"))?;
            loop {
                if self.owned_group_has_no_active_descendants()? {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned process group still contains an active descendant after SIGKILL",
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
        {
            // POSIX has no portable process-group enumeration API. On these
            // Unix targets, successful delivery of the uncatchable SIGKILL is
            // the strongest portable terminality boundary available.
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }

    #[cfg(target_os = "macos")]
    fn owned_group_has_no_active_descendants(&self) -> io::Result<bool> {
        self.darwin_group_has_no_active_descendants()
    }

    #[cfg(target_os = "linux")]
    fn owned_group_has_no_active_descendants(&self) -> io::Result<bool> {
        let leader = i32::try_from(self.child.id())
            .map_err(|_| io::Error::other("child pid does not fit Linux pid_t"))?;
        for entry in std::fs::read_dir("/proc")? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if linux_process_observation_disappeared(&error) => continue,
                Err(error) => return Err(error),
            };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if pid == leader {
                continue;
            }
            let stat = match std::fs::read_to_string(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error) if linux_process_observation_disappeared(&error) => continue,
                Err(error) => return Err(error),
            };
            let close = stat.rfind(')').ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "malformed /proc process stat")
            })?;
            let mut fields = stat[close + 1..].split_whitespace();
            let state = fields.next();
            let _parent = fields.next();
            let group = fields
                .next()
                .and_then(|field| field.parse::<i32>().ok())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "malformed /proc process group")
                })?;
            // Orphaned grandchildren cannot be reaped by this coordinator.
            // A zombie has no executable state and cannot retain route pipes,
            // so it is terminal for the owned-process security boundary.
            // Linux reports zombies as `Z` and may expose the transient dead
            // states `X`/`x`. None can execute or retain a route pipe.
            if group == leader && !matches!(state, Some("Z" | "X" | "x")) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = &self.reaped_status {
            return Ok(*status);
        }
        let status = self.child.wait()?;
        self.reaped_status = Some(status);
        Ok(status)
    }

    #[cfg(target_os = "macos")]
    fn normalize_darwin_zombie_group_error(&self, signal_result: io::Result<()>) -> io::Result<()> {
        let Err(signal_error) = signal_result else {
            return Ok(());
        };
        if signal_error.raw_os_error() != Some(libc::EPERM) {
            return Err(signal_error);
        }
        match self.darwin_group_has_no_active_descendants() {
            Ok(true) => Ok(()),
            Ok(false) => Err(signal_error),
            Err(inspection_error) => Err(io::Error::other(format!(
                "{signal_error}; failed to prove Darwin process group has no active descendants: {inspection_error}"
            ))),
        }
    }

    #[cfg(target_os = "macos")]
    fn normalize_darwin_forced_group_error(
        &self,
        mut forced: ForcedTermination,
    ) -> ForcedTermination {
        if forced.owned_group.as_ref().is_some_and(|result| {
            result
                .as_ref()
                .is_err_and(|error| error.raw_os_error() == Some(libc::EPERM))
        }) {
            let group = forced
                .owned_group
                .take()
                .expect("owned-group result presence was checked");
            forced.owned_group = Some(self.normalize_darwin_zombie_group_error(group));
        }
        forced
    }

    #[cfg(target_os = "macos")]
    fn darwin_group_has_no_active_descendants(&self) -> io::Result<bool> {
        let leader = i32::try_from(self.child.id())
            .map_err(|_| io::Error::other("child pid does not fit Darwin pid_t"))?;
        // SAFETY: a null buffer asks libproc for a conservative PID capacity
        // and does not dereference memory.
        let capacity = unsafe { libc::proc_listpgrppids(leader, std::ptr::null_mut(), 0) };
        if capacity <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mut members = vec![0 as libc::pid_t; capacity as usize];
        let bytes = members
            .len()
            .checked_mul(std::mem::size_of::<libc::pid_t>())
            .and_then(|size| i32::try_from(size).ok())
            .ok_or_else(|| io::Error::other("Darwin process-group buffer is too large"))?;
        // SAFETY: `members` owns `bytes` writable bytes and remains alive for
        // the complete libproc call.
        let count = unsafe { libc::proc_listpgrppids(leader, members.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        let count = usize::try_from(count)
            .map_err(|_| io::Error::other("Darwin process-group count does not fit usize"))?;
        if count > members.len() {
            return Err(io::Error::other(
                "Darwin process-group enumeration exceeded its buffer",
            ));
        }
        members.truncate(count);
        for member in members.into_iter().filter(|member| *member != leader) {
            // SAFETY: `information` is valid writable storage of the exact
            // size supplied to libproc for the queried PID.
            let mut information: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let information_size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
                .map_err(|_| io::Error::other("Darwin process info buffer is too large"))?;
            let read = unsafe {
                libc::proc_pidinfo(
                    member,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut information as *mut libc::proc_bsdinfo).cast(),
                    information_size,
                )
            };
            if read <= 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                return Err(error);
            }
            if read != information_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Darwin returned a partial process info record",
                ));
            }
            // The PID can disappear and be reused between group enumeration
            // and inspection. Only a record still belonging to this anchored
            // group is relevant. Zombies are inert and cannot retain pipes.
            if information.pbi_pgid == leader as u32 && information.pbi_status != libc::SZOMB {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[cfg(unix)]
    fn signal_graceful(&mut self) -> io::Result<()> {
        self.signal_unix(libc::SIGTERM)
    }

    #[cfg(not(unix))]
    fn signal_graceful(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    #[cfg(unix)]
    fn force_terminate(&mut self) -> ForcedTermination {
        let owned_group = (self.policy == ProcessTreePolicy::OwnedProcessGroup)
            .then(|| self.signal_unix(libc::SIGKILL));
        // Also target the direct child while it remains unreaped. For an owned
        // group this covers a violated group invariant without weakening the
        // group error into success.
        let leader = self.child.kill();
        ForcedTermination {
            owned_group,
            leader,
        }
    }

    #[cfg(not(unix))]
    fn force_terminate(&mut self) -> ForcedTermination {
        ForcedTermination {
            owned_group: None,
            leader: self.child.kill(),
        }
    }

    #[cfg(unix)]
    fn signal_unix(&self, signal: libc::c_int) -> io::Result<()> {
        let pid = i32::try_from(self.child.id())
            .map_err(|_| io::Error::other("child pid does not fit Unix pid_t"))?;
        let target = match self.policy {
            ProcessTreePolicy::OwnedProcessGroup => -pid,
            ProcessTreePolicy::LeaderOnly => pid,
        };
        // SAFETY: the target is either the live, unreaped child or the fresh
        // process group anchored by that child. The leader remains unreaped
        // until all configured signals have been issued.
        if unsafe { libc::kill(target, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

impl Drop for OwnedRouteProcess {
    fn drop(&mut self) {
        if self.reaped_status.is_none() {
            let policy = self.policy;
            let _ = self.force_terminate().into_policy_result(policy);
            // Drop must never reintroduce an unbounded wait on an exceptional
            // signal-delivery failure. Normal paths prove exit before reaping.
            let _ = self.child.try_wait();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_process_observation_tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn repeated_proc_scans_tolerate_short_lived_process_churn() {
        const ITERATIONS: usize = 32;
        const CHILDREN_PER_ITERATION: usize = 32;

        for iteration in 0..ITERATIONS {
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg(format!(
                    "i=0; while [ \"$i\" -lt {CHILDREN_PER_ITERATION} ]; do (exit 0) & i=$((i + 1)); done; wait"
                ))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command.process_group(0);
            let child = command
                .spawn()
                .expect("short-lived process churn must spawn");
            let mut process = OwnedRouteProcess::new(child, ProcessTreePolicy::OwnedProcessGroup);
            let deadline = Instant::now() + Duration::from_secs(5);

            loop {
                process
                    .owned_group_has_no_active_descendants()
                    .unwrap_or_else(|error| {
                        panic!(
                            "iteration {iteration} treated a disappearing /proc entry as fatal: {error}"
                        )
                    });
                if process
                    .exited_without_reaping()
                    .expect("short-lived process leader must remain observable")
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "short-lived process churn did not settle in iteration {iteration}"
                );
                std::thread::yield_now();
            }

            process
                .owned_group_has_no_active_descendants()
                .unwrap_or_else(|error| {
                    panic!(
                        "iteration {iteration} failed the final disappearing-entry scan: {error}"
                    )
                });
            assert!(
                process
                    .wait()
                    .expect("short-lived process must be reapable")
                    .success(),
                "short-lived process churn failed in iteration {iteration}"
            );
        }
    }
}

/// Resolve a route's working directory inside the workspace, rejecting escapes.
fn resolve_cwd(root: &Path, working_directory: &str) -> Result<PathBuf> {
    let rel = Path::new(working_directory);
    let mut resolved = root.to_path_buf();
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir => {
                bail!("working_directory `{working_directory}` escapes workspace")
            }
            Component::Prefix(_) | Component::RootDir => {
                bail!("working_directory `{working_directory}` must be relative")
            }
        }
    }
    if !resolved.starts_with(root) {
        bail!("working_directory `{working_directory}` escapes workspace");
    }
    Ok(resolved)
}

// ─────────────────────────────────────────────────────────────────────────────
// Artifact collection & glob matching
// ─────────────────────────────────────────────────────────────────────────────

fn collect_artifacts(
    root: &Path,
    patterns: &[String],
    limits: &ExecutionLimits,
    budget: &ArtifactCaptureBudget<'_>,
) -> std::result::Result<Vec<Artifact>, ArtifactCaptureError> {
    budget.check()?;
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|source| {
        classify_artifact_io_error(".".to_string(), "canonicalize artifact root", source, true)
    })?;
    let paths = discover_artifact_paths(
        root,
        &canonical_root,
        patterns,
        limits.max_artifact_count,
        limits.max_artifact_scan_entries,
        budget,
    )?;
    preflight_artifact_limits(root, &canonical_root, &paths, limits, budget)?;

    let mut artifacts = Vec::with_capacity(paths.len());
    let mut aggregate_bytes = 0_u64;
    for path in paths {
        budget.check()?;
        let artifact = capture_artifact(
            root,
            &canonical_root,
            &path,
            limits.max_single_artifact_bytes,
            budget,
        )?;
        aggregate_bytes = checked_aggregate_artifact_bytes(
            aggregate_bytes,
            artifact.bytes_len,
            &path,
            limits.max_aggregate_artifact_bytes,
        )?;
        artifacts.push(artifact);
    }
    budget.check()?;
    Ok(artifacts)
}

fn discover_artifact_paths(
    root: &Path,
    canonical_root: &Path,
    patterns: &[String],
    max_artifact_count: usize,
    max_scan_entries: usize,
    budget: &ArtifactCaptureBudget<'_>,
) -> std::result::Result<Vec<String>, ArtifactCaptureError> {
    let mut directories = vec![root.to_path_buf()];
    let mut matched_patterns = vec![false; patterns.len()];
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut scanned_entries = 0_usize;

    while let Some(directory) = directories.pop() {
        budget.check()?;
        let directory_label = artifact_path_label(root, &directory);
        ensure_artifact_directory_contained(root, canonical_root, &directory, &directory_label)?;
        let entries = std::fs::read_dir(&directory).map_err(|source| {
            classify_artifact_io_error(
                directory_label.clone(),
                "read artifact directory",
                source,
                true,
            )
        })?;
        for entry in entries {
            budget.check()?;
            scanned_entries =
                scanned_entries
                    .checked_add(1)
                    .ok_or(ArtifactCaptureError::ArtifactScanLimit {
                        limit: max_scan_entries,
                        observed_at_least: usize::MAX,
                    })?;
            if scanned_entries > max_scan_entries {
                return Err(ArtifactCaptureError::ArtifactScanLimit {
                    limit: max_scan_entries,
                    observed_at_least: scanned_entries,
                });
            }
            let entry = entry.map_err(|source| ArtifactCaptureError::Unreadable {
                path: directory_label.clone(),
                operation: "read artifact directory entry from",
                source,
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|source| {
                classify_artifact_io_error(
                    artifact_path_label(root, &path),
                    "read artifact metadata for",
                    source,
                    true,
                )
            })?;
            if metadata.file_type().is_dir() {
                directories.push(path);
                continue;
            }

            let relative = path.strip_prefix(root).map_err(|_| {
                ArtifactCaptureError::ChangedDuringCapture {
                    path: path.display().to_string(),
                    detail: "discovered path escaped the artifact root".to_string(),
                }
            })?;
            let Some(relative) = relative.to_str() else {
                let lossy = normalize_artifact_separators(&relative.to_string_lossy());
                if patterns.iter().any(|pattern| glob_match(pattern, &lossy)) {
                    return Err(ArtifactCaptureError::UnsupportedPathEncoding {
                        path: relative.to_path_buf(),
                        relative_path_sha256: artifact_path_sha256(relative),
                    });
                }
                continue;
            };
            let relative = normalize_artifact_separators(relative);
            let mut matched = false;
            for (index, pattern) in patterns.iter().enumerate() {
                if glob_match(pattern, &relative) {
                    matched_patterns[index] = true;
                    matched = true;
                }
            }
            if matched && seen.insert(relative.clone()) {
                if paths.len() >= max_artifact_count {
                    return Err(ArtifactCaptureError::ArtifactCountLimit {
                        limit: max_artifact_count,
                        observed_at_least: max_artifact_count.saturating_add(1),
                    });
                }
                paths.push(relative);
            }
        }
    }

    if let Some((index, _)) = matched_patterns
        .iter()
        .enumerate()
        .find(|(_, matched)| !**matched)
    {
        return Err(ArtifactCaptureError::Missing {
            requirement: patterns[index].clone(),
        });
    }
    paths.sort();
    Ok(paths)
}

fn preflight_artifact_limits(
    root: &Path,
    canonical_root: &Path,
    paths: &[String],
    limits: &ExecutionLimits,
    budget: &ArtifactCaptureBudget<'_>,
) -> std::result::Result<(), ArtifactCaptureError> {
    let mut aggregate_bytes = 0_u64;
    for path in paths {
        budget.check()?;
        let full = root.join(path);
        let canonical = canonical_artifact_path(canonical_root, &full, path)?;
        let metadata = initial_artifact_metadata(&full, path)?;
        let canonical_metadata = initial_artifact_metadata(&canonical, path)?;
        if artifact_snapshot(&metadata, path)? != artifact_snapshot(&canonical_metadata, path)? {
            return Err(ArtifactCaptureError::ChangedDuringCapture {
                path: path.clone(),
                detail: "artifact path identity changed during limit preflight".to_string(),
            });
        }
        enforce_single_artifact_limit(path, metadata.len(), limits.max_single_artifact_bytes)?;
        aggregate_bytes = checked_aggregate_artifact_bytes(
            aggregate_bytes,
            metadata.len(),
            path,
            limits.max_aggregate_artifact_bytes,
        )?;
    }
    Ok(())
}

fn capture_artifact(
    root: &Path,
    canonical_root: &Path,
    relative: &str,
    max_single_artifact_bytes: u64,
    budget: &ArtifactCaptureBudget<'_>,
) -> std::result::Result<Artifact, ArtifactCaptureError> {
    budget.check()?;
    let full = root.join(relative);
    let canonical_before = canonical_artifact_path(canonical_root, &full, relative)?;
    let path_before = initial_artifact_metadata(&full, relative)?;
    let canonical_before_metadata = initial_artifact_metadata(&canonical_before, relative)?;
    if artifact_snapshot(&path_before, relative)?
        != artifact_snapshot(&canonical_before_metadata, relative)?
    {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: relative.to_string(),
            detail: "artifact path identity changed before open".to_string(),
        });
    }
    enforce_single_artifact_limit(relative, path_before.len(), max_single_artifact_bytes)?;

    let mut file = std::fs::File::open(&full)
        .map_err(|source| classify_artifact_io_error(relative.to_string(), "open", source, true))?;
    capture_opened_artifact(
        relative,
        &mut file,
        ArtifactPathState {
            full: &full,
            path_before: &path_before,
            canonical_root,
            canonical_before: &canonical_before,
        },
        max_single_artifact_bytes,
        budget,
    )
}

struct ArtifactPathState<'a> {
    full: &'a Path,
    path_before: &'a std::fs::Metadata,
    canonical_root: &'a Path,
    canonical_before: &'a Path,
}

fn capture_opened_artifact<R>(
    relative: &str,
    reader: &mut R,
    path_state: ArtifactPathState<'_>,
    max_single_artifact_bytes: u64,
    budget: &ArtifactCaptureBudget<'_>,
) -> std::result::Result<Artifact, ArtifactCaptureError>
where
    R: Read + ArtifactMetadata,
{
    let opened_before =
        reader
            .artifact_metadata()
            .map_err(|source| ArtifactCaptureError::Unreadable {
                path: relative.to_string(),
                operation: "read opened-file metadata for",
                source,
            })?;
    if !opened_before.file_type().is_file() {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: relative.to_string(),
            detail: "path stopped naming a regular file before it was opened".to_string(),
        });
    }
    if artifact_snapshot(path_state.path_before, relative)?
        != artifact_snapshot(&opened_before, relative)?
    {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: relative.to_string(),
            detail: "path identity changed before the file was opened".to_string(),
        });
    }

    let (content_hash, bytes_len) =
        hash_artifact_reader(reader, relative, max_single_artifact_bytes, budget)?;
    budget.check()?;
    let opened_after =
        reader
            .artifact_metadata()
            .map_err(|source| ArtifactCaptureError::Unreadable {
                path: relative.to_string(),
                operation: "re-read opened-file metadata for",
                source,
            })?;
    let path_after = std::fs::symlink_metadata(path_state.full).map_err(|source| {
        classify_artifact_io_error(relative.to_string(), "re-read metadata for", source, true)
    })?;
    if !path_after.file_type().is_file() {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: relative.to_string(),
            detail: "path stopped naming a regular file while it was hashed".to_string(),
        });
    }
    let canonical_after =
        canonical_artifact_path(path_state.canonical_root, path_state.full, relative)?;
    if canonical_after != path_state.canonical_before {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: relative.to_string(),
            detail: "artifact canonical path changed while it was hashed".to_string(),
        });
    }
    let canonical_after_metadata = initial_artifact_metadata(&canonical_after, relative)?;
    let before = artifact_snapshot(&opened_before, relative)?;
    if before != artifact_snapshot(&opened_after, relative)?
        || before != artifact_snapshot(&path_after, relative)?
        || before != artifact_snapshot(&canonical_after_metadata, relative)?
    {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: relative.to_string(),
            detail: "file identity, size, or modification time changed while hashing".to_string(),
        });
    }
    if bytes_len != opened_after.len() {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: relative.to_string(),
            detail: format!(
                "streamed {bytes_len} bytes but stable metadata reports {}",
                opened_after.len()
            ),
        });
    }
    budget.check()?;
    Ok(Artifact {
        path: relative.to_string(),
        content_hash,
        bytes_len,
    })
}

trait ArtifactMetadata {
    fn artifact_metadata(&self) -> io::Result<std::fs::Metadata>;
}

impl ArtifactMetadata for std::fs::File {
    fn artifact_metadata(&self) -> io::Result<std::fs::Metadata> {
        self.metadata()
    }
}

fn hash_artifact_reader(
    reader: &mut impl Read,
    relative: &str,
    max_single_artifact_bytes: u64,
    budget: &ArtifactCaptureBudget<'_>,
) -> std::result::Result<(String, u64), ArtifactCaptureError> {
    let mut hasher = Sha256::new();
    let mut bytes_len = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        budget.check()?;
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(ArtifactCaptureError::Unreadable {
                    path: relative.to_string(),
                    operation: "read content from",
                    source,
                })
            }
        };
        budget.check()?;
        bytes_len = bytes_len.checked_add(count as u64).ok_or_else(|| {
            ArtifactCaptureError::SingleArtifactLimit {
                path: relative.to_string(),
                limit: max_single_artifact_bytes,
                observed_at_least: u64::MAX,
            }
        })?;
        enforce_single_artifact_limit(relative, bytes_len, max_single_artifact_bytes)?;
        hasher.update(&buffer[..count]);
    }
    Ok((hex::encode(hasher.finalize()), bytes_len))
}

struct ArtifactCaptureBudget<'a> {
    deadline: Instant,
    cancel: &'a CancellationToken,
}

impl ArtifactCaptureBudget<'_> {
    fn check(&self) -> std::result::Result<(), ArtifactCaptureError> {
        if self.cancel.is_cancelled() {
            Err(ArtifactCaptureError::CaptureCancelled)
        } else if Instant::now() >= self.deadline {
            Err(ArtifactCaptureError::CaptureDeadlineExceeded)
        } else {
            Ok(())
        }
    }
}

fn ensure_artifact_directory_contained(
    root: &Path,
    canonical_root: &Path,
    directory: &Path,
    label: &str,
) -> std::result::Result<(), ArtifactCaptureError> {
    let metadata = std::fs::symlink_metadata(directory).map_err(|source| {
        classify_artifact_io_error(
            label.to_string(),
            "read artifact directory metadata for",
            source,
            true,
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(ArtifactCaptureError::ChangedDuringCapture {
            path: label.to_string(),
            detail: "artifact directory stopped naming a directory".to_string(),
        });
    }
    let canonical = std::fs::canonicalize(directory).map_err(|source| {
        classify_artifact_io_error(
            label.to_string(),
            "canonicalize artifact directory",
            source,
            true,
        )
    })?;
    if canonical != canonical_root && !canonical.starts_with(canonical_root) {
        return Err(ArtifactCaptureError::OutsideArtifactRoot {
            path: artifact_path_label(root, directory),
        });
    }
    Ok(())
}

fn canonical_artifact_path(
    canonical_root: &Path,
    full: &Path,
    relative: &str,
) -> std::result::Result<PathBuf, ArtifactCaptureError> {
    let canonical = std::fs::canonicalize(full).map_err(|source| {
        classify_artifact_io_error(relative.to_string(), "canonicalize", source, true)
    })?;
    if canonical == canonical_root || !canonical.starts_with(canonical_root) {
        return Err(ArtifactCaptureError::OutsideArtifactRoot {
            path: relative.to_string(),
        });
    }
    Ok(canonical)
}

fn artifact_path_sha256(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hex::encode(Sha256::digest(path.as_os_str().as_bytes()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = Vec::new();
        for code_unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&code_unit.to_le_bytes());
        }
        hex::encode(Sha256::digest(&bytes))
    }
    #[cfg(not(any(unix, windows)))]
    {
        hex::encode(Sha256::digest(path.to_string_lossy().as_bytes()))
    }
}

fn initial_artifact_metadata(
    full: &Path,
    relative: &str,
) -> std::result::Result<std::fs::Metadata, ArtifactCaptureError> {
    let metadata = std::fs::symlink_metadata(full).map_err(|source| {
        // Every caller reached this path through declared-output discovery, so
        // NotFound here is an observed disappearance rather than an output
        // that was never produced.
        classify_artifact_io_error(relative.to_string(), "read metadata for", source, true)
    })?;
    ensure_regular_artifact(relative, &metadata)?;
    Ok(metadata)
}

fn ensure_regular_artifact(
    relative: &str,
    metadata: &std::fs::Metadata,
) -> std::result::Result<(), ArtifactCaptureError> {
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(ArtifactCaptureError::UnsupportedFileType {
            path: relative.to_string(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ArtifactSnapshot {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

fn artifact_snapshot(
    metadata: &std::fs::Metadata,
    relative: &str,
) -> std::result::Result<ArtifactSnapshot, ArtifactCaptureError> {
    let modified = metadata
        .modified()
        .map_err(|source| ArtifactCaptureError::Unreadable {
            path: relative.to_string(),
            operation: "read modification time for",
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ArtifactSnapshot {
            len: metadata.len(),
            modified,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(ArtifactSnapshot {
            len: metadata.len(),
            modified,
        })
    }
}

fn enforce_single_artifact_limit(
    path: &str,
    observed_at_least: u64,
    limit: u64,
) -> std::result::Result<(), ArtifactCaptureError> {
    if observed_at_least > limit {
        Err(ArtifactCaptureError::SingleArtifactLimit {
            path: path.to_string(),
            limit,
            observed_at_least,
        })
    } else {
        Ok(())
    }
}

fn checked_aggregate_artifact_bytes(
    captured_before: u64,
    artifact_bytes: u64,
    path: &str,
    limit: u64,
) -> std::result::Result<u64, ArtifactCaptureError> {
    match captured_before.checked_add(artifact_bytes) {
        Some(total) if total <= limit => Ok(total),
        _ => Err(ArtifactCaptureError::AggregateArtifactLimit {
            path: path.to_string(),
            limit,
            captured_before,
            artifact_bytes,
        }),
    }
}

fn classify_artifact_io_error(
    path: String,
    operation: &'static str,
    source: io::Error,
    existed_before: bool,
) -> ArtifactCaptureError {
    if source.kind() == io::ErrorKind::NotFound {
        if existed_before {
            ArtifactCaptureError::ChangedDuringCapture {
                path,
                detail: format!("path disappeared while attempting to {operation}"),
            }
        } else {
            ArtifactCaptureError::Missing { requirement: path }
        }
    } else {
        ArtifactCaptureError::Unreadable {
            path,
            operation,
            source,
        }
    }
}

fn artifact_path_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .filter(|relative| !relative.is_empty())
        .map(normalize_artifact_separators)
        .unwrap_or_else(|| ".".to_string())
}

fn normalize_artifact_separators(relative: &str) -> String {
    #[cfg(windows)]
    {
        relative.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        relative.to_string()
    }
}

#[cfg(test)]
mod artifact_capture_tests {
    use super::*;

    fn test_budget(cancel: &CancellationToken) -> ArtifactCaptureBudget<'_> {
        ArtifactCaptureBudget {
            deadline: Instant::now() + Duration::from_secs(5),
            cancel,
        }
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected artifact read failure",
            ))
        }
    }

    #[test]
    fn genuine_empty_artifact_has_the_empty_content_digest() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("empty.bin"), []).unwrap();

        let cancel = CancellationToken::new();
        let budget = test_budget(&cancel);

        let artifacts = collect_artifacts(
            workspace.path(),
            &["empty.bin".to_string()],
            &ExecutionLimits::default(),
            &budget,
        )
        .unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].bytes_len, 0);
        assert_eq!(
            artifacts[0].content_hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn failed_artifact_read_never_constructs_an_empty_artifact() {
        let cancel = CancellationToken::new();
        let budget = test_budget(&cancel);
        let error =
            hash_artifact_reader(&mut FailingReader, "failed.bin", 1024, &budget).unwrap_err();
        match error {
            ArtifactCaptureError::Unreadable {
                path,
                operation,
                source,
            } => {
                assert_eq!(path, "failed.bin");
                assert_eq!(operation, "read content from");
                assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("failed read produced the wrong evidence state: {other}"),
        }
    }

    #[test]
    fn missing_and_oversized_artifacts_are_distinct_typed_failures() {
        let workspace = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let budget = test_budget(&cancel);
        let missing = collect_artifacts(
            workspace.path(),
            &["required.bin".to_string()],
            &ExecutionLimits::default(),
            &budget,
        )
        .unwrap_err();
        assert!(matches!(
            missing,
            ArtifactCaptureError::Missing { requirement } if requirement == "required.bin"
        ));

        std::fs::write(workspace.path().join("large.bin"), b"12345").unwrap();
        let limits = ExecutionLimits {
            max_single_artifact_bytes: 4,
            ..ExecutionLimits::default()
        };
        let oversized = collect_artifacts(
            workspace.path(),
            &["large.bin".to_string()],
            &limits,
            &budget,
        )
        .unwrap_err();
        assert!(matches!(
            oversized,
            ArtifactCaptureError::SingleArtifactLimit {
                path,
                limit: 4,
                observed_at_least: 5,
            } if path == "large.bin"
        ));
    }

    #[test]
    fn artifact_disappearance_after_discovery_is_not_reported_as_empty_or_never_produced() {
        let workspace = tempfile::tempdir().unwrap();
        let full = workspace.path().join("disappearing.bin");
        std::fs::write(&full, b"evidence").unwrap();
        let limits = ExecutionLimits::default();
        let canonical_root = std::fs::canonicalize(workspace.path()).unwrap();
        let cancel = CancellationToken::new();
        let budget = test_budget(&cancel);
        let paths = discover_artifact_paths(
            workspace.path(),
            &canonical_root,
            &["disappearing.bin".to_string()],
            limits.max_artifact_count,
            limits.max_artifact_scan_entries,
            &budget,
        )
        .unwrap();
        std::fs::remove_file(full).unwrap();

        let error =
            preflight_artifact_limits(workspace.path(), &canonical_root, &paths, &limits, &budget)
                .unwrap_err();
        assert!(matches!(
            error,
            ArtifactCaptureError::ChangedDuringCapture { path, .. }
                if path == "disappearing.bin"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn replacement_after_open_is_detected_as_changed_during_capture() {
        let workspace = tempfile::tempdir().unwrap();
        let full = workspace.path().join("changing.bin");
        std::fs::write(&full, b"old").unwrap();
        let canonical_root = std::fs::canonicalize(workspace.path()).unwrap();
        let canonical_before = std::fs::canonicalize(&full).unwrap();
        let path_before = std::fs::symlink_metadata(&full).unwrap();
        let mut opened = std::fs::File::open(&full).unwrap();
        let cancel = CancellationToken::new();
        let budget = test_budget(&cancel);

        std::fs::rename(&full, workspace.path().join("original.bin")).unwrap();
        std::fs::write(&full, b"new").unwrap();

        let error = capture_opened_artifact(
            "changing.bin",
            &mut opened,
            ArtifactPathState {
                full: &full,
                path_before: &path_before,
                canonical_root: &canonical_root,
                canonical_before: &canonical_before,
            },
            1024,
            &budget,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ArtifactCaptureError::ChangedDuringCapture { path, .. }
                if path == "changing.bin"
        ));
    }

    #[test]
    fn artifact_scan_limit_bounds_nonmatching_directory_traversal() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.tmp"), b"1").unwrap();
        std::fs::write(workspace.path().join("two.tmp"), b"2").unwrap();
        let limits = ExecutionLimits {
            max_artifact_scan_entries: 1,
            max_artifact_count: 1,
            ..ExecutionLimits::default()
        };
        let cancel = CancellationToken::new();
        let budget = test_budget(&cancel);

        let error = collect_artifacts(
            workspace.path(),
            &["required.bin".to_string()],
            &limits,
            &budget,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ArtifactCaptureError::ArtifactScanLimit {
                limit: 1,
                observed_at_least: 2,
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_artifact_path_rejects_intermediate_symlink_escape() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("secret.bin"), b"outside").unwrap();
        symlink(external.path(), workspace.path().join("escape")).unwrap();
        let canonical_root = std::fs::canonicalize(workspace.path()).unwrap();

        let error = canonical_artifact_path(
            &canonical_root,
            &workspace.path().join("escape/secret.bin"),
            "escape/secret.bin",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ArtifactCaptureError::OutsideArtifactRoot { path, .. }
                if path == "escape/secret.bin"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_backslash_filename_cannot_alias_a_nested_artifact() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a\\b"), b"backslash-file").unwrap();
        std::fs::create_dir(workspace.path().join("a")).unwrap();
        std::fs::write(workspace.path().join("a/b"), b"nested-file").unwrap();
        let cancel = CancellationToken::new();
        let budget = test_budget(&cancel);

        let backslash = collect_artifacts(
            workspace.path(),
            &["a\\b".to_string()],
            &ExecutionLimits::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(backslash[0].path, "a\\b");
        assert_eq!(
            backslash[0].content_hash,
            hex::encode(Sha256::digest(b"backslash-file"))
        );

        let nested = collect_artifacts(
            workspace.path(),
            &["a/b".to_string()],
            &ExecutionLimits::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(nested[0].path, "a/b");
        assert_eq!(
            nested[0].content_hash,
            hex::encode(Sha256::digest(b"nested-file"))
        );
    }
}

/// Match a glob pattern against a `/`-separated path.
///
/// Supports `**` (any number of path segments), `*` (any characters within a
/// segment), and `?` (a single non-`/` character).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let text: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &text)
}

fn match_segments(pat: &[&str], text: &[&str]) -> bool {
    let mut pattern_index = 0;
    let mut text_index = 0;
    let mut last_globstar = None;
    let mut globstar_text_index = 0;

    while text_index < text.len() {
        if pattern_index < pat.len()
            && pat[pattern_index] != "**"
            && segment_match(pat[pattern_index], text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pat.len() && pat[pattern_index] == "**" {
            last_globstar = Some(pattern_index);
            pattern_index += 1;
            globstar_text_index = text_index;
        } else if let Some(globstar) = last_globstar {
            globstar_text_index += 1;
            text_index = globstar_text_index;
            pattern_index = globstar + 1;
        } else {
            return false;
        }
    }

    while pattern_index < pat.len() && pat[pattern_index] == "**" {
        pattern_index += 1;
    }
    pattern_index == pat.len()
}

/// Match a single path segment against a pattern with `*` and `?`.
fn segment_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    wildcard(&p, &t)
}

fn wildcard(pat: &[char], text: &[char]) -> bool {
    let mut pattern_index = 0;
    let mut text_index = 0;
    let mut last_star = None;
    let mut star_text_index = 0;

    while text_index < text.len() {
        if pattern_index < pat.len()
            && (pat[pattern_index] == '?' || pat[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pat.len() && pat[pattern_index] == '*' {
            last_star = Some(pattern_index);
            pattern_index += 1;
            star_text_index = text_index;
        } else if let Some(star) = last_star {
            star_text_index += 1;
            text_index = star_text_index;
            pattern_index = star + 1;
        } else {
            return false;
        }
    }

    while pattern_index < pat.len() && pat[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pat.len()
}

// ─────────────────────────────────────────────────────────────────────────────
// Route-set / policy execution
// ─────────────────────────────────────────────────────────────────────────────

/// Run a target under a policy. `target` may name a route or a route set (by
/// its `provides` token); when `None`, the unambiguous default route runs.
///
/// Returns every result produced (a fallback may produce several).
pub fn run_selection(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
) -> Result<Vec<OExecutionResult>> {
    let selection = resolve_selection(bundle, target, policy_override)?;
    execute_policy(bundle, &selection.alternatives, &selection.policy, opts)
}

/// A route selection after every decision that can be made without executing
/// a command has been resolved.
///
/// The planner and hosted runtime share this exact value so `o plan` cannot
/// describe different alternatives or fallback order from `--target script`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSelection {
    /// The caller-visible route or route-set name after default resolution.
    pub target: String,
    /// Concrete route ids in execution/candidate order.
    pub alternatives: Vec<String>,
    /// The policy whose dynamic result/cancellation semantics remain to run.
    pub policy: RoutePolicy,
}

/// Resolve a route/route-set request without materializing a workspace or
/// executing any command.
pub fn resolve_selection(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
) -> Result<ResolvedSelection> {
    let (target, alternatives, policy) = match target {
        Some(name) => {
            if let Some(set) = bundle.route_set(name) {
                let policy = policy_override.unwrap_or_else(|| set.policy.clone());
                (name.to_string(), set.alternatives.clone(), policy)
            } else if bundle.route(name).is_some() {
                let policy = match policy_override {
                    Some(RoutePolicy::Explicit(id)) if id.is_empty() => {
                        RoutePolicy::Explicit(name.to_string())
                    }
                    Some(RoutePolicy::Default) | None => RoutePolicy::Explicit(name.to_string()),
                    Some(other) => other,
                };
                (name.to_string(), vec![name.to_string()], policy)
            } else {
                return Err(anyhow::anyhow!(
                    "no route or route set named `{name}`\n{}",
                    bundle.route_table()
                ));
            }
        }
        None => {
            if let Some(policy) = policy_override {
                return Err(anyhow::anyhow!(
                    "--routes-policy `{}` requires --route to name a route or route set",
                    policy.token()
                ));
            }
            match bundle.resolved_default() {
                Some(id) => (id.clone(), vec![id.clone()], RoutePolicy::Explicit(id)),
                None => {
                    return Err(anyhow::anyhow!(
                        "no unambiguous default route — select one with --route <ID>\n{}",
                        bundle.route_table()
                    ))
                }
            }
        }
    };

    if alternatives.is_empty() {
        bail!("route set `{target}` has no alternatives to run");
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in &alternatives {
        if !seen.insert(id.clone()) {
            bail!("route selection `{target}` repeats alternative `{id}`");
        }
        if bundle.route(id).is_none() {
            bail!("route selection `{target}` references missing route `{id}`");
        }
    }

    let (alternatives, policy) = match policy {
        RoutePolicy::Explicit(id) => {
            let id = if id.is_empty() {
                if alternatives.len() != 1 {
                    bail!("explicit policy needs a specific route id");
                }
                alternatives[0].clone()
            } else {
                if !alternatives.contains(&id) {
                    bail!("explicit route `{id}` is not among the alternatives");
                }
                id
            };
            (vec![id.clone()], RoutePolicy::Explicit(id))
        }
        RoutePolicy::Default => {
            let default = bundle
                .resolved_default()
                .filter(|id| alternatives.contains(id))
                .or_else(|| {
                    let defaults = alternatives
                        .iter()
                        .filter(|id| bundle.route(id).is_some_and(|route| route.is_default))
                        .cloned()
                        .collect::<Vec<_>>();
                    (defaults.len() == 1).then(|| defaults[0].clone())
                })
                .context("no unambiguous default route among alternatives")?;
            (vec![default], RoutePolicy::Default)
        }
        RoutePolicy::Fallback => {
            let mut ordered = alternatives;
            ordered.sort_by_key(|id| {
                std::cmp::Reverse(bundle.route(id).map(|route| route.priority).unwrap_or(0))
            });
            (ordered, RoutePolicy::Fallback)
        }
        other => (alternatives, other),
    };

    Ok(ResolvedSelection {
        target,
        alternatives,
        policy,
    })
}

fn potential_route_execution_count(
    bundle: &ProjectBundle,
    alternatives: &[String],
) -> Result<usize> {
    let mut total = 0_usize;
    for alternative in alternatives {
        // Each alternative owns an isolated workspace, so a shared
        // prerequisite may execute once per alternative. Within one branch,
        // RunCtx memoizes repeated prerequisites and cycles are counted once
        // here before the existing lifecycle validator reports the cycle.
        let mut pending = vec![alternative.as_str()];
        let mut seen = HashSet::new();
        while let Some(route_id) = pending.pop() {
            if !seen.insert(route_id) {
                continue;
            }
            total = total
                .checked_add(1)
                .ok_or_else(|| RouteExecutionError::Configuration {
                    detail: "potential route-execution count overflowed".to_string(),
                })?;
            if let Some(route) = bundle.route(route_id) {
                pending.extend(route.prerequisites.iter().map(String::as_str));
            }
        }
    }
    Ok(total)
}

fn execute_policy(
    bundle: &ProjectBundle,
    alternatives: &[String],
    policy: &RoutePolicy,
    opts: &RunOptions,
) -> Result<Vec<OExecutionResult>> {
    if alternatives.is_empty() {
        bail!("route set has no alternatives to run");
    }
    let potential_route_executions = potential_route_execution_count(bundle, alternatives)?;
    opts.limits
        .validate_route_execution_set(potential_route_executions)?;
    match policy {
        RoutePolicy::Explicit(id) => {
            let id = alternatives
                .first()
                .filter(|candidate| *candidate == id)
                .context("resolved explicit selection lost its route")?;
            Ok(vec![run_route(bundle, id, opts)?])
        }
        RoutePolicy::Default => {
            let default = alternatives
                .first()
                .context("resolved default selection lost its route")?;
            Ok(vec![run_route(bundle, default, opts)?])
        }
        RoutePolicy::Fallback => {
            let mut results = Vec::new();
            for id in alternatives {
                let result = run_route(bundle, id, opts)?;
                let ok = result.succeeded();
                results.push(result);
                if ok {
                    return Ok(results);
                }
            }
            Ok(results)
        }
        RoutePolicy::AnySuccess => {
            let mut results = Vec::new();
            for id in alternatives {
                let result = run_route(bundle, id, opts)?;
                let ok = result.succeeded();
                results.push(result);
                if ok {
                    return Ok(results);
                }
            }
            Ok(results)
        }
        RoutePolicy::All => {
            let mut results = Vec::new();
            for id in alternatives {
                results.push(run_route(bundle, id, opts)?);
            }
            Ok(results)
        }
        RoutePolicy::RaceSuccess => {
            race_alternatives(bundle, alternatives, opts, RaceMode::FirstSuccess)
        }
        RoutePolicy::RaceSettle => {
            race_alternatives(bundle, alternatives, opts, RaceMode::FirstSettle)
        }
        RoutePolicy::VerifyEquivalent => {
            let results = run_all_parallel(bundle, alternatives, opts)?;
            let failures: Vec<&OExecutionResult> =
                results.iter().filter(|r| !r.succeeded()).collect();
            if !failures.is_empty() {
                bail!(
                    "verify_equivalent requires every alternative to succeed; failed: {}",
                    failures
                        .iter()
                        .map(|r| format!("`{}` (exit {:?})", r.route_id, r.exit_code))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            verify_results_equivalent(&results)?;
            Ok(results)
        }
        RoutePolicy::BenchmarkAndSelect => {
            let mut results = run_all_parallel(bundle, alternatives, opts)?;
            let winner = results
                .iter()
                .enumerate()
                .filter(|(_, r)| r.succeeded())
                .min_by_key(|(index, r)| (r.duration_ns, *index))
                .map(|(index, _)| index);
            match winner {
                Some(index) => {
                    // The selected (fastest successful) result is returned
                    // last, mirroring the fallback policy where the effective
                    // result is the final element.
                    let selected = results.remove(index);
                    results.push(selected);
                    Ok(results)
                }
                None => bail!("benchmark_and_select: no alternative succeeded"),
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parallel policy machinery
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RaceMode {
    /// Cancel the race as soon as one alternative succeeds.
    FirstSuccess,
    /// Cancel the race as soon as one alternative settles at all.
    FirstSettle,
}

/// Launch every alternative on its own thread, each in its own isolated
/// workspace with its own cancellation token, and forward `(index, outcome)`
/// completions over a channel to the caller-provided selector.
fn run_alternatives_parallel<T>(
    bundle: &ProjectBundle,
    alternatives: &[String],
    opts: &RunOptions,
    select: impl FnOnce(
        &mpsc::Receiver<(usize, Result<OExecutionResult>)>,
        &[CancellationToken],
    ) -> Result<T>,
) -> Result<T> {
    let tokens: Vec<CancellationToken> = alternatives
        .iter()
        .map(|_| CancellationToken::new())
        .collect();
    let (sender, receiver) = mpsc::channel::<(usize, Result<OExecutionResult>)>();

    std::thread::scope(|scope| {
        for (index, id) in alternatives.iter().enumerate() {
            let sender = sender.clone();
            let token = tokens[index].clone();
            scope.spawn(move || {
                let outcome = run_route_cancellable(bundle, id, opts, token);
                let _ = sender.send((index, outcome));
            });
        }
        drop(sender);
        select(&receiver, &tokens)
    })
}

/// Run all alternatives concurrently to completion (no cancellation), and
/// return their results in declaration order. Real launch errors propagate
/// deterministically: the error of the earliest-declared failing alternative
/// wins.
fn run_all_parallel(
    bundle: &ProjectBundle,
    alternatives: &[String],
    opts: &RunOptions,
) -> Result<Vec<OExecutionResult>> {
    run_alternatives_parallel(bundle, alternatives, opts, |receiver, _tokens| {
        let mut slots: Vec<Option<Result<OExecutionResult>>> =
            (0..alternatives.len()).map(|_| None).collect();
        for (index, outcome) in receiver.iter() {
            slots[index] = Some(outcome);
        }
        let mut results = Vec::with_capacity(alternatives.len());
        for (index, slot) in slots.into_iter().enumerate() {
            match slot {
                Some(Ok(result)) => results.push(result),
                Some(Err(err)) => {
                    return Err(err.context(format!(
                        "alternative `{}` failed to launch",
                        alternatives[index]
                    )))
                }
                // Defensive: unreachable in practice — every scoped thread
                // sends exactly one message before the channel closes.
                None => bail!(
                    "alternative `{}` never reported a result",
                    alternatives[index]
                ),
            }
        }
        Ok(results)
    })
}

/// Race all alternatives. On the first qualifying settlement (per `mode`) the
/// remaining alternatives are cancelled cooperatively. Returns every settled
/// (non-cancelled) result in declaration order with the selected result last.
/// Selection is deterministic under ties: among qualifying settlements the
/// earliest-declared alternative wins.
fn race_alternatives(
    bundle: &ProjectBundle,
    alternatives: &[String],
    opts: &RunOptions,
    mode: RaceMode,
) -> Result<Vec<OExecutionResult>> {
    run_alternatives_parallel(bundle, alternatives, opts, |receiver, tokens| {
        let mut slots: Vec<Option<Result<OExecutionResult>>> =
            (0..alternatives.len()).map(|_| None).collect();
        let mut winner: Option<usize> = None;

        for (index, outcome) in receiver.iter() {
            let qualifies = match (&outcome, mode) {
                (Ok(result), RaceMode::FirstSuccess) => result.succeeded(),
                (Ok(_), RaceMode::FirstSettle) => true,
                (Err(err), RaceMode::FirstSettle) => !is_cancellation_error(err),
                (Err(_), RaceMode::FirstSuccess) => false,
            };
            slots[index] = Some(outcome);
            if qualifies && winner.is_none() {
                winner = Some(index);
                for (other, token) in tokens.iter().enumerate() {
                    if other != index {
                        token.cancel();
                    }
                }
            }
        }

        // Deterministic tie-break: if several qualifying settlements arrived
        // before cancellation took effect, prefer the earliest declared one.
        if winner.is_some() {
            for (index, slot) in slots.iter().enumerate() {
                let qualifies = match (slot, mode) {
                    (Some(Ok(result)), RaceMode::FirstSuccess) => result.succeeded(),
                    (Some(Ok(_)), RaceMode::FirstSettle) => true,
                    (Some(Err(err)), RaceMode::FirstSettle) => !is_cancellation_error(err),
                    _ => false,
                };
                if qualifies {
                    winner = Some(index);
                    break;
                }
            }
        }

        let Some(winner) = winner else {
            // No qualifying settlement: propagate the earliest real error, or
            // return every settled failure so the caller reports "no route
            // succeeded".
            let mut results = Vec::new();
            for (index, slot) in slots.into_iter().enumerate() {
                match slot {
                    Some(Ok(result)) => results.push(result),
                    Some(Err(err)) if !is_cancellation_error(&err) => {
                        return Err(err.context(format!(
                            "alternative `{}` failed to launch",
                            alternatives[index]
                        )))
                    }
                    _ => {}
                }
            }
            if results.is_empty() {
                // Defensive: unreachable in practice — cancellation only
                // starts after a qualifying settlement, so at least one
                // alternative settles with a result or a real error above.
                bail!("race: no alternative settled");
            }
            return Ok(results);
        };

        let mut results = Vec::new();
        let mut selected = None;
        for (index, slot) in slots.into_iter().enumerate() {
            match slot {
                Some(Ok(result)) if index == winner => selected = Some(result),
                Some(Ok(result)) => results.push(result),
                Some(Err(err)) if index == winner => {
                    // FirstSettle winner settled with a launch error.
                    return Err(err.context(format!(
                        "race: selected alternative `{}` settled with an error",
                        alternatives[index]
                    )));
                }
                _ => {}
            }
        }
        match selected {
            Some(result) => {
                results.push(result);
                Ok(results)
            }
            None => bail!(
                "race: selected alternative `{}` produced no result",
                alternatives[winner]
            ),
        }
    })
}

/// The equivalence contract for `verify_equivalent`: when every result carries
/// a decoded JSON value, values must be equal; otherwise trimmed stdout text
/// must match across all alternatives.
fn verify_results_equivalent(results: &[OExecutionResult]) -> Result<()> {
    if results.len() < 2 {
        return Ok(());
    }
    if results.iter().any(|result| result.stdout_capture.truncated) {
        let reference = &results[0];
        for other in &results[1..] {
            if other.stdout_capture.sha256 != reference.stdout_capture.sha256
                || other.stdout_capture.total_observed_bytes
                    != reference.stdout_capture.total_observed_bytes
            {
                bail!(
                    "verify_equivalent: route `{}` and route `{}` produced different stdout",
                    reference.route_id,
                    other.route_id
                );
            }
        }
        return Ok(());
    }
    let all_json = results.iter().all(|r| r.value.is_some());
    if all_json {
        let reference = &results[0];
        for other in &results[1..] {
            if other.value != reference.value {
                bail!(
                    "verify_equivalent: route `{}` and route `{}` produced different JSON values",
                    reference.route_id,
                    other.route_id
                );
            }
        }
        return Ok(());
    }
    let reference = &results[0];
    let reference_text = reference.stdout_text();
    let reference_out = reference_text.trim_end();
    for other in &results[1..] {
        let other_text = other.stdout_text();
        if other_text.trim_end() != reference_out {
            bail!(
                "verify_equivalent: route `{}` and route `{}` produced different stdout",
                reference.route_id,
                other.route_id
            );
        }
    }
    Ok(())
}
