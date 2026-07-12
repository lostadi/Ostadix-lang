//! JavaScript / Node.js route discovery.
//!
//! Sources of routes:
//!   * `package.json` `scripts` → a script-runner task per entry, using the
//!     detected package manager (pnpm-lock.yaml → pnpm, yarn.lock → yarn,
//!     otherwise npm)
//!   * `package.json` `bin` → a `node <bin>` package entrypoint per entry
//!   * `package.json` `main` → a `node <main>` route

use std::path::Path;

use crate::project::discover::{file_text, find_file, has_file, slug, EcosystemDiscoverer};
use crate::project::model::{ProjectFile, RouteKind, RouteProvenance, RouteSpec};

pub struct JavaScriptDiscoverer;

fn package_manager(files: &[ProjectFile]) -> &'static str {
    if has_file(files, "pnpm-lock.yaml") {
        "pnpm"
    } else if has_file(files, "yarn.lock") {
        "yarn"
    } else {
        "npm"
    }
}

fn discovered(id: String, evidence: String) -> RouteSpec {
    RouteSpec::new(
        id,
        RouteProvenance::Discovered {
            ecosystem: "javascript".to_string(),
            evidence,
        },
    )
}

impl EcosystemDiscoverer for JavaScriptDiscoverer {
    fn name(&self) -> &str {
        "javascript"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let Some(pkg) = find_file(files, "package.json").and_then(file_text) else {
            return Vec::new();
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(pkg) else {
            return Vec::new();
        };

        let pm = package_manager(files);
        let mut routes = Vec::new();

        // ── scripts ─────────────────────────────────────────────────────────
        if let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) {
            for (name, _body) in scripts {
                let mut route = discovered(
                    format!("js-script-{}", slug(name)),
                    format!("package.json script `{name}` via {pm}"),
                );
                route.kind = RouteKind::ShellTask;
                route.command = vec![pm.to_string(), "run".to_string(), name.clone()];
                route.label = format!("{pm} run {name}");
                route.provides = vec![name.clone()];
                routes.push(route);
            }
        }

        // ── bin ─────────────────────────────────────────────────────────────
        match json.get("bin") {
            Some(serde_json::Value::String(path)) => {
                let name = json
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("bin")
                    .to_string();
                routes.push(node_bin_route(&name, path));
            }
            Some(serde_json::Value::Object(map)) => {
                for (name, path) in map {
                    if let Some(path) = path.as_str() {
                        routes.push(node_bin_route(name, path));
                    }
                }
            }
            _ => {}
        }

        // ── main ────────────────────────────────────────────────────────────
        if let Some(main) = json.get("main").and_then(|m| m.as_str()) {
            let mut route = discovered(
                "js-main".to_string(),
                format!("package.json main entry `{main}`"),
            );
            route.kind = RouteKind::PackageEntrypoint;
            route.command = vec!["node".to_string(), main.to_string()];
            route.entrypoint = Some(main.to_string());
            route.label = format!("node {main}");
            route.provides = vec!["main".to_string()];
            routes.push(route);
        }

        routes
    }
}

fn node_bin_route(name: &str, path: &str) -> RouteSpec {
    let mut route = discovered(
        format!("js-bin-{}", slug(name)),
        format!("package.json bin `{name}` → {path}"),
    );
    route.kind = RouteKind::PackageEntrypoint;
    route.command = vec!["node".to_string(), path.to_string()];
    route.entrypoint = Some(path.to_string());
    route.label = format!("node {path}");
    route.provides = vec![name.to_string()];
    route
}
