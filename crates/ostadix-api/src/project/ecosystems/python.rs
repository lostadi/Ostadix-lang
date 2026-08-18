//! Python route discovery.
//!
//! Sources of routes:
//!   * `pyproject.toml` `[project.scripts]` and `[tool.poetry.scripts]`
//!   * packages that contain a `__main__.py` → `python3 -m <pkg>`
//!   * files containing an `if __name__ == "__main__"` guard → `python3 <file>`
//!   * executable files whose shebang names a python interpreter

use std::path::Path;

use crate::project::discover::{file_text, find_file, slug, EcosystemDiscoverer};
use crate::project::model::{ResultCodec, RouteKind, RouteProvenance, RouteSpec};

pub struct PythonDiscoverer;

fn discovered(id: String, ecosystem: &str, evidence: String) -> RouteSpec {
    RouteSpec::new(
        id,
        RouteProvenance::Discovered {
            ecosystem: ecosystem.to_string(),
            evidence,
        },
    )
}

impl EcosystemDiscoverer for PythonDiscoverer {
    fn name(&self) -> &str {
        "python"
    }

    fn discover(
        &self,
        _root: &Path,
        files: &[crate::project::model::ProjectFile],
    ) -> Vec<RouteSpec> {
        let mut routes = Vec::new();

        // ── pyproject.toml scripts ──────────────────────────────────────────
        if let Some(pyproject) = find_file(files, "pyproject.toml").and_then(file_text) {
            if let Ok(value) = pyproject.parse::<toml::Value>() {
                collect_scripts(&value, &mut routes);
            }
        }

        // ── packages with __main__.py ───────────────────────────────────────
        for file in files {
            if file.path.ends_with("__main__.py") {
                let pkg_path = file.path.trim_end_matches("/__main__.py");
                // Root-level __main__.py has no enclosing package dir to run.
                if pkg_path.is_empty() || pkg_path == file.path {
                    continue;
                }
                let module = pkg_path.replace('/', ".");
                let mut route = discovered(
                    format!("py-module-{}", slug(&module)),
                    "python",
                    format!("{} declares a runnable package", file.path),
                );
                route.kind = RouteKind::PackageEntrypoint;
                route.command = vec!["python3".to_string(), "-m".to_string(), module.clone()];
                route.entrypoint = Some(file.path.clone());
                route.label = format!("python -m {module}");
                route.provides = vec!["main".to_string()];
                routes.push(route);
            }
        }

        // ── files with an if __name__ == "__main__" guard ───────────────────
        for file in files {
            if !file.path.ends_with(".py") || file.path.ends_with("__main__.py") {
                continue;
            }
            let Some(text) = file_text(file) else {
                continue;
            };
            if !has_main_guard(text) {
                continue;
            }
            let mut route = discovered(
                format!("py-main-{}", slug(&file.path)),
                "python",
                format!("{} has an `if __name__ == \"__main__\"` guard", file.path),
            );
            route.kind = RouteKind::InterpreterCommand;
            route.command = vec!["python3".to_string(), file.path.clone()];
            route.entrypoint = Some(file.path.clone());
            route.label = format!("python3 {}", file.path);
            routes.push(route);
        }

        // ── executable python shebang files ─────────────────────────────────
        for file in files {
            if !file.executable {
                continue;
            }
            let Some(text) = file_text(file) else {
                continue;
            };
            if !shebang_is_python(text) {
                continue;
            }
            let id = format!("py-exec-{}", slug(&file.path));
            if routes
                .iter()
                .any(|r| r.entrypoint.as_deref() == Some(file.path.as_str()))
            {
                continue;
            }
            let mut route = discovered(
                id,
                "python",
                format!("{} is an executable python script", file.path),
            );
            route.kind = RouteKind::InterpreterCommand;
            route.command = vec!["python3".to_string(), file.path.clone()];
            route.entrypoint = Some(file.path.clone());
            route.label = format!("python3 {}", file.path);
            routes.push(route);
        }

        routes
    }
}

fn collect_scripts(value: &toml::Value, routes: &mut Vec<RouteSpec>) {
    let script_tables = [
        value.get("project").and_then(|p| p.get("scripts")),
        value
            .get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("scripts")),
    ];
    for table in script_tables.into_iter().flatten() {
        let Some(map) = table.as_table() else {
            continue;
        };
        for (name, target) in map {
            let Some(target) = target.as_str() else {
                continue;
            };
            let mut route = discovered(
                format!("py-script-{}", slug(name)),
                "python",
                format!("pyproject.toml script `{name} = {target}`"),
            );
            route.kind = RouteKind::PackageEntrypoint;
            route.command = vec![
                "python3".to_string(),
                "-c".to_string(),
                script_runner(target),
            ];
            route.label = format!("script {name}");
            route.provides = vec![name.clone()];
            route.result_codec = ResultCodec::Text;
            routes.push(route);
        }
    }
}

/// Build a `python3 -c` runner for a `module:function` (or `module`) target.
fn script_runner(target: &str) -> String {
    match target.split_once(':') {
        Some((module, func)) => format!(
            "import sys; from {module} import {func}; sys.exit({func}())",
            module = module.trim(),
            func = func.trim(),
        ),
        None => format!(
            "import runpy; runpy.run_module({:?}, run_name='__main__')",
            target.trim()
        ),
    }
}

fn has_main_guard(text: &str) -> bool {
    text.lines().any(|line| {
        let l = line.replace(char::is_whitespace, "");
        l.starts_with("if__name__==\"__main__\"") || l.starts_with("if__name__=='__main__'")
    })
}

fn shebang_is_python(text: &str) -> bool {
    text.lines()
        .next()
        .map(|line| line.starts_with("#!") && line.contains("python"))
        .unwrap_or(false)
}
