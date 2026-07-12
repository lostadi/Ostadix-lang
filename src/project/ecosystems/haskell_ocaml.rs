//! Haskell / OCaml route discovery (thin, best-effort).
//!
//! `*.cabal` → `cabal run`; `dune-project` → `dune build`.

use std::path::Path;

use crate::project::discover::{has_file, EcosystemDiscoverer};
use crate::project::model::{ProjectFile, RouteGuard, RouteKind, RouteProvenance, RouteSpec};

pub struct HaskellOcamlDiscoverer;

impl EcosystemDiscoverer for HaskellOcamlDiscoverer {
    fn name(&self) -> &str {
        "haskell_ocaml"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let mut routes = Vec::new();

        if files.iter().any(|f| f.path.ends_with(".cabal")) {
            let mut route = RouteSpec::new(
                "cabal-run".to_string(),
                RouteProvenance::Discovered {
                    ecosystem: "haskell_ocaml".to_string(),
                    evidence: "a .cabal file is present".to_string(),
                },
            );
            route.kind = RouteKind::CompiledBinary;
            route.command = vec!["cabal".to_string(), "run".to_string()];
            route.label = "cabal run".to_string();
            route.provides = vec!["main".to_string()];
            route.guards.push(RouteGuard::CommandAvailable("cabal".to_string()));
            routes.push(route);
        }

        if has_file(files, "dune-project") {
            let mut route = RouteSpec::new(
                "dune-build".to_string(),
                RouteProvenance::Discovered {
                    ecosystem: "haskell_ocaml".to_string(),
                    evidence: "dune-project present".to_string(),
                },
            );
            route.kind = RouteKind::BuildTarget;
            route.command = vec!["dune".to_string(), "build".to_string()];
            route.label = "dune build".to_string();
            route.provides = vec!["build".to_string()];
            route.guards.push(RouteGuard::CommandAvailable("dune".to_string()));
            routes.push(route);
        }

        routes
    }
}
