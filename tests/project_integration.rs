//! Integration tests for the first-class project model: bundling, manifest
//! parsing, discovery, materialization, runtime, and lowering.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use o_lang::project::bundle::{bundle_dir, bundle_dir_excluding, deserialize, serialize};
use o_lang::project::lower::{extract_bundle_from_o, has_embedded_bundle, lower_to_o_validated};
use o_lang::project::manifest::{apply_cli_overrides, apply_manifest, parse_route_decl};
use o_lang::project::materialize::{materialize, materialize_isolated};
use o_lang::project::model::{
    FileRole, ProjectBundle, ProjectFile, ResultCodec, RoutePolicy, RouteProvenance, RouteSet,
    RouteSpec,
};
use o_lang::project::runtime::{glob_match, run_route, run_selection, GuardBehavior, RunOptions};
use o_lang::project::{assemble, discover, RouteGuard};

fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[cfg(unix)]
fn chmod_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// Bundling round-trip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn project_bundle_roundtrip_lossless() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(root, "a.txt", b"hello world");
    write(root, "data.bin", &[0u8, 1, 2, 255, 254, 0, 42]);
    write(root, "empty", b"");
    write(root, "LICENSE", b"MIT-ish text");
    write(root, "nested/deep/file.md", b"# heading\n");
    write(root, "run.sh", b"#!/usr/bin/env bash\necho hi\n");
    #[cfg(unix)]
    chmod_exec(&root.join("run.sh"));
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", root.join("link.txt")).unwrap();

    let bundle = bundle_dir(root, "roundtrip").unwrap();

    // Serialize → deserialize preserves the bundle exactly.
    let bytes = serialize(&bundle).unwrap();
    let restored = deserialize(&bytes).unwrap();
    assert_eq!(bundle, restored);

    // Materialize into a fresh dir and compare byte-for-byte.
    let out = tempfile::tempdir().unwrap();
    materialize(&restored, out.path()).unwrap();

    assert_eq!(fs::read(out.path().join("a.txt")).unwrap(), b"hello world");
    assert_eq!(
        fs::read(out.path().join("data.bin")).unwrap(),
        vec![0u8, 1, 2, 255, 254, 0, 42]
    );
    assert_eq!(fs::read(out.path().join("empty")).unwrap(), b"");
    assert_eq!(
        fs::read(out.path().join("LICENSE")).unwrap(),
        b"MIT-ish text"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(out.path().join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "executable bit must be restored");

        let target = fs::read_link(out.path().join("link.txt")).unwrap();
        assert_eq!(target.to_string_lossy(), "a.txt");
    }

    // The empty file is captured, and the binary asset is classified as such.
    let bin = bundle.files.iter().find(|f| f.path == "data.bin").unwrap();
    assert_eq!(bin.role, FileRole::Asset);
    assert!(bundle.files.iter().any(|f| f.path == "empty"));
}

#[test]
fn project_bundle_fingerprint_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "x.py", b"print(1)\n");
    write(dir.path(), "y.txt", b"data\n");

    let a = bundle_dir(dir.path(), "p").unwrap();
    let b = bundle_dir(dir.path(), "p").unwrap();
    assert_eq!(a.root_fingerprint, b.root_fingerprint);
    assert!(!a.root_fingerprint.is_empty());
}

#[test]
fn project_bundle_skips_git_and_target() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "keep.txt", b"keep");
    write(dir.path(), ".git/config", b"[core]");
    write(dir.path(), "target/debug/artifact", b"binary");

    let bundle = bundle_dir(dir.path(), "p").unwrap();
    assert!(bundle.files.iter().any(|f| f.path == "keep.txt"));
    assert!(!bundle.files.iter().any(|f| f.path.starts_with(".git")));
    assert!(!bundle.files.iter().any(|f| f.path.starts_with("target")));
}

#[test]
fn project_bundle_excludes_requested_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "keep.txt", b"keep");
    write(
        dir.path(),
        "combined.O",
        b"ordinary pre-existing file, not an o-link document\n",
    );

    let output = dir.path().join("combined.O");
    let bundle = bundle_dir_excluding(dir.path(), "p", &[output]).unwrap();
    assert!(bundle.files.iter().any(|file| file.path == "keep.txt"));
    assert!(!bundle.files.iter().any(|file| file.path == "combined.O"));
}

