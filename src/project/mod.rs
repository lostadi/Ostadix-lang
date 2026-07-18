//! First-class project, route, and bundle model for O-lang.
//!
//! This module gives O-lang a lossless, route-preserving representation of an
//! entire codebase:
//!
//!   * [`model`] — the core serde vocabulary (bundles, files, routes, sets,
//!     policies, guards, results).
//!   * [`bundle`] — lossless directory bundling and (de)serialization.
//!   * [`materialize`] — safe workspace materialization on disk.
//!   * [`manifest`] — `olang.project.toml` parsing and CLI route overrides.
//!   * [`discover`] + [`ecosystems`] — automatic ecosystem route discovery.
//!   * [`runtime`] — native route execution with prerequisites and policies.
//!   * [`lower`] — lifting a project into a single valid `.O` document.

use anyhow::Result;
use std::path::{Path, PathBuf};

pub mod bundle;
pub mod discover;
pub mod ecosystems;
pub mod lower;
pub mod manifest;
pub mod materialize;
pub mod model;
pub mod runtime;

pub use model::{
    Artifact, ExecutionProvenance, FileRole, OExecutionResult, ProjectBundle, ProjectFile,
    ResultCodec, RouteEffects, RouteGuard, RouteKind, RoutePolicy, RouteProvenance, RouteSet,
    RouteSpec,
};

/// Derive a project name from a directory path.
pub fn name_from_path(root: &Path) -> String {
    root.canonicalize()
        .ok()
        .as_deref()
        .and_then(|p| p.file_name())
        .or_else(|| root.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty() && s != ".")
        .unwrap_or_else(|| "project".to_string())
}

/// Assemble a complete [`ProjectBundle`] from a directory: bundle the files,
/// discover routes, apply the manifest, then apply CLI route overrides.
///
/// The precedence is: CLI overrides > manifest > discovery.
pub fn assemble(root: &Path, name: &str, route_decls: &[String]) -> Result<ProjectBundle> {
    assemble_excluding(root, name, route_decls, &[])
}

/// Assemble a complete [`ProjectBundle`] while excluding exact filesystem
/// paths from the captured file set.
///
/// This is primarily used by `o-link` when its output path is inside the
/// project root: an existing non-generated output must not be captured and
/// then overwritten as part of the new bundle. Relative exclusions are
/// resolved from the caller's current working directory.
pub fn assemble_excluding(
    root: &Path,
    name: &str,
    route_decls: &[String],
    exclusions: &[PathBuf],
) -> Result<ProjectBundle> {
    let mut bundle = bundle::bundle_dir_excluding(root, name, exclusions)?;
    discover::apply_discovery(&mut bundle, root);
    manifest::load_and_apply(&mut bundle, root)?;
    manifest::apply_cli_overrides(&mut bundle, route_decls)?;
    finalize_default(&mut bundle);
    Ok(bundle)
}

/// If no default route is set yet but exactly one credible run route exists,
/// adopt it as the default.
pub fn finalize_default(bundle: &mut ProjectBundle) {
    if bundle.default_route.is_some() {
        // Keep the manifest/CLI choice, but reflect it on the route flags.
        if let Some(id) = bundle.default_route.clone() {
            for route in &mut bundle.routes {
                route.is_default = route.id == id;
            }
        }
        return;
    }
    let run_candidates: Vec<String> = bundle
        .routes
        .iter()
        .filter(|r| discover::is_run_candidate(r))
        .map(|r| r.id.clone())
        .collect();
    if run_candidates.len() == 1 {
        let id = run_candidates.into_iter().next().unwrap();
        for route in &mut bundle.routes {
            route.is_default = route.id == id;
        }
        bundle.default_route = Some(id);
    }
}
