//! Ecosystem route discovery.
//!
//! Discovery is best-effort and produces *candidate* routes. The manifest and
//! CLI overrides always win over anything found here. Discovery never marks
//! every entrypoint for execution: when there is exactly one credible run
//! route it may become the default; when there are several, all are preserved
//! but none is defaulted, forcing an explicit selection.

use std::path::Path;

use super::ecosystems;
use super::model::{ProjectBundle, ProjectFile, RouteKind, RouteSpec};

/// A pluggable per-ecosystem route probe.
pub trait EcosystemDiscoverer {
    /// The ecosystem name, recorded in route provenance.
    fn name(&self) -> &str;
    /// Inspect the project and return candidate routes.
    fn discover(&self, root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec>;
}

/// The built-in set of ecosystem discoverers, in priority order.
pub fn discoverers() -> Vec<Box<dyn EcosystemDiscoverer>> {
    vec![
        Box::new(ecosystems::rust::RustDiscoverer),
        Box::new(ecosystems::python::PythonDiscoverer),
        Box::new(ecosystems::javascript::JavaScriptDiscoverer),
        Box::new(ecosystems::shell::ShellDiscoverer),
        Box::new(ecosystems::c_family::CFamilyDiscoverer),
        Box::new(ecosystems::java::JavaDiscoverer),
        Box::new(ecosystems::dotnet::DotnetDiscoverer),
        Box::new(ecosystems::haskell_ocaml::HaskellOcamlDiscoverer),
        Box::new(ecosystems::nix::NixDiscoverer),
        Box::new(ecosystems::generic::GenericDiscoverer),
    ]
}

/// Run every discoverer and return the merged candidate list (first occurrence
/// of any route id wins, so higher-priority ecosystems take precedence).
pub fn discover_all(root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
    let mut merged: Vec<RouteSpec> = Vec::new();
    for discoverer in discoverers() {
        for route in discoverer.discover(root, files) {
            if !merged.iter().any(|r| r.id == route.id) {
                merged.push(route);
            }
        }
    }
    merged
}

/// A route is a *run candidate* — a plausible "how do I run this project"
/// answer — when it executes a program rather than merely building or testing.
pub fn is_run_candidate(route: &RouteSpec) -> bool {
    matches!(
        route.kind,
        RouteKind::InterpreterCommand
            | RouteKind::CompiledBinary
            | RouteKind::PackageEntrypoint
            | RouteKind::OEvaluator
    )
}

/// Discover routes for `bundle` and merge them in, then set a tentative
/// default when there is exactly one credible run route.
pub fn apply_discovery(bundle: &mut ProjectBundle, root: &Path) {
    let discovered = discover_all(root, &bundle.files);
    for route in discovered {
        if !bundle.routes.iter().any(|r| r.id == route.id) {
            bundle.routes.push(route);
        }
    }

    let run_candidates: Vec<String> = bundle
        .routes
        .iter()
        .filter(|r| is_run_candidate(r))
        .map(|r| r.id.clone())
        .collect();

    if run_candidates.len() == 1 {
        let id = &run_candidates[0];
        if let Some(route) = bundle.routes.iter_mut().find(|r| &r.id == id) {
            route.is_default = true;
        }
        if bundle.default_route.is_none() {
            bundle.default_route = Some(id.clone());
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers for ecosystem probes
// ─────────────────────────────────────────────────────────────────────────────

/// Find a captured file by its exact relative path.
pub fn find_file<'a>(files: &'a [ProjectFile], path: &str) -> Option<&'a ProjectFile> {
    files.iter().find(|f| f.path == path)
}

/// True when a file with the exact relative path exists.
pub fn has_file(files: &[ProjectFile], path: &str) -> bool {
    find_file(files, path).is_some()
}

/// The UTF-8 text of a captured file, if it decodes.
pub fn file_text(file: &ProjectFile) -> Option<&str> {
    std::str::from_utf8(&file.bytes).ok()
}

/// Sanitize an arbitrary string into a route-id-safe token.
pub fn slug(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "x".to_string()
    } else {
        out
    }
}
