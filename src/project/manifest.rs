//! `olang.project.toml` parsing and CLI route overrides.
//!
//! Manifest routes and route sets *override* anything discovered automatically:
//! a manifest route replaces a discovered route with the same id, and CLI
//! `--route-decl` declarations override both.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::model::{
    ProjectBundle, ResultCodec, RouteEffects, RouteFailureContinuation, RouteGuard, RouteKind,
    RoutePolicy, RouteProvenance, RouteSet, RouteSpec,
};

/// The canonical manifest filename at a project root.
pub const MANIFEST_FILENAME: &str = "olang.project.toml";

// ─────────────────────────────────────────────────────────────────────────────
// TOML shape (tolerant of unknown extra fields)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ManifestRoot {
    project: Option<ProjectSection>,
    #[serde(default)]
    routes: Vec<ManifestRoute>,
    #[serde(default)]
    route_sets: Vec<ManifestRouteSet>,
}

#[derive(Debug, Deserialize)]
struct ProjectSection {
    name: Option<String>,
    default_route: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestRoute {
    id: String,
    label: Option<String>,
    #[serde(default)]
    command: Vec<String>,
    kind: Option<String>,
    evaluator: Option<String>,
    entrypoint: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    provides: Vec<String>,
    result_codec: Option<String>,
    priority: Option<i32>,
    default: Option<bool>,
    guards: Option<ManifestGuards>,
    pure: Option<bool>,
    failure_continuation: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestGuards {
    os: Option<String>,
    requires_command: Option<String>,
    requires_env: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestRouteSet {
    provides: String,
    alternatives: Vec<String>,
    policy: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Kind parsing
// ─────────────────────────────────────────────────────────────────────────────

fn parse_kind(token: Option<&str>, has_evaluator: bool) -> RouteKind {
    if let Some(token) = token {
        match token.trim().to_ascii_lowercase().as_str() {
            "interpreter" | "interpretercommand" | "interpreter_command" => {
                RouteKind::InterpreterCommand
            }
            "binary" | "compiled" | "compiledbinary" | "compiled_binary" => {
                RouteKind::CompiledBinary
            }
            "build" | "buildtarget" | "build_target" => RouteKind::BuildTarget,
            "package" | "packageentrypoint" | "package_entrypoint" => RouteKind::PackageEntrypoint,
            "shell" | "shelltask" | "shell_task" | "task" => RouteKind::ShellTask,
            "evaluator" | "oevaluator" | "o" => RouteKind::OEvaluator,
            "composite" => RouteKind::Composite,
            _ => RouteKind::ShellTask,
        }
    } else if has_evaluator {
        RouteKind::OEvaluator
    } else {
        RouteKind::InterpreterCommand
    }
}

fn guards_from_manifest(guards: &ManifestGuards) -> Vec<RouteGuard> {
    let mut out = Vec::new();
    if let Some(os) = &guards.os {
        out.push(RouteGuard::PlatformOs(os.clone()));
    }
    if let Some(cmd) = &guards.requires_command {
        out.push(RouteGuard::CommandAvailable(cmd.clone()));
    }
    if let Some(env) = &guards.requires_env {
        out.push(RouteGuard::EnvVarSet(env.clone()));
    }
    out
}

fn route_from_manifest(route: ManifestRoute, manifest_path: &str) -> Result<RouteSpec> {
    let has_evaluator = route.evaluator.is_some();
    let mut spec = RouteSpec::new(
        route.id.clone(),
        RouteProvenance::Manifest {
            path: manifest_path.to_string(),
        },
    );
    spec.label = route.label.unwrap_or(route.id);
    spec.kind = parse_kind(route.kind.as_deref(), has_evaluator);
    spec.command = route.command;
    spec.evaluator = route.evaluator;
    spec.entrypoint = route.entrypoint;
    spec.working_directory = route.cwd.unwrap_or_else(|| ".".to_string());
    spec.arguments = route.args;
    spec.environment = route.env;
    spec.prerequisites = route.depends_on;
    spec.inputs = route.inputs;
    spec.outputs = route.outputs;
    spec.provides = route.provides;
    spec.result_codec = route
        .result_codec
        .as_deref()
        .map(ResultCodec::parse)
        .unwrap_or(ResultCodec::Text);
    spec.priority = route.priority.unwrap_or(0);
    spec.is_default = route.default.unwrap_or(false);
    if let Some(guards) = &route.guards {
        spec.guards = guards_from_manifest(guards);
    }
    if let Some(is_pure) = route.pure {
        spec.effects = if is_pure {
            RouteEffects {
                pure: true,
                unknown: false,
                reads: Vec::new(),
                writes: Vec::new(),
            }
        } else {
            RouteEffects::unknown()
        };
    }
    if let Some(continuation) = route.failure_continuation {
        spec.failure_continuation = RouteFailureContinuation::parse_checked(&continuation)
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("route `{}` has an invalid failure_continuation", spec.id))?;
    }
    Ok(spec)
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Parse and apply a manifest string onto `bundle`, replacing discovered
/// routes that share an id.
pub fn apply_manifest(bundle: &mut ProjectBundle, text: &str, manifest_path: &str) -> Result<()> {
    let root: ManifestRoot =
        toml::from_str(text).context("failed to parse olang.project.toml manifest")?;

    if let Some(project) = root.project {
        if let Some(name) = project.name {
            bundle.name = name;
        }
        if let Some(default_route) = project.default_route {
            bundle.default_route = Some(default_route);
        }
    }

    for manifest_route in root.routes {
        let spec = route_from_manifest(manifest_route, manifest_path)?;
        upsert_route(bundle, spec);
    }

    for set in root.route_sets {
        let policy = set
            .policy
            .as_deref()
            .map(RoutePolicy::parse)
            .unwrap_or(RoutePolicy::Default);
        let route_set = RouteSet {
            provides: set.provides.clone(),
            alternatives: set.alternatives,
            policy,
        };
        upsert_route_set(bundle, route_set);
    }

    Ok(())
}

/// Load `olang.project.toml` from the project root (if present) and apply it.
pub fn load_and_apply(bundle: &mut ProjectBundle, root: &Path) -> Result<bool> {
    let path = root.join(MANIFEST_FILENAME);
    if !path.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    apply_manifest(bundle, &text, MANIFEST_FILENAME)?;
    Ok(true)
}

/// Replace an existing route with the same id, or append.
fn upsert_route(bundle: &mut ProjectBundle, spec: RouteSpec) {
    if let Some(existing) = bundle.routes.iter_mut().find(|r| r.id == spec.id) {
        *existing = spec;
    } else {
        bundle.routes.push(spec);
    }
}

fn upsert_route_set(bundle: &mut ProjectBundle, set: RouteSet) {
    if let Some(existing) = bundle
        .route_sets
        .iter_mut()
        .find(|s| s.provides == set.provides)
    {
        *existing = set;
    } else {
        bundle.route_sets.push(set);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CLI route overrides
// ─────────────────────────────────────────────────────────────────────────────

/// Apply a list of `--route-decl` declarations, overriding any route with the
/// same id.
///
/// ## Micro-syntax
///
/// A declaration is a `;`-separated list of `key=value` fields. The `cmd` value
/// is whitespace-split into the command vector. Recognised keys:
///
/// | key        | meaning                                             |
/// |------------|-----------------------------------------------------|
/// | `id`       | route id (**required**)                             |
/// | `cmd`      | command, whitespace-split (first token = program)   |
/// | `label`    | human label                                         |
/// | `cwd`      | working directory (default `.`)                     |
/// | `args`     | comma-separated extra arguments                     |
/// | `env`      | comma-separated `K=V` pairs                          |
/// | `provides` | comma-separated capability tokens                   |
/// | `depends`  | comma-separated prerequisite route ids              |
/// | `outputs`  | comma-separated output globs                        |
/// | `codec`    | `text` \| `json` \| `bytes`                         |
/// | `kind`     | route kind token (interpreter, binary, shell, …)    |
/// | `priority` | integer selection priority                          |
/// | `default`  | `true` \| `false`                                   |
/// | `failure_continuation` | `unproven` \| `declared_idempotent`      |
///
/// Example: `id=main-a;cmd=python3 implementation_a.py;cwd=.;provides=main;codec=json`
pub fn apply_cli_overrides(bundle: &mut ProjectBundle, route_decls: &[String]) -> Result<()> {
    for decl in route_decls {
        let spec = parse_route_decl(decl)?;
        upsert_route(bundle, spec);
    }
    Ok(())
}

/// Parse a single `--route-decl` string into a [`RouteSpec`].
pub fn parse_route_decl(decl: &str) -> Result<RouteSpec> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for field in decl.split(';') {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        let (key, value) = field
            .split_once('=')
            .with_context(|| format!("route declaration field `{field}` is not key=value"))?;
        fields.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let id = fields
        .get("id")
        .filter(|s| !s.is_empty())
        .cloned()
        .context("route declaration requires an `id=` field")?;

    let mut spec = RouteSpec::new(id.clone(), RouteProvenance::CliOverride);

    if let Some(label) = fields.get("label") {
        spec.label = label.clone();
    }
    if let Some(cmd) = fields.get("cmd") {
        spec.command = cmd.split_whitespace().map(|s| s.to_string()).collect();
    }
    if let Some(cwd) = fields.get("cwd") {
        spec.working_directory = cwd.clone();
    }
    if let Some(args) = fields.get("args") {
        spec.arguments = split_list(args);
    }
    if let Some(env) = fields.get("env") {
        for pair in env.split(',') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let (k, v) = pair
                .split_once('=')
                .with_context(|| format!("env entry `{pair}` is not K=V"))?;
            spec.environment
                .insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    if let Some(provides) = fields.get("provides") {
        spec.provides = split_list(provides);
    }
    if let Some(depends) = fields.get("depends") {
        spec.prerequisites = split_list(depends);
    }
    if let Some(outputs) = fields.get("outputs") {
        spec.outputs = split_list(outputs);
    }
    if let Some(codec) = fields.get("codec") {
        spec.result_codec = ResultCodec::parse(codec);
    }
    spec.kind = parse_kind(
        fields.get("kind").map(|s| s.as_str()),
        spec.evaluator.is_some(),
    );
    if let Some(priority) = fields.get("priority") {
        spec.priority = priority
            .parse()
            .with_context(|| format!("priority `{priority}` is not an integer"))?;
    }
    if let Some(default) = fields.get("default") {
        spec.is_default = matches!(default.to_ascii_lowercase().as_str(), "true" | "1" | "yes");
    }
    if let Some(continuation) = fields.get("failure_continuation") {
        spec.failure_continuation = RouteFailureContinuation::parse_checked(continuation)
            .map_err(anyhow::Error::msg)
            .with_context(|| {
                format!("route declaration `{id}` has an invalid failure_continuation")
            })?;
    }

    if spec.command.is_empty() && spec.evaluator.is_none() {
        bail!("route declaration `{id}` has no `cmd=` and no evaluator");
    }
    Ok(spec)
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
