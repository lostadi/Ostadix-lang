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
//! an unambiguous default route or an explicit selection.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use super::bundle::sha256_hex;
use super::materialize::{materialize_isolated, Workspace};
use super::model::{
    Artifact, ExecutionProvenance, OExecutionResult, ProjectBundle, ResultCodec, RouteGuard,
    RoutePolicy, RouteSpec,
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
    if bundle.route(route_id).is_none() {
        bail!("no route named `{route_id}`");
    }
    let workspace = materialize_isolated(bundle)
        .context("failed to materialize an isolated workspace")?;
    let mut ctx = RunCtx {
        bundle,
        opts,
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

    // ── Guards ──────────────────────────────────────────────────────────────
    if let Some(reason) = unmet_guard(&route) {
        ctx.stack.pop();
        match ctx.opts.guard_behavior {
            GuardBehavior::Enforce => {
                bail!("route `{route_id}` guard not satisfied: {reason}")
            }
            GuardBehavior::Skip => {
                let result = skipped_result(&route, workspace, &reason);
                ctx.skipped.insert(route_id.to_string());
                ctx.done.insert(route_id.to_string(), result.clone());
                return Ok(result);
            }
        }
    }

    // ── Execute ─────────────────────────────────────────────────────────────
    let result = spawn_route(&route, workspace, ctx.opts)?;
    ctx.stack.pop();
    ctx.done.insert(route_id.to_string(), result.clone());
    Ok(result)
}

fn skipped_result(route: &RouteSpec, workspace: &Workspace, reason: &str) -> OExecutionResult {
    OExecutionResult {
        route_id: route.id.clone(),
        exit_code: None,
        stdout: Vec::new(),
        stderr: format!("{SKIP_MARKER} {reason}\n").into_bytes(),
        value: None,
        artifacts: Vec::new(),
        duration_ns: 0,
        provenance: ExecutionProvenance {
            workspace: workspace.root.clone(),
            command: route.full_command(),
            cwd: workspace.root.clone(),
        },
    }
}

fn spawn_route(
    route: &RouteSpec,
    workspace: &Workspace,
    opts: &RunOptions,
) -> Result<OExecutionResult> {
    let command = route.full_command();
    if command.is_empty() {
        bail!("route `{}` has an empty command", route.id);
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

    let start = Instant::now();
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn `{}`", command.join(" ")))?;
    let duration_ns = start.elapsed().as_nanos();

    let value = match route.result_codec {
        ResultCodec::Json => serde_json::from_slice::<serde_json::Value>(&output.stdout).ok(),
        _ => None,
    };

    let artifacts = collect_artifacts(&workspace.root, &route.outputs)?;

    Ok(OExecutionResult {
        route_id: route.id.clone(),
        exit_code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
        value,
        artifacts,
        duration_ns,
        provenance: ExecutionProvenance {
            workspace: workspace.root.clone(),
            command,
            cwd,
        },
    })
}

/// Resolve a route's working directory inside the workspace, rejecting escapes.
fn resolve_cwd(root: &Path, working_directory: &str) -> Result<PathBuf> {
    let rel = Path::new(working_directory);
    let mut resolved = root.to_path_buf();
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir => bail!("working_directory `{working_directory}` escapes workspace"),
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
    let (alternatives, policy) = resolve_selection(bundle, target, policy_override)?;
    execute_policy(bundle, &alternatives, &policy, opts)
}

fn resolve_selection(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
) -> Result<(Vec<String>, RoutePolicy)> {
    match target {
        Some(name) => {
            if let Some(set) = bundle.route_set(name) {
                let policy = policy_override.unwrap_or_else(|| set.policy.clone());
                Ok((set.alternatives.clone(), policy))
            } else if bundle.route(name).is_some() {
                let policy = match policy_override {
                    Some(RoutePolicy::Explicit(id)) if id.is_empty() => {
                        RoutePolicy::Explicit(name.to_string())
                    }
                    Some(RoutePolicy::Default) | None => RoutePolicy::Explicit(name.to_string()),
                    Some(other) => other,
                };
                Ok((vec![name.to_string()], policy))
            } else {
                bail!("no route or route set named `{name}`\n{}", bundle.route_table())
            }
        }
        None => {
            if let Some(policy) = policy_override {
                bail!(
                    "--routes-policy `{}` requires --route to name a route or route set",
                    policy.token()
                );
            }
            match bundle.resolved_default() {
                Some(id) => Ok((vec![id.clone()], RoutePolicy::Explicit(id))),
                None => bail!(
                    "no unambiguous default route — select one with --route <ID>\n{}",
                    bundle.route_table()
                ),
            }
        }
    }
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
            let id = if id.is_empty() {
                if alternatives.len() != 1 {
                    bail!("explicit policy needs a specific route id");
                }
                &alternatives[0]
            } else {
                if !alternatives.contains(id) {
                    bail!("explicit route `{id}` is not among the alternatives");
                }
                id
            };
            Ok(vec![run_route(bundle, id, opts)?])
        }
        RoutePolicy::Default => {
            let default = bundle
                .resolved_default()
                .filter(|d| alternatives.contains(d))
                .or_else(|| {
                    let defaults: Vec<&String> = alternatives
                        .iter()
                        .filter(|id| bundle.route(id).map(|r| r.is_default).unwrap_or(false))
                        .collect();
                    if defaults.len() == 1 {
                        Some(defaults[0].clone())
                    } else {
                        None
                    }
                });
            match default {
                Some(id) => Ok(vec![run_route(bundle, &id, opts)?]),
                None => bail!("no unambiguous default route among alternatives"),
            }
        }
        RoutePolicy::Fallback => {
            let mut ordered: Vec<String> = alternatives.to_vec();
            ordered.sort_by_key(|id| {
                std::cmp::Reverse(bundle.route(id).map(|r| r.priority).unwrap_or(0))
            });
            let mut results = Vec::new();
            for id in ordered {
                let result = run_route(bundle, &id, opts)?;
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
        other => bail!(
            "route policy `{}` is represented but not yet executable",
            other.token()
        ),
    }
}
