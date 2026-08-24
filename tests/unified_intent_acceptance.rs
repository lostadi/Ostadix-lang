//! Acceptance boundaries for the task-oriented Ostadix intent front door.
//!
//! These tests stay below the CLI grammar and exercise the reusable API that
//! performs exact input classification, preflight, static planning, and engine
//! selection.  Process dispatch itself remains covered by
//! `test_o_cli_dispatch.py`.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use o_lang::hosted_remote::project_mesh::{MeshExecutionConfig, MeshRequirement};
use o_lang::intent::{
    execute_prepared_ordinary_o, execute_prepared_project, live_placement_preview,
    prepare_execution_intent, render_ordinary_static_plan, IntentInputKindV1, LocalOExecutorV1,
    PrepareExecutionOptionsV1, PreparedExecutionIntentV1, ProjectExecutorV1,
};

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn options(shim_dir: &Path) -> PrepareExecutionOptionsV1 {
    PrepareExecutionOptionsV1 {
        shim_dir: shim_dir.to_path_buf(),
        ..PrepareExecutionOptionsV1::default()
    }
}

fn one_route_project(root: &Path) {
    write(
        root.join("payload.txt").as_path(),
        b"source-closed payload\n",
    );
    write(
        root.join("olang.project.toml").as_path(),
        br#"[project]
name = "unified-intent-one-route"
default_route = "main"

[[routes]]
id = "main"
label = "local acceptance route"
kind = "shell"
command = ["sh", "-c", "printf 'intent-local-ok\\n'"]
default = true
pure = true
guards = { requires_command = "sh" }
"#,
    );
}

fn ambiguous_project(root: &Path, execution_marker: &Path) {
    let marker = execution_marker.to_string_lossy();
    write(root.join("payload.txt").as_path(), b"ambiguous source\n");
    write(
        root.join("olang.project.toml").as_path(),
        format!(
            r#"[project]
name = "unified-intent-ambiguous"

[[routes]]
id = "left"
label = "left candidate"
kind = "shell"
command = ["sh", "-c", "printf left > '{marker}'"]

[[routes]]
id = "right"
label = "right candidate"
kind = "shell"
command = ["sh", "-c", "printf right > '{marker}'"]
"#
        ),
    );
}

