//! Integration tests for the first-class project model: bundling, manifest
//! parsing, discovery, materialization, runtime, and lowering.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use o_lang::executor::CancellationToken;
use o_lang::project::bundle::{
    bundle_dir, bundle_dir_excluding, deserialize, serialize, serialize_pretty,
};
use o_lang::project::lower::{extract_bundle_from_o, has_embedded_bundle, lower_to_o_validated};
use o_lang::project::manifest::{apply_cli_overrides, apply_manifest, parse_route_decl};
use o_lang::project::materialize::{materialize, materialize_isolated};
use o_lang::project::model::{
    ArtifactCaptureFailure, ArtifactCaptureStatus, FileRole, ProjectBundle, ProjectFile,
    ResultCodec, RouteFailureContinuation, RoutePolicy, RouteProvenance, RouteSet, RouteSpec,
    BUNDLE_FORMAT_VERSION,
};
use o_lang::project::runtime::{
    glob_match, is_cancellation_error, is_timeout_error, run_route, run_route_cancellable,
    run_selection, ArtifactCaptureError, EnvironmentPolicy, GuardBehavior, ProcessTreePolicy,
    RouteExecutionError, RunOptions,
};
use o_lang::project::{assemble, discover, RouteGuard};
use sha2::{Digest, Sha256};

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
    assert_eq!(bundle.format_version, BUNDLE_FORMAT_VERSION);

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
fn project_bundle_v1_migrates_only_without_v2_continuation_fields() {
    let mut bundle = ProjectBundle::empty("legacy-v1");
    let mut route = RouteSpec::new("main", RouteProvenance::CliOverride);
    route.command = vec!["sh".into(), "-c".into(), "exit 0".into()];
    bundle.routes.push(route);

    let mut legacy = serde_json::to_value(&bundle).unwrap();
    legacy["format_version"] = serde_json::json!(1);
    for route in legacy["routes"].as_array_mut().unwrap() {
        route
            .as_object_mut()
            .unwrap()
            .remove("failure_continuation");
    }
    let migrated = deserialize(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert_eq!(migrated.format_version, BUNDLE_FORMAT_VERSION);
    assert_eq!(
        migrated.route("main").unwrap().failure_continuation,
        RouteFailureContinuation::Unproven
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&serialize(&migrated).unwrap()).unwrap()
            ["format_version"],
        BUNDLE_FORMAT_VERSION
    );

    let mut mislabeled_v1 = serde_json::to_value(&bundle).unwrap();
    mislabeled_v1["format_version"] = serde_json::json!(1);
    let error = deserialize(&serde_json::to_vec(&mislabeled_v1).unwrap()).unwrap_err();
    assert!(format!("{error:#}").contains("must not carry"));

    let mut future = serde_json::to_value(&bundle).unwrap();
    future["format_version"] = serde_json::json!(BUNDLE_FORMAT_VERSION + 1);
    let error = deserialize(&serde_json::to_vec(&future).unwrap()).unwrap_err();
    assert!(format!("{error:#}").contains("unsupported project bundle format version"));

    let mut mislabeled_in_memory = bundle;
    mislabeled_in_memory.format_version = 1;
    let error = serialize(&mislabeled_in_memory).unwrap_err();
    assert!(format!("{error:#}").contains("refusing to serialize"));
    let error = serialize_pretty(&mislabeled_in_memory).unwrap_err();
    assert!(format!("{error:#}").contains("refusing to serialize"));
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
failure_continuation = "declared_idempotent"

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
    assert_eq!(
        main_a.failure_continuation,
        RouteFailureContinuation::DeclaredIdempotent
    );
    assert_eq!(
        bundle.route("main-b").unwrap().failure_continuation,
        RouteFailureContinuation::Unproven,
        "an omitted continuation contract must remain fail-closed"
    );

    let set = bundle.route_set("main").unwrap();
    assert_eq!(set.alternatives, vec!["main-a", "main-b"]);
    assert_eq!(set.policy, RoutePolicy::Explicit(String::new()));
}

