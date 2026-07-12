//! Nix route discovery (thin, best-effort).
//!
//! `flake.nix` → `nix run`; `default.nix` → `nix-build`.

use std::path::Path;

use crate::project::discover::{has_file, EcosystemDiscoverer};
use crate::project::model::{ProjectFile, RouteGuard, RouteKind, RouteProvenance, RouteSpec};

pub struct NixDiscoverer;

impl EcosystemDiscoverer for NixDiscoverer {
    fn name(&self) -> &str {
        "nix"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let mut routes = Vec::new();

        if has_file(files, "flake.nix") {
            let mut route = RouteSpec::new(
                "nix-run".to_string(),
                RouteProvenance::Discovered {
                    ecosystem: "nix".to_string(),
                    evidence: "flake.nix present".to_string(),
                },
            );
            route.kind = RouteKind::CompiledBinary;
            route.command = vec!["nix".to_string(), "run".to_string()];
            route.label = "nix run".to_string();
            route.provides = vec!["main".to_string()];
            route.guards.push(RouteGuard::CommandAvailable("nix".to_string()));
            routes.push(route);
        }

        if has_file(files, "default.nix") {
            let mut route = RouteSpec::new(
                "nix-build".to_string(),
                RouteProvenance::Discovered {
                    ecosystem: "nix".to_string(),
                    evidence: "default.nix present".to_string(),
                },
            );
            route.kind = RouteKind::BuildTarget;
            route.command = vec!["nix-build".to_string()];
            route.label = "nix-build".to_string();
            route.provides = vec!["build".to_string()];
            route.guards.push(RouteGuard::CommandAvailable("nix-build".to_string()));
            routes.push(route);
        }

        routes
    }
}
