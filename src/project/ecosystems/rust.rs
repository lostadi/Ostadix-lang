//! Rust route discovery.
//!
//! `Cargo.toml` is parsed with the `toml` crate (never by shelling out to
//! `cargo metadata`). Discovered routes:
//!   * a default run route when `src/main.rs` exists
//!   * a run route per `[[bin]]` target
//!   * `cargo build` and `cargo test` build targets

use std::path::Path;

use crate::project::discover::{file_text, find_file, has_file, slug, EcosystemDiscoverer};
use crate::project::model::{ProjectFile, RouteGuard, RouteKind, RouteProvenance, RouteSpec};

pub struct RustDiscoverer;

fn discovered(id: String, evidence: String) -> RouteSpec {
    let mut route = RouteSpec::new(
        id,
        RouteProvenance::Discovered {
            ecosystem: "rust".to_string(),
            evidence,
        },
    );
    route.guards.push(RouteGuard::CommandAvailable("cargo".to_string()));
    route
}

impl EcosystemDiscoverer for RustDiscoverer {
    fn name(&self) -> &str {
        "rust"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let Some(cargo) = find_file(files, "Cargo.toml").and_then(file_text) else {
            return Vec::new();
        };
        let Ok(value) = cargo.parse::<toml::Value>() else {
            return Vec::new();
        };

        let mut routes = Vec::new();

        // ── explicit [[bin]] targets ────────────────────────────────────────
        let mut has_named_default = false;
        if let Some(bins) = value.get("bin").and_then(|b| b.as_array()) {
            for bin in bins {
                let Some(name) = bin.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let mut route = discovered(
                    format!("rust-bin-{}", slug(name)),
                    format!("Cargo.toml [[bin]] target `{name}`"),
                );
                route.kind = RouteKind::CompiledBinary;
                route.command =
                    vec!["cargo".to_string(), "run".to_string(), "--bin".to_string(), name.to_string()];
                route.label = format!("cargo run --bin {name}");
                route.provides = vec![name.to_string()];
                routes.push(route);
                has_named_default = true;
            }
        }

        // ── default bin (src/main.rs) ───────────────────────────────────────
        if has_file(files, "src/main.rs") && !has_named_default {
            let mut route = discovered(
                "rust-run".to_string(),
                "src/main.rs default binary".to_string(),
            );
            route.kind = RouteKind::CompiledBinary;
            route.command = vec!["cargo".to_string(), "run".to_string()];
            route.label = "cargo run".to_string();
            route.provides = vec!["main".to_string()];
            routes.push(route);
        }

        // ── build & test targets ────────────────────────────────────────────
        let mut build = discovered("rust-build".to_string(), "cargo build".to_string());
        build.kind = RouteKind::BuildTarget;
        build.command = vec!["cargo".to_string(), "build".to_string()];
        build.label = "cargo build".to_string();
        build.provides = vec!["build".to_string()];
        routes.push(build);

        let mut test = discovered("rust-test".to_string(), "cargo test".to_string());
        test.kind = RouteKind::BuildTarget;
        test.command = vec!["cargo".to_string(), "test".to_string()];
        test.label = "cargo test".to_string();
        test.provides = vec!["test".to_string()];
        routes.push(test);

        routes
    }
}