#[test]
fn project_cli_route_decl_micro_syntax() {
    let spec = parse_route_decl(
        "id=main-a;cmd=python3 implementation_a.py;cwd=.;provides=main;codec=json;depends=assets;failure_continuation=declared_idempotent",
    )
    .unwrap();
    assert_eq!(spec.id, "main-a");
    assert_eq!(spec.command, vec!["python3", "implementation_a.py"]);
    assert_eq!(spec.working_directory, ".");
    assert_eq!(spec.provides, vec!["main"]);
    assert_eq!(spec.result_codec, ResultCodec::Json);
    assert_eq!(spec.prerequisites, vec!["assets"]);
    assert_eq!(
        spec.failure_continuation,
        RouteFailureContinuation::DeclaredIdempotent
    );
    assert!(matches!(spec.provenance, RouteProvenance::CliOverride));
}

#[test]
fn invalid_failure_continuation_tokens_are_rejected() {
    let mut bundle = ProjectBundle::empty("invalid-continuation");
    let error = apply_manifest(
        &mut bundle,
        r#"[[routes]]
id = "main"
command = ["sh", "-c", "exit 0"]
failure_continuation = "trust_me"
"#,
        "olang.project.toml",
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("invalid failure_continuation"));

    let error =
        parse_route_decl("id=main;cmd=sh run.sh;failure_continuation=trust_me").unwrap_err();
    assert!(format!("{error:#}").contains("invalid failure_continuation"));
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
    let artifact = result
        .artifacts
        .iter()
        .find(|artifact| artifact.path == "dist/out.txt")
        .unwrap();
    assert_eq!(artifact.bytes_len, 3);
    assert_eq!(artifact.content_hash, hex::encode(Sha256::digest(b"hi\n")));
}

#[test]
fn project_runtime_missing_required_artifact_is_typed_failure() {
    let mut route = shell_route("missing", "true");
    route.outputs = vec!["required.bin".into()];
    let bundle = bundle_with_routes(vec![route]);

    let error = run_route(&bundle, "missing", &RunOptions::default()).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ArtifactCaptureError>(),
        Some(ArtifactCaptureError::Missing { requirement })
            if requirement == "required.bin"
    ));
}

#[test]
fn project_runtime_nonzero_missing_output_is_explicitly_incomplete() {
    let mut route = shell_route("nonzero-missing", "exit 7");
    route.outputs = vec!["required.bin".into()];
    let bundle = bundle_with_routes(vec![route]);

    let result = run_route(&bundle, "nonzero-missing", &RunOptions::default()).unwrap();
    assert_eq!(result.exit_code, Some(7));
    assert!(!result.succeeded());
    assert!(result.artifacts.is_empty());
    assert!(matches!(
        result.artifact_capture,
        ArtifactCaptureStatus::Incomplete { failure }
            if matches!(
                failure.as_ref(),
                ArtifactCaptureFailure::Missing { requirement }
                    if requirement == "required.bin"
            )
    ));
}

#[test]
fn project_runtime_artifact_limits_are_typed_and_fail_closed() {
    let mut oversized = shell_route("oversized", "printf '12345' > large.bin");
    oversized.outputs = vec!["large.bin".into()];
    let bundle = bundle_with_routes(vec![oversized]);
    let mut options = RunOptions::default();
    options.limits.max_single_artifact_bytes = 4;
    let error = run_route(&bundle, "oversized", &options).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ArtifactCaptureError>(),
        Some(ArtifactCaptureError::SingleArtifactLimit {
            path,
            limit: 4,
            observed_at_least: 5,
        }) if path == "large.bin"
    ));

    let mut too_many = shell_route("too-many", ": > a.bin; : > b.bin");
    too_many.outputs = vec!["*.bin".into()];
    let bundle = bundle_with_routes(vec![too_many]);
    let mut options = RunOptions::default();
    options.limits.max_artifact_count = 1;
    let error = run_route(&bundle, "too-many", &options).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ArtifactCaptureError>(),
        Some(ArtifactCaptureError::ArtifactCountLimit {
            limit: 1,
            observed_at_least: 2,
        })
    ));

    let mut aggregate = shell_route("aggregate", "printf 'abc' > a.bin; printf 'def' > b.bin");
    aggregate.outputs = vec!["*.bin".into()];
    let bundle = bundle_with_routes(vec![aggregate]);
    let mut options = RunOptions::default();
    options.limits.max_single_artifact_bytes = 4;
    options.limits.max_aggregate_artifact_bytes = 5;
    let error = run_route(&bundle, "aggregate", &options).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ArtifactCaptureError>(),
        Some(ArtifactCaptureError::AggregateArtifactLimit {
            limit: 5,
            captured_before: 3,
            artifact_bytes: 3,
            ..
        })
    ));
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
        ..RunOptions::default()
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