#[test]
fn project_bundle_skips_generated_olink_documents() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "keep.txt", b"keep");
    write(
        dir.path(),
        "combined.O",
        b"# Ostadix-lang lifted project\n# generated output\n",
    );
    write(
        dir.path(),
        "legacy.O",
        b"# Linked by o-link\n# generated output\n",
    );
    write(
        dir.path(),
        "executable.O",
        b"#!/usr/bin/env o\n# Ostadix-lang lifted project\n",
    );

    let bundle = bundle_dir(dir.path(), "p").unwrap();
    assert!(bundle.files.iter().any(|file| file.path == "keep.txt"));
    for generated in ["combined.O", "legacy.O", "executable.O"] {
        assert!(!bundle.files.iter().any(|file| file.path == generated));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Materialization safety
// ─────────────────────────────────────────────────────────────────────────────

fn bundle_with_path(path: &str) -> ProjectBundle {
    let mut bundle = ProjectBundle::empty("unsafe");
    bundle.files.push(ProjectFile {
        path: path.to_string(),
        bytes: b"payload".to_vec(),
        executable: false,
        unix_mode: None,
        symlink_target: None,
        evaluator: None,
        content_hash: "0".repeat(64),
        role: FileRole::Other,
    });
    bundle
}

#[test]
fn project_materialize_rejects_parent_traversal() {
    let out = tempfile::tempdir().unwrap();
    let bundle = bundle_with_path("../escape.txt");
    let err = materialize(&bundle, out.path()).unwrap_err();
    assert!(err.to_string().contains(".."), "got: {err}");
    assert!(!out.path().parent().unwrap().join("escape.txt").exists());
}

#[test]
fn project_materialize_rejects_absolute_path() {
    let out = tempfile::tempdir().unwrap();
    let bundle = bundle_with_path("/tmp/olang-abs-escape-test.txt");
    let err = materialize(&bundle, out.path()).unwrap_err();
    assert!(err.to_string().contains("absolute"), "got: {err}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Manifest parsing and overrides
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn project_manifest_parses_routes_and_route_sets() {
    let mut bundle = ProjectBundle::empty("example");
    // A discovered route that the manifest should override.
    let mut discovered = RouteSpec::new(
        "main-a",
        RouteProvenance::Discovered {
            ecosystem: "python".into(),
            evidence: "auto".into(),
        },
    );
    discovered.command = vec!["python3".into(), "old.py".into()];
    bundle.routes.push(discovered);

    let manifest = r#"
[project]
name = "example"
default_route = "main-a"

[[routes]]
id = "main-a"
command = ["python3", "implementation_a.py"]
cwd = "."
provides = ["main"]
result_codec = "json"
depends_on = ["assets"]
outputs = ["dist/**"]
env = { KEY = "V" }
guards = { os = "linux", requires_command = "python3" }

[[routes]]
id = "main-b"
command = ["python3", "implementation_b.py"]
provides = ["main"]

[[route_sets]]
provides = "main"
alternatives = ["main-a", "main-b"]
policy = "explicit"
"#;

    apply_manifest(&mut bundle, manifest, "olang.project.toml").unwrap();

    assert_eq!(bundle.name, "example");
    assert_eq!(bundle.default_route.as_deref(), Some("main-a"));

    let main_a = bundle.route("main-a").unwrap();
    // Manifest wins: the discovered "old.py" command is replaced.
    assert_eq!(main_a.command, vec!["python3", "implementation_a.py"]);
    assert_eq!(main_a.result_codec, ResultCodec::Json);
    assert_eq!(main_a.prerequisites, vec!["assets"]);
    assert_eq!(main_a.outputs, vec!["dist/**"]);
    assert_eq!(main_a.environment.get("KEY").map(|s| s.as_str()), Some("V"));
    assert!(matches!(
        main_a.provenance,
        RouteProvenance::Manifest { .. }
    ));
    assert!(main_a
        .guards
        .contains(&RouteGuard::CommandAvailable("python3".into())));

    let set = bundle.route_set("main").unwrap();
    assert_eq!(set.alternatives, vec!["main-a", "main-b"]);
    assert_eq!(set.policy, RoutePolicy::Explicit(String::new()));
}

#[test]
fn project_cli_route_decl_micro_syntax() {
    let spec = parse_route_decl(
        "id=main-a;cmd=python3 implementation_a.py;cwd=.;provides=main;codec=json;depends=assets",
    )
    .unwrap();
    assert_eq!(spec.id, "main-a");
    assert_eq!(spec.command, vec!["python3", "implementation_a.py"]);
    assert_eq!(spec.working_directory, ".");
    assert_eq!(spec.provides, vec!["main"]);
    assert_eq!(spec.result_codec, ResultCodec::Json);
    assert_eq!(spec.prerequisites, vec!["assets"]);
    assert!(matches!(spec.provenance, RouteProvenance::CliOverride));
}

#[test]
fn project_cli_overrides_replace_existing_routes() {
    let mut bundle = ProjectBundle::empty("p");
    let mut r = RouteSpec::new("run", RouteProvenance::CliOverride);
    r.command = vec!["echo".into(), "old".into()];
    bundle.routes.push(r);

    apply_cli_overrides(&mut bundle, &["id=run;cmd=echo new".to_string()]).unwrap();

    assert_eq!(bundle.routes.len(), 1);
    assert_eq!(bundle.route("run").unwrap().command, vec!["echo", "new"]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Discovery
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn project_discovery_python() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app.py",
        b"def go():\n    pass\n\nif __name__ == \"__main__\":\n    go()\n",
    );
    write(dir.path(), "pkg/__main__.py", b"print('pkg')\n");
    write(
        dir.path(),
        "pyproject.toml",
        b"[project.scripts]\nmytool = \"pkg.cli:main\"\n",
    );

    let bundle = bundle_dir(dir.path(), "py").unwrap();
    let routes = discover::discover_all(dir.path(), &bundle.files);
    let ids: Vec<&str> = routes.iter().map(|r| r.id.as_str()).collect();

    assert!(ids.iter().any(|id| id.starts_with("py-main-")), "{ids:?}");
    assert!(routes.iter().any(|r| r.id == "py-module-pkg"), "{ids:?}");
    assert!(routes.iter().any(|r| r.id == "py-script-mytool"), "{ids:?}");
}

#[test]
fn project_discovery_python_single_quote_main_guard() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "single.py",
        b"if __name__ == '__main__':\n    print('hi')\n",
    );
    let bundle = bundle_dir(dir.path(), "py").unwrap();
    let routes = discover::discover_all(dir.path(), &bundle.files);
    assert!(routes.iter().any(|r| r.id.starts_with("py-main-")));
}

#[test]
fn project_discovery_javascript() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        br#"{"name":"app","main":"index.js","scripts":{"build":"tsc","start":"node ."},"bin":{"app":"cli.js"}}"#,
    );
    write(dir.path(), "yarn.lock", b"# yarn lockfile\n");

    let bundle = bundle_dir(dir.path(), "js").unwrap();
    let routes = discover::discover_all(dir.path(), &bundle.files);

    let start = routes.iter().find(|r| r.id == "js-script-start").unwrap();
    assert_eq!(start.command, vec!["yarn", "run", "start"]);
    assert!(routes.iter().any(|r| r.id == "js-main"));
    assert!(routes.iter().any(|r| r.id == "js-bin-app"));
}

