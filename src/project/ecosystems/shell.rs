//! Shell / Make route discovery.
//!
//! Sources of routes:
//!   * executable files with a shebang → run via the shebang interpreter
//!   * `Makefile` targets (`^name:` lines, excluding pattern rules)

use std::path::Path;

use crate::project::discover::{file_text, find_file, slug, EcosystemDiscoverer};
use crate::project::model::{ProjectFile, RouteKind, RouteProvenance, RouteSpec};

pub struct ShellDiscoverer;

fn discovered(id: String, evidence: String) -> RouteSpec {
    RouteSpec::new(
        id,
        RouteProvenance::Discovered {
            ecosystem: "shell".to_string(),
            evidence,
        },
    )
}

impl EcosystemDiscoverer for ShellDiscoverer {
    fn name(&self) -> &str {
        "shell"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let mut routes = Vec::new();

        // ── executable shebang scripts ──────────────────────────────────────
        for file in files {
            if !file.executable {
                continue;
            }
            let Some(text) = file_text(file) else {
                continue;
            };
            let Some(shebang) = shebang_interpreter(text) else {
                continue;
            };
            // Python executables are handled by the python discoverer.
            if shebang.contains("python") {
                continue;
            }
            let mut route = discovered(
                format!("sh-exec-{}", slug(&file.path)),
                format!("{} is an executable script ({shebang})", file.path),
            );
            route.kind = RouteKind::ShellTask;
            route.command = interpreter_command(&shebang, &file.path);
            route.entrypoint = Some(file.path.clone());
            route.label = route.command.join(" ");
            routes.push(route);
        }

        // ── Makefile targets ────────────────────────────────────────────────
        for name in ["Makefile", "makefile", "GNUmakefile"] {
            if let Some(makefile) = find_file(files, name).and_then(file_text) {
                for target in makefile_targets(makefile) {
                    let mut route = discovered(
                        format!("make-{}", slug(&target)),
                        format!("{name} target `{target}`"),
                    );
                    route.kind = RouteKind::ShellTask;
                    route.command = vec!["make".to_string(), target.clone()];
                    route.label = format!("make {target}");
                    route.provides = vec![target.clone()];
                    routes.push(route);
                }
                break;
            }
        }

        routes
    }
}

/// Return the interpreter portion of a shebang line, if present.
fn shebang_interpreter(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    let rest = first.strip_prefix("#!")?.trim();
    // `#!/usr/bin/env bash` → interpreter is `bash`.
    // `#!/bin/sh` → interpreter is `/bin/sh`.
    let mut parts = rest.split_whitespace();
    let head = parts.next()?;
    if head.ends_with("env") {
        parts.next().map(|s| s.to_string())
    } else {
        Some(head.to_string())
    }
}

fn interpreter_command(interpreter: &str, path: &str) -> Vec<String> {
    // Prefer a bare interpreter name so it resolves on PATH inside the
    // materialized workspace; fall back to the raw shebang path otherwise.
    let name = interpreter.rsplit('/').next().unwrap_or(interpreter);
    vec![name.to_string(), path.to_string()]
}

/// Extract simple explicit Makefile targets. Skips pattern rules (`%`), the
/// automatic `.SUFFIXES`/`.PHONY` style dot-directives, and variable
/// assignments.
fn makefile_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in text.lines() {
        // Recipe lines start with a tab; skip them and comments/blank lines.
        if line.starts_with('\t') || line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((lhs, _rhs)) = line.split_once(':') else {
            continue;
        };
        // Assignment (`:=`) is not a target.
        if _rhs.starts_with('=') || lhs.contains('=') {
            continue;
        }
        let name = lhs.trim();
        if name.is_empty()
            || name.contains('%')
            || name.contains(' ')
            || name.starts_with('.')
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
        {
            continue;
        }
        if !targets.contains(&name.to_string()) {
            targets.push(name.to_string());
        }
    }
    targets
}