#[test]
fn project_runtime_selection_output_budget_is_checked_before_launch() {
    let external = tempfile::tempdir().unwrap();
    let marker = external.path().join("must-not-launch");
    let mut first = shell_route("first", "printf launched > \"$MARKER\"");
    let mut second = shell_route("second", "printf launched > \"$MARKER\"");
    for route in [&mut first, &mut second] {
        route
            .environment
            .insert("MARKER".to_string(), marker.to_string_lossy().into_owned());
    }
    let mut bundle = bundle_with_routes(vec![first, second]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["first".into(), "second".into()],
        policy: RoutePolicy::All,
    });
    let mut options = RunOptions::default();
    options.limits.max_retained_stdout_bytes = 8;
    options.limits.max_retained_stderr_bytes = 8;
    options.limits.max_selection_retained_output_bytes = 31;

    let error = run_selection(&bundle, Some("main"), None, &options).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<RouteExecutionError>(),
        Some(RouteExecutionError::Configuration { detail })
            if detail.contains("could retain 32 output bytes")
    ));
    assert!(
        !marker.exists(),
        "selection budget failure launched a route"
    );
}

#[test]
fn project_runtime_selection_budget_counts_prerequisite_results() {
    let external = tempfile::tempdir().unwrap();
    let marker = external.path().join("prerequisite-must-not-launch");
    let mut prerequisite = shell_route("prepare", "printf launched > \"$MARKER\"");
    prerequisite
        .environment
        .insert("MARKER".to_string(), marker.to_string_lossy().into_owned());
    let mut main = shell_route("main", "printf main");
    main.prerequisites.push("prepare".to_string());
    let bundle = bundle_with_routes(vec![prerequisite, main]);
    let mut options = RunOptions::default();
    options.limits.max_retained_stdout_bytes = 8;
    options.limits.max_retained_stderr_bytes = 8;
    options.limits.max_selection_retained_output_bytes = 31;

    let error = run_route(&bundle, "main", &options).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<RouteExecutionError>(),
        Some(RouteExecutionError::Configuration { detail })
            if detail.contains("could retain 32 output bytes")
    ));
    assert!(
        !marker.exists(),
        "prerequisite budget failure launched a route"
    );
}

fn bounded_runtime_options(timeout: Duration) -> RunOptions {
    let mut options = RunOptions::default();
    options.limits.wall_clock_timeout = timeout;
    options.limits.termination_grace_period = Duration::from_millis(50);
    options
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn project_runtime_short_success_preserves_complete_output() {
    let route = shell_route(
        "ordinary",
        "printf 'ordinary-out'; printf 'ordinary-err' >&2",
    );
    let bundle = bundle_with_routes(vec![route]);
    let result = run_route(&bundle, "ordinary", &RunOptions::default()).unwrap();

    assert!(result.succeeded());
    assert_eq!(result.stdout, b"ordinary-out");
    assert_eq!(result.stderr, b"ordinary-err");
    assert_eq!(result.stdout_capture.total_observed_bytes, 12);
    assert_eq!(result.stdout_capture.retained_bytes, 12);
    assert!(!result.stdout_capture.truncated);
    assert_eq!(
        result.stdout_capture.sha256,
        hex::encode(Sha256::digest(b"ordinary-out"))
    );
    assert_eq!(result.stderr_capture.total_observed_bytes, 12);
    assert!(!result.stderr_capture.truncated);
    #[cfg(unix)]
    assert_eq!(
        RunOptions::default().limits.process_tree_policy,
        ProcessTreePolicy::OwnedProcessGroup
    );
}

#[test]
fn project_runtime_clear_environment_is_explicit_and_keeps_route_overlay() {
    let mut route = shell_route(
        "clear-env",
        "printf '%s:%s' \"${HOME-unset}\" \"$DECLARED_VALUE\"",
    );
    route.command[0] = "/bin/sh".to_string();
    route
        .environment
        .insert("DECLARED_VALUE".to_string(), "visible".to_string());
    let bundle = bundle_with_routes(vec![route]);
    let mut options = RunOptions::default();
    options.limits.environment_policy = EnvironmentPolicy::Clear;

    let result = run_route(&bundle, "clear-env", &options).unwrap();
    assert_eq!(result.stdout, b"unset:visible");
}

#[test]
fn project_runtime_rejects_zero_terminality_grace_before_launch() {
    let external = tempfile::tempdir().unwrap();
    let marker = external.path().join("zero-grace-must-not-launch");
    let mut route = shell_route("zero-grace", "printf launched > \"$MARKER\"");
    route
        .environment
        .insert("MARKER".to_string(), marker.to_string_lossy().into_owned());
    let bundle = bundle_with_routes(vec![route]);
    let mut options = RunOptions::default();
    options.limits.termination_grace_period = Duration::ZERO;

    let error = run_route(&bundle, "zero-grace", &options).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<RouteExecutionError>(),
        Some(RouteExecutionError::Configuration { detail })
            if detail.contains("termination grace period must be greater than zero")
    ));
    assert!(!marker.exists(), "zero-grace validation launched a route");
}