#[test]
fn project_discovery_rust() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        b"[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    write(dir.path(), "src/main.rs", b"fn main() {}\n");

    let bundle = bundle_dir(dir.path(), "rs").unwrap();
    let routes = discover::discover_all(dir.path(), &bundle.files);
    assert!(routes.iter().any(|r| r.id == "rust-run"));
    assert!(routes.iter().any(|r| r.id == "rust-build"));
    assert!(routes.iter().any(|r| r.id == "rust-test"));
}

#[test]
fn project_discovery_makefile() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Makefile",
        b"all: build\n\tsomething\n\nbuild:\n\tcc x.c\n\n%.o: %.c\n\tcc -c $<\n",
    );
    let bundle = bundle_dir(dir.path(), "mk").unwrap();
    let routes = discover::discover_all(dir.path(), &bundle.files);
    let ids: Vec<&str> = routes.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&"make-all"), "{ids:?}");
    assert!(ids.contains(&"make-build"), "{ids:?}");
    // Pattern rules (`%.o`) must not become targets.
    assert!(!ids.iter().any(|id| id.contains("o-c") || id.contains('%')));
}

#[test]
fn project_assemble_single_run_candidate_becomes_default() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app.py",
        b"if __name__ == \"__main__\":\n    print('x')\n",
    );
    let bundle = assemble(dir.path(), "solo", &[]).unwrap();
    assert!(bundle.default_route.is_some(), "single run route → default");
}