fn environment_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvironmentRestore {
    values: Vec<(&'static str, Option<OsString>)>,
}

impl EnvironmentRestore {
    fn set(values: &[(&'static str, OsString)]) -> Self {
        let prior = values
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect();
        for (name, value) in values {
            std::env::set_var(name, value);
        }
        Self { values: prior }
    }

    fn unset(name: &'static str) -> Self {
        let prior = std::env::var_os(name);
        std::env::remove_var(name);
        Self {
            values: vec![(name, prior)],
        }
    }
}

impl Drop for EnvironmentRestore {
    fn drop(&mut self) {
        for (name, value) in self.values.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

#[test]
fn lifted_bundle_is_classified_before_ordinary_o_parsing() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    one_route_project(&project);

    let bundle = o_lang::project::assemble(&project, "lifted-acceptance", &[]).unwrap();
    let lifted = o_lang::project::lower::lower_to_o_validated(&bundle).unwrap();
    let lifted_path = temp.path().join("lifted-project.O");
    write(&lifted_path, lifted);

    let prepared = prepare_execution_intent(&lifted_path, options(temp.path())).unwrap();
    assert!(prepared.static_plan().contains("LogicalHGraphV1"));
    let PreparedExecutionIntentV1::Project(project) = prepared else {
        panic!("embedded project bundle was misclassified as ordinary O")
    };
    assert_eq!(project.input_kind, IntentInputKindV1::LiftedProject);
    assert_eq!(project.selected_target, "main");
}

#[test]
fn standalone_foreign_file_fails_with_deterministic_o_link_guidance() {
    let temp = tempfile::tempdir().unwrap();
    let foreign = temp.path().join("standalone.py");
    write(&foreign, b"print('not yet bundled')\n");

    let error = prepare_execution_intent(&foreign, options(temp.path()))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unsupported standalone foreign file"),
        "{error}"
    );
    assert!(
        error.contains("o-link --project <DIRECTORY> -o project.O"),
        "{error}"
    );
}

#[test]
fn ambiguous_project_preflight_prints_routes_without_executing() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let marker = temp.path().join("route-executed");
    fs::create_dir(&project).unwrap();
    ambiguous_project(&project, &marker);

    let error = prepare_execution_intent(&project, options(temp.path()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no unambiguous default route"), "{error}");
    assert!(error.contains("--route <ID>"), "{error}");
    assert!(error.contains("left"), "{error}");
    assert!(error.contains("right"), "{error}");
    assert!(!marker.exists(), "preflight executed an ambiguous route");
}

#[test]
fn ordinary_parallel_auto_is_local_only_even_with_poisoned_mesh_state() {
    let _serial = environment_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("local-only.O");
    write(&program, b"text^(local-only-ok)_text\n");

    let poisoned_state = temp.path().join("state-is-a-file");
    write(&poisoned_state, b"must remain untouched");
    let poison_bin = temp.path().join("bin");
    fs::create_dir(&poison_bin).unwrap();
    let node_marker = temp.path().join("o-node-was-started");
    let poison_node = poison_bin.join("o-node");
    write(
        &poison_node,
        b"#!/bin/sh\nprintf invoked > \"$O_NODE_POISON_MARKER\"\nexit 99\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&poison_node, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let prior_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![poison_bin.clone()];
    path_entries.extend(std::env::split_paths(&prior_path));
    let poisoned_path = std::env::join_paths(path_entries).unwrap();
    let _environment = EnvironmentRestore::set(&[
        ("XDG_STATE_HOME", poisoned_state.clone().into_os_string()),
        ("O_LANG_NODE_BIN", poison_node.clone().into_os_string()),
        ("O_NODE_POISON_MARKER", node_marker.clone().into_os_string()),
        ("PATH", poisoned_path),
    ]);

    let mut run_options = options(temp.path());
    run_options.parallel_auto = true;
    run_options.local_workers = Some(2);
    let prepared = prepare_execution_intent(&program, run_options).unwrap();
    let preview = live_placement_preview(&prepared).unwrap();
    assert!(preview.candidates.is_empty());
    assert!(preview
        .explanation
        .iter()
        .any(|line| line.contains("discovery and remote RPCs were not performed")));

    let PreparedExecutionIntentV1::OrdinaryO(ordinary) = prepared else {
        panic!("ordinary source was misclassified")
    };
    assert_eq!(ordinary.executor, LocalOExecutorV1::ForcedGraph);
    let outcome = execute_prepared_ordinary_o(&ordinary).unwrap();
    assert_eq!(outcome.value, o_lang::value::OValue::text("local-only-ok"));
    assert_eq!(fs::read(&poisoned_state).unwrap(), b"must remain untouched");
    assert!(!node_marker.exists(), "ordinary auto launched o-node");
}

#[test]
fn project_auto_means_mesh_prefer_and_conflicts_with_explicit_mesh() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    one_route_project(&project);

    let mut auto = options(temp.path());
    auto.parallel_auto = true;
    let prepared = prepare_execution_intent(&project, auto).unwrap();
    let PreparedExecutionIntentV1::Project(prepared) = prepared else {
        panic!("project directory was misclassified")
    };
    assert_eq!(prepared.executor, ProjectExecutorV1::MeshPrefer);
    assert_eq!(
        prepared.mesh.as_ref().map(|mesh| mesh.requirement),
        Some(MeshRequirement::Prefer)
    );

    let mut conflicting = options(temp.path());
    conflicting.parallel_auto = true;
    conflicting.explicit_mesh = true;
    let required = MeshExecutionConfig {
        requirement: MeshRequirement::Required,
        ..MeshExecutionConfig::default()
    };
    conflicting.mesh = Some(required);
    let error = prepare_execution_intent(&project, conflicting)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--parallel auto conflicts with explicit --mesh"));
    assert!(error.contains("--mesh=required"));
}

#[test]
fn static_plan_matches_olangc_exactly_and_does_not_open_run_state() {
    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join("static-plan.O");
    let source = "text^(static-plan)_text\n";
    write(&program, source);
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();

    let prepared = prepare_execution_intent(&program, options(temp.path())).unwrap();
    let expected = render_ordinary_static_plan(source).unwrap();
    assert_eq!(prepared.static_plan(), expected);
    assert_eq!(fs::read_dir(&state).unwrap().count(), 0);

    let output = Command::new(env!("CARGO_BIN_EXE_olangc"))
        .arg(&program)
        .args(["--target", "ir"])
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "olangc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, expected.as_bytes());
    assert_eq!(fs::read_dir(&state).unwrap().count(), 0);
}

#[test]
fn local_project_preserves_compatibility_default_and_hgraph_override() {
    let _serial = environment_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    one_route_project(&project);

    let _unset = EnvironmentRestore::unset("O_PROJECT_EXECUTOR");
    let compatibility = prepare_execution_intent(&project, options(temp.path())).unwrap();
    let PreparedExecutionIntentV1::Project(compatibility) = compatibility else {
        panic!("project directory was misclassified")
    };
    assert_eq!(compatibility.executor, ProjectExecutorV1::Compatibility);
    let compatibility_outcome = execute_prepared_project(&compatibility).unwrap();
    assert_eq!(compatibility_outcome.results.len(), 1);
    assert!(compatibility_outcome.results[0].succeeded());
    assert!(compatibility_outcome.project_trace.is_none());
    assert!(compatibility_outcome.trace_unavailable_reason.is_some());

    std::env::set_var("O_PROJECT_EXECUTOR", "hgraph");
    let hgraph = prepare_execution_intent(&project, options(temp.path())).unwrap();
    let PreparedExecutionIntentV1::Project(hgraph) = hgraph else {
        panic!("project directory was misclassified")
    };
    assert_eq!(hgraph.executor, ProjectExecutorV1::Hgraph);
    let hgraph_outcome = execute_prepared_project(&hgraph).unwrap();
    assert_eq!(hgraph_outcome.results.len(), 1);
    assert!(hgraph_outcome.results[0].succeeded());
    assert!(hgraph_outcome.project_trace.is_some());
    assert!(hgraph_outcome.trace_unavailable_reason.is_none());
}