#[test]
fn project_runtime_deadline_is_typed_and_terminates_process() {
    let route = shell_route("deadline", "trap '' TERM; sleep 5");
    let bundle = bundle_with_routes(vec![route]);
    let options = bounded_runtime_options(Duration::from_millis(100));
    let started = Instant::now();

    let error = run_route(&bundle, "deadline", &options).unwrap_err();
    assert!(is_timeout_error(&error), "unexpected error: {error:#}");
    assert!(!is_cancellation_error(&error));
    assert!(matches!(
        error.downcast_ref::<RouteExecutionError>(),
        Some(RouteExecutionError::DeadlineExceeded { .. })
    ));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "deadline termination took {:?}",
        started.elapsed()
    );
}

#[test]
fn project_runtime_cancellation_is_distinct_from_deadline() {
    let external = tempfile::tempdir().unwrap();
    let ready = external.path().join("route-ready");
    let mut route = shell_route(
        "cancel",
        "printf ready > \"$READY_MARKER\"; trap '' TERM; sleep 5",
    );
    route.environment.insert(
        "READY_MARKER".to_string(),
        ready.to_string_lossy().into_owned(),
    );
    let bundle = bundle_with_routes(vec![route]);
    let options = bounded_runtime_options(Duration::from_secs(4));
    let cancellation = CancellationToken::new();
    let route_cancellation = cancellation.clone();
    let worker = std::thread::spawn(move || {
        run_route_cancellable(&bundle, "cancel", &options, route_cancellation)
    });

    wait_for_file(&ready, Duration::from_secs(1));
    cancellation.cancel();
    let error = worker.join().unwrap().unwrap_err();
    assert!(is_cancellation_error(&error), "unexpected error: {error:#}");
    assert!(!is_timeout_error(&error));
    assert!(matches!(
        error.downcast_ref::<RouteExecutionError>(),
        Some(RouteExecutionError::Cancelled { .. })
    ));
}

#[cfg(unix)]
#[test]
fn project_runtime_leader_exit_cannot_be_held_open_by_descendant_pipe() {
    let route = shell_route("pipe-holder", "(trap '' HUP TERM; sleep 2) & exit 0");
    let bundle = bundle_with_routes(vec![route]);
    let options = bounded_runtime_options(Duration::from_secs(3));
    let started = Instant::now();

    let result = run_route(&bundle, "pipe-holder", &options).unwrap();
    assert!(result.succeeded());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "descendant-held pipe delayed terminality for {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn project_runtime_success_has_no_live_owned_group_effects() {
    let external = tempfile::tempdir().unwrap();
    let leaked_effect = external.path().join("leaked-effect");
    let descendant_pid = external.path().join("descendant-pid");
    let mut route = shell_route(
        "background",
        "(trap '' HUP TERM; sleep 1; printf leaked > \"$LEAKED_EFFECT\") </dev/null >/dev/null 2>&1 & printf '%s' \"$!\" > \"$DESCENDANT_PID\"; exit 0",
    );
    route.environment.insert(
        "LEAKED_EFFECT".to_string(),
        leaked_effect.to_string_lossy().into_owned(),
    );
    route.environment.insert(
        "DESCENDANT_PID".to_string(),
        descendant_pid.to_string_lossy().into_owned(),
    );
    let bundle = bundle_with_routes(vec![route]);
    let result = run_route(&bundle, "background", &RunOptions::default()).unwrap();

    assert!(result.succeeded());
    assert!(descendant_pid.exists(), "background child never started");
    let descendant_pid = std::fs::read_to_string(&descendant_pid)
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    assert!(
        !process_is_active(descendant_pid),
        "owned descendant remained active after successful settlement"
    );
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(
        !leaked_effect.exists(),
        "owned descendant continued host effects after successful settlement"
    );
}