#[test]
fn project_assemble_multiple_run_candidates_no_default() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.py",
        b"if __name__ == \"__main__\":\n    print('a')\n",
    );
    write(
        dir.path(),
        "b.py",
        b"if __name__ == \"__main__\":\n    print('b')\n",
    );
    let bundle = assemble(dir.path(), "multi", &[]).unwrap();
    assert!(
        bundle.default_route.is_none(),
        "ambiguous run routes → no default"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Runtime
// ─────────────────────────────────────────────────────────────────────────────

fn shell_route(id: &str, script: &str) -> RouteSpec {
    let mut r = RouteSpec::new(id, RouteProvenance::CliOverride);
    r.command = vec!["sh".into(), "-c".into(), script.into()];
    r
}

fn bundle_with_routes(routes: Vec<RouteSpec>) -> ProjectBundle {
    let mut bundle = ProjectBundle::empty("rt");
    // A tiny file so the workspace is non-empty.
    bundle.files.push(ProjectFile {
        path: "marker".into(),
        bytes: b"x".to_vec(),
        executable: false,
        unix_mode: None,
        symlink_target: None,
        evaluator: None,
        content_hash: "0".repeat(64),
        role: FileRole::Other,
    });
    bundle.routes = routes;
    bundle
}

#[test]
fn project_runtime_prerequisite_shares_workspace() {
    // Prerequisite writes a file; the dependent reads it in the same workspace.
    let mut prereq = shell_route("prep", "echo shared > artifact.txt");
    prereq.outputs = vec!["artifact.txt".into()];
    let mut main = shell_route("main", "cat artifact.txt");
    main.prerequisites = vec!["prep".into()];
    main.result_codec = ResultCodec::Text;

    let bundle = bundle_with_routes(vec![prereq, main]);
    let result = run_route(&bundle, "main", &RunOptions::default()).unwrap();
    assert!(result.succeeded(), "stderr: {}", result.stderr_text());
    assert!(result.stdout_text().contains("shared"));
}

#[test]
fn project_runtime_collects_output_artifacts() {
    let mut route = shell_route("gen", "mkdir -p dist && echo hi > dist/out.txt");
    route.outputs = vec!["dist/**".into()];
    let bundle = bundle_with_routes(vec![route]);
    let result = run_route(&bundle, "gen", &RunOptions::default()).unwrap();
    assert!(result.succeeded());
    assert!(
        result.artifacts.iter().any(|a| a.path == "dist/out.txt"),
        "artifacts: {:?}",
        result.artifacts
    );
}

#[test]
fn project_runtime_json_codec_decodes_value() {
    let mut route = shell_route("j", "echo '{\"ok\":true,\"n\":3}'");
    route.result_codec = ResultCodec::Json;
    let bundle = bundle_with_routes(vec![route]);
    let result = run_route(&bundle, "j", &RunOptions::default()).unwrap();
    let value = result.value.unwrap();
    assert_eq!(value["ok"], serde_json::json!(true));
    assert_eq!(value["n"], serde_json::json!(3));
}

#[test]
fn project_runtime_cycle_is_detected() {
    let mut a = shell_route("a", "true");
    a.prerequisites = vec!["b".into()];
    let mut b = shell_route("b", "true");
    b.prerequisites = vec!["a".into()];
    let bundle = bundle_with_routes(vec![a, b]);

    let err = run_route(&bundle, "a", &RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("cycle"), "got: {err}");
}

#[test]
fn project_runtime_failed_prerequisite_aborts() {
    let prereq = shell_route("bad", "exit 7");
    let mut main = shell_route("main", "echo unreachable");
    main.prerequisites = vec!["bad".into()];
    let bundle = bundle_with_routes(vec![prereq, main]);

    let err = run_route(&bundle, "main", &RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("prerequisite"), "got: {err}");
}

#[test]
fn project_runtime_guard_enforce_vs_skip() {
    let mut route = shell_route("guarded", "echo ran");
    route.guards.push(RouteGuard::CommandAvailable(
        "definitely-not-a-real-cmd-xyz".into(),
    ));
    let bundle = bundle_with_routes(vec![route]);

    // Enforce → error.
    let err = run_route(&bundle, "guarded", &RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("guard"), "got: {err}");

    // Skip → synthetic no-op result, command never runs.
    let opts = RunOptions {
        guard_behavior: GuardBehavior::Skip,
        inherit_env: true,
    };
    let result = run_route(&bundle, "guarded", &opts).unwrap();
    assert_eq!(result.exit_code, None);
    assert!(!result.stdout_text().contains("ran"));
}

#[test]
fn project_runtime_all_policy_uses_isolated_workspaces() {
    // Both alternatives write the same project-relative path but must not
    // collide because each runs in its own isolated workspace.
    let mut a = shell_route("alt-a", "echo A > result.txt && cat result.txt");
    a.result_codec = ResultCodec::Text;
    let mut b = shell_route("alt-b", "echo B > result.txt && cat result.txt");
    b.result_codec = ResultCodec::Text;

    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["alt-a".into(), "alt-b".into()],
        policy: RoutePolicy::All,
    });

    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.succeeded()));
    let outputs: Vec<String> = results
        .iter()
        .map(|r| r.stdout_text().trim().to_string())
        .collect();
    assert!(outputs.contains(&"A".to_string()));
    assert!(outputs.contains(&"B".to_string()));
    // Distinct workspaces.
    assert_ne!(
        results[0].provenance.workspace,
        results[1].provenance.workspace
    );
}

