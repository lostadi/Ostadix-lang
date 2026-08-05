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
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::executor::CancellationToken;

use super::bundle::sha256_hex;
use super::materialize::{materialize_isolated, Workspace};
use super::model::{
    Artifact, ExecutionProvenance, OExecutionResult, ProjectBundle, ResultCodec,
    RouteExecutionDisposition, RouteGuard, RoutePolicy, RouteSpec,
};

/// How unmet guards are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardBehavior {
    /// Fail if a guard is not satisfied.
    Enforce,
    /// Skip the route (return a synthetic no-op result) if a guard fails.
    Skip,
}

/// Options controlling route execution.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// How to treat unmet guards.
    pub guard_behavior: GuardBehavior,
    /// Whether to inherit the parent process environment.
    pub inherit_env: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            guard_behavior: GuardBehavior::Enforce,
            inherit_env: true,
        }
    }
}

/// Marker embedded in the stderr of a guard-skipped result.
const SKIP_MARKER: &str = "[olang:route-skipped]";

// ─────────────────────────────────────────────────────────────────────────────
// Guards
// ─────────────────────────────────────────────────────────────────────────────

/// Return the first unmet guard's description, if any.
pub fn unmet_guard(route: &RouteSpec) -> Option<String> {
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
                if !command_on_path(cmd) {
                    return Some(format!("requires command `{cmd}` on PATH"));
                }
            }
            RouteGuard::EnvVarSet(var) => {
                if std::env::var(var).map(|v| v.is_empty()).unwrap_or(true) {
                    return Some(format!("requires environment variable `{var}` to be set"));
                }
            }
        }
    }
    None
}

/// Resolve a command name against `PATH` (or accept an explicit path).
fn command_on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).exists();
    }
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(cmd);
        candidate.is_file() && is_executable(&candidate)
    })
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
    if bundle.route(route_id).is_none() {
        bail!("no route named `{route_id}`");
    }
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
    if let Some(reason) = unmet_guard(route) {
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
    OExecutionResult {
        route_id: route.id.clone(),
        exit_code: None,
        stdout: Vec::new(),
        stderr: format!("{SKIP_MARKER} {reason}\n").into_bytes(),
        value: None,
        artifacts: Vec::new(),
        disposition: RouteExecutionDisposition::GuardSkipped,
        duration_ns: 0,
        provenance: ExecutionProvenance {
            workspace: workspace.root.clone(),
            command: route.full_command(),
            cwd: workspace.root.clone(),
        },
    }
}

/// Marker message used in the error of a cancellation-terminated route.
const CANCEL_MARKER: &str = "[olang:route-canceled]";

/// Whether an error came from cooperative route cancellation.
pub fn is_cancellation_error(err: &anyhow::Error) -> bool {
    err.to_string().contains(CANCEL_MARKER)
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
        bail!(
            "{CANCEL_MARKER} route `{}` canceled before launch",
            route.id
        );
    }

    let cwd = resolve_cwd(&workspace.root, &route.working_directory)?;

    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    cmd.current_dir(&cwd);
    if !opts.inherit_env {
        cmd.env_clear();
    }
    for (key, value) in &route.environment {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // Place the route in its own process group so cancellation can terminate
    // the whole tree (e.g. a shell plus its children), not just the leader.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `{}`", command.join(" ")))?;

    // Drain pipes on helper threads so the child never blocks on a full pipe
    // while the coordinator polls for exit or cancellation.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || drain_pipe(stdout_pipe));
    let stderr_handle = std::thread::spawn(move || drain_pipe(stderr_pipe));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if cancel.is_cancelled() {
                    kill_route_process(&mut child);
                    let _ = child.wait();
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    bail!("{CANCEL_MARKER} route `{}` canceled", route.id);
                }
                // Short poll keeps cancellation latency low; the sleeping
                // coordinator thread costs no meaningful CPU between polls.
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(err) => {
                kill_route_process(&mut child);
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(err)
                    .with_context(|| format!("failed waiting on `{}`", command.join(" ")));
            }
        }
    };
    let duration_ns = start.elapsed().as_nanos();

    // A drain-thread panic (only possible via a std I/O bug) degrades to
    // empty captured output rather than poisoning the route result.
    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    let value = match route.result_codec {
        ResultCodec::Json => serde_json::from_slice::<serde_json::Value>(&stdout).ok(),
        _ => None,
    };

    let artifacts = collect_artifacts(&workspace.root, &route.outputs)?;

    Ok(OExecutionResult {
        route_id: route.id.clone(),
        exit_code: status.code(),
        stdout,
        stderr,
        value,
        artifacts,
        disposition: RouteExecutionDisposition::Executed,
        duration_ns,
        provenance: ExecutionProvenance {
            workspace: workspace.root.clone(),
            command,
            cwd,
        },
    })
}

/// Read a child pipe to completion, returning the captured bytes.
fn drain_pipe(pipe: Option<impl Read>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    bytes
}

/// Terminate a route's process tree. On unix the route runs in its own
/// process group, so the whole group is signalled; elsewhere only the group
/// leader can be killed.
fn kill_route_process(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pgid = child.id() as i32;
        // SAFETY: `kill` is a plain syscall with no memory-safety
        // preconditions. The negative pid targets the whole process group,
        // whose pgid equals the leader's pid because the route was spawned
        // with `process_group(0)`, and the pid is valid: `child` has not been
        // reaped yet (no `wait` has returned for it).
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
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

fn collect_artifacts(root: &Path, patterns: &[String]) -> Result<Vec<Artifact>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let mut all_files = Vec::new();
    collect_files(root, root, &mut all_files);
    all_files.sort();

    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    for pattern in patterns {
        for rel in &all_files {
            if glob_match(pattern, rel) && seen.insert(rel.clone()) {
                let full = root.join(rel);
                let bytes = std::fs::read(&full).unwrap_or_default();
                artifacts.push(Artifact {
                    path: rel.clone(),
                    content_hash: sha256_hex(&bytes),
                    bytes_len: bytes.len() as u64,
                });
            }
        }
    }
    Ok(artifacts)
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_dir() {
            collect_files(root, &path, out);
        } else if meta.file_type().is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
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
    match pat.split_first() {
        None => text.is_empty(),
        Some((&"**", rest)) => {
            // `**` matches zero or more segments.
            for i in 0..=text.len() {
                if match_segments(rest, &text[i..]) {
                    return true;
                }
            }
            false
        }
        Some((seg, rest)) => {
            let Some((first, text_rest)) = text.split_first() else {
                return false;
            };
            if segment_match(seg, first) {
                match_segments(rest, text_rest)
            } else {
                false
            }
        }
    }
}

/// Match a single path segment against a pattern with `*` and `?`.
fn segment_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    wildcard(&p, &t)
}

fn wildcard(pat: &[char], text: &[char]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => {
            for i in 0..=text.len() {
                if wildcard(rest, &text[i..]) {
                    return true;
                }
            }
            false
        }
        Some(('?', rest)) => {
            if text.is_empty() {
                false
            } else {
                wildcard(rest, &text[1..])
            }
        }
        Some((c, rest)) => {
            if text.first() == Some(c) {
                wildcard(rest, &text[1..])
            } else {
                false
            }
        }
    }
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

fn execute_policy(
    bundle: &ProjectBundle,
    alternatives: &[String],
    policy: &RoutePolicy,
    opts: &RunOptions,
) -> Result<Vec<OExecutionResult>> {
    if alternatives.is_empty() {
        bail!("route set has no alternatives to run");
    }
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