#[cfg(target_os = "macos")]
fn process_is_active(pid: libc::pid_t) -> bool {
    // SAFETY: `information` is writable storage of the exact size supplied to
    // libproc; failure is handled as disappearance or a test failure.
    let mut information: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).unwrap();
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut information as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read == 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return false;
    }
    assert_eq!(read, size, "failed to inspect descendant process state");
    information.pbi_status != libc::SZOMB
}

#[cfg(target_os = "linux")]
fn process_is_active(pid: libc::pid_t) -> bool {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => panic!("failed to inspect descendant process state: {error}"),
    };
    let close = stat
        .rfind(')')
        .expect("descendant /proc stat has a command terminator");
    stat[close + 1..].split_whitespace().next() != Some("Z")
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn process_is_active(pid: libc::pid_t) -> bool {
    // SAFETY: signal zero only probes for a live PID.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn project_runtime_output_retention_is_bounded_but_fully_drained() {
    const ITERATIONS: usize = 20_000;
    const CHUNK: &[u8] = b"0123456789abcdef";
    let route = shell_route(
        "flood",
        "i=0; while [ \"$i\" -lt 20000 ]; do printf '0123456789abcdef'; i=$((i + 1)); done; i=0; while [ \"$i\" -lt 20000 ]; do printf '0123456789abcdef' >&2; i=$((i + 1)); done",
    );
    let bundle = bundle_with_routes(vec![route]);
    let mut options = bounded_runtime_options(Duration::from_secs(15));
    options.limits.max_retained_stdout_bytes = 1_024;
    options.limits.max_retained_stderr_bytes = 2_048;
    let expected = CHUNK.repeat(ITERATIONS);

    let result = run_route(&bundle, "flood", &options).unwrap();
    assert!(result.succeeded(), "stderr: {}", result.stderr_text());
    assert_eq!(result.stdout, expected[..1_024]);
    assert_eq!(result.stderr, expected[..2_048]);
    for (capture, retained) in [
        (&result.stdout_capture, 1_024_u64),
        (&result.stderr_capture, 2_048_u64),
    ] {
        assert_eq!(capture.total_observed_bytes, expected.len() as u64);
        assert_eq!(capture.retained_bytes, retained);
        assert!(capture.truncated);
        assert_eq!(capture.sha256, hex::encode(Sha256::digest(&expected)));
    }
}

#[test]
fn project_runtime_verify_equivalent_uses_complete_truncated_streams() {
    let a = shell_route("prefix-a", "printf 'same-A'");
    let b = shell_route("prefix-b", "printf 'same-B'");
    let mut bundle = bundle_with_routes(vec![a, b]);
    bundle.route_sets.push(RouteSet {
        provides: "main".into(),
        alternatives: vec!["prefix-a".into(), "prefix-b".into()],
        policy: RoutePolicy::VerifyEquivalent,
    });
    let mut options = RunOptions::default();
    options.limits.max_retained_stdout_bytes = 4;

    let error = run_selection(&bundle, Some("main"), None, &options).unwrap_err();
    assert!(
        error.to_string().contains("different stdout"),
        "truncated prefixes were treated as equivalent: {error:#}"
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

    // A long adversarial pattern must have bounded stack use and deterministic
    // wildcard semantics; the matcher is iterative rather than recursive.
    let repeated = "*a".repeat(4_096);
    assert!(glob_match(&format!("{repeated}*"), &"a".repeat(4_096)));
    assert!(!glob_match(&format!("{repeated}b"), &"a".repeat(4_096)));
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