#[test]
fn project_runtime_default_policy_never_runs_all_alternatives() {
    let a = shell_route("alt-a", "echo A");
    let b = shell_route("alt-b", "echo B");
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["alt-a".into(), "alt-b".into()],
        policy: RoutePolicy::Default,
    });

    // No unambiguous default among the alternatives → error, and crucially it
    // does NOT execute both alternatives.
    let err = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("default"), "got: {err}");
}

#[test]
fn project_runtime_fallback_stops_at_first_success() {
    let mut first = shell_route("f1", "exit 1");
    first.priority = 10;
    let mut second = shell_route("f2", "echo recovered");
    second.priority = 5;
    let mut bundle = bundle_with_routes(vec![first, second]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["f1".into(), "f2".into()],
        policy: RoutePolicy::Fallback,
    });

    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    // Higher-priority f1 runs first (fails), then f2 succeeds.
    assert_eq!(results.len(), 2);
    assert!(!results[0].succeeded());
    assert!(results[1].succeeded());
}

#[test]
fn project_runtime_race_success_selects_success_and_cancels_losers() {
    // fast-fail settles first but fails; slow-ok is the only success. The
    // hanging alternative would block forever without cooperative cancellation.
    let fast_fail = shell_route("fast-fail", "exit 3");
    let slow_ok = shell_route("slow-ok", "sleep 0.2; echo winner");
    let hang = shell_route("hang", "sleep 30; echo never");
    let mut bundle = bundle_with_routes(vec![fast_fail, slow_ok, hang]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["fast-fail".into(), "slow-ok".into(), "hang".into()],
        policy: RoutePolicy::RaceSuccess,
    });

    let start = std::time::Instant::now();
    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "losers were not cancelled"
    );
    let selected = results.last().unwrap();
    assert_eq!(selected.route_id, "slow-ok");
    assert!(selected.succeeded());
    assert!(selected.stdout_text().contains("winner"));
}

#[test]
fn project_runtime_race_success_all_failures_reports_no_success() {
    let a = shell_route("a", "exit 1");
    let b = shell_route("b", "exit 2");
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["a".into(), "b".into()],
        policy: RoutePolicy::RaceSuccess,
    });

    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| !r.succeeded()));
}

#[test]
fn project_runtime_race_settle_returns_first_settled_even_failure() {
    let fast_fail = shell_route("fast-fail", "exit 7");
    let hang = shell_route("hang", "sleep 30; echo never");
    let mut bundle = bundle_with_routes(vec![fast_fail, hang]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["hang".into(), "fast-fail".into()],
        policy: RoutePolicy::RaceSettle,
    });

    let start = std::time::Instant::now();
    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "loser was not cancelled"
    );
    let selected = results.last().unwrap();
    assert_eq!(selected.route_id, "fast-fail");
    assert_eq!(selected.exit_code, Some(7));
}

#[test]
fn project_runtime_verify_equivalent_accepts_matching_outputs() {
    let a = shell_route("alt-a", "echo same");
    let b = shell_route("alt-b", "printf 'same\\n'");
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["alt-a".into(), "alt-b".into()],
        policy: RoutePolicy::VerifyEquivalent,
    });

    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.succeeded()));
}

#[test]
fn project_runtime_verify_equivalent_rejects_divergent_outputs() {
    let a = shell_route("alt-a", "echo one");
    let b = shell_route("alt-b", "echo two");
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["alt-a".into(), "alt-b".into()],
        policy: RoutePolicy::VerifyEquivalent,
    });

    let err = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("different stdout"), "got: {err}");
}

#[test]
fn project_runtime_verify_equivalent_compares_json_values() {
    let mut a = shell_route("alt-a", "echo '{\"x\": 1, \"y\": 2}'");
    a.result_codec = ResultCodec::Json;
    let mut b = shell_route("alt-b", "echo '{\"y\":2,\"x\":1}'");
    b.result_codec = ResultCodec::Json;
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["alt-a".into(), "alt-b".into()],
        policy: RoutePolicy::VerifyEquivalent,
    });

    // Key order differs but decoded JSON values are equal.
    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn project_runtime_verify_equivalent_requires_all_success() {
    let a = shell_route("alt-a", "echo ok");
    let b = shell_route("alt-b", "exit 1");
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["alt-a".into(), "alt-b".into()],
        policy: RoutePolicy::VerifyEquivalent,
    });

    let err = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap_err();
    assert!(
        err.to_string()
            .contains("requires every alternative to succeed"),
        "got: {err}"
    );
}

#[test]
fn project_runtime_benchmark_selects_fastest_success() {
    let slow = shell_route("slow", "sleep 0.4; echo slow");
    let fast = shell_route("fast", "echo fast");
    let broken = shell_route("broken", "exit 1");
    let mut bundle = bundle_with_routes(vec![slow, fast, broken]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["slow".into(), "fast".into(), "broken".into()],
        policy: RoutePolicy::BenchmarkAndSelect,
    });

    let results = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap();
    assert_eq!(results.len(), 3);
    let selected = results.last().unwrap();
    assert_eq!(selected.route_id, "fast");
    assert!(selected.succeeded());
}

#[test]
fn project_runtime_benchmark_fails_when_nothing_succeeds() {
    let a = shell_route("a", "exit 1");
    let b = shell_route("b", "exit 2");
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["a".into(), "b".into()],
        policy: RoutePolicy::BenchmarkAndSelect,
    });

    let err = run_selection(&bundle, Some("main"), None, &RunOptions::default()).unwrap_err();
    assert!(
        err.to_string().contains("no alternative succeeded"),
        "got: {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Glob matching
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn project_glob_matcher() {
    assert!(glob_match("dist/**", "dist/a.txt"));
    assert!(glob_match("dist/**", "dist/sub/b.txt"));
    assert!(glob_match("**/*.txt", "a/b/c.txt"));
    assert!(glob_match("*.rs", "main.rs"));
    assert!(!glob_match("*.rs", "src/main.rs"));
    assert!(glob_match("src/*.rs", "src/main.rs"));
    assert!(!glob_match("dist/**", "build/a.txt"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Lowering
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn project_lower_to_o_reparses_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.py", b"print('hi')\n");
    write(dir.path(), "data.bin", &[0u8, 1, 2, 3, 255]);
    write(
        dir.path(),
        "app.py",
        b"if __name__ == \"__main__\":\n    print('run')\n",
    );

    let bundle = assemble(dir.path(), "lift", &[]).unwrap();
    let lifted = lower_to_o_validated(&bundle).unwrap();

    assert!(has_embedded_bundle(&lifted));

    // The lifted document does NOT wrap each source file as an evaluator block.
    // It contains one payload block and one inert direct-evaluation notice.
    assert_eq!(
        lifted.matches("text^(").count(),
        2,
        "payload plus safety notice"
    );
    assert!(!lifted.contains("python^(") && !lifted.contains("bash^("));
    assert!(lifted.contains("Ostadix project bundle loaded safely."));
    assert!(lifted.contains("No project route was executed."));

    let extracted = extract_bundle_from_o(&lifted).unwrap();
    assert_eq!(extracted, bundle);
}

// ─────────────────────────────────────────────────────────────────────────────
// Isolated workspaces
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn project_isolated_workspaces_are_distinct() {
    let bundle = bundle_with_routes(vec![]);
    let ws1 = materialize_isolated(&bundle).unwrap();
    let ws2 = materialize_isolated(&bundle).unwrap();
    assert!(ws1.isolated && ws2.isolated);
    assert_ne!(ws1.root, ws2.root);
    assert!(ws1.root.join("marker").exists());
}

// ─────────────────────────────────────────────────────────────────────────────
// Route table rendering
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn project_route_table_lists_routes_without_executing() {
    let mut bundle = ProjectBundle::empty("tbl");
    let mut r = RouteSpec::new("run", RouteProvenance::CliOverride);
    r.command = vec!["echo".into(), "hi".into()];
    r.provides = vec!["main".into()];
    bundle.routes.push(r);
    bundle.metadata = BTreeMap::new();

    let table = bundle.route_table();
    assert!(table.contains("run"));
    assert!(table.contains("echo hi"));
}
