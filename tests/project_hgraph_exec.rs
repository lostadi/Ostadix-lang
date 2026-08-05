//! PR8A single-branch hosted Project HGraph execution.
//!
//! This corpus is deliberately bounded to one resolved Explicit/Default
//! alternative. It is not multipath, retry, placement, governed, attested,
//! exactly-once, native, O-core, or World G1 evidence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use o_lang::effects::ResourceKey;
use o_lang::hgraph::{ExecutableOp, HNode, HNodeKind, ReadySchedule};
use o_lang::project::runtime::{run_route, RunOptions};
use o_lang::project::{
    self, build_project_hgraph, execute_project_hgraph, OExecutionResult, ProjectAttemptIdentity,
    ProjectAttemptState, ProjectBundle, ProjectExecutionError, ProjectExecutionOutcome,
    ProjectHGraph, RouteExecutionDisposition, RouteGuard, RoutePolicy, RouteProvenance, RouteSet,
    RouteSpec,
};
use o_lang::value::OValue;
use sha2::{Digest, Sha256};

const EXPECTED_STDOUT: &[u8] = b"{\"prepared\":true,\"result\":\"project-hgraph\"}\n";
const EXPECTED_STDERR: &[u8] = b"project-hgraph-stderr\n";

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph_exec")
}

fn fixture_bundle(poison: &Path) -> ProjectBundle {
    let mut bundle = project::assemble(&fixture_path(), "pr8a-project-hgraph-exec", &[]).unwrap();
    for route in &mut bundle.routes {
        route.environment.insert(
            "PR8A_EXTERNAL_POISON_MARKER".to_string(),
            poison.to_string_lossy().into_owned(),
        );
    }
    bundle
}

fn explicit_project(bundle: &ProjectBundle) -> ProjectHGraph {
    build_project_hgraph(bundle, None, None).unwrap()
}

fn execute_explicit(bundle: &ProjectBundle) -> (ProjectHGraph, ProjectExecutionOutcome) {
    let project = explicit_project(bundle);
    let outcome = execute_project_hgraph(bundle, &project, &RunOptions::default()).unwrap();
    (project, outcome)
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_cli_trace(path: &Path) -> serde_json::Value {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("failed to read trace {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("trace {} is not valid JSON: {error}", path.display()))
}

fn assert_unsigned_diagnostic_trace(trace: &serde_json::Value) {
    let root = trace.as_object().expect("trace root must be a JSON object");
    assert_eq!(root.len(), 3, "unexpected trace root fields: {root:?}");
    assert_eq!(trace["format_version"], 2);
    assert!(trace["header"].is_object());
    let events = trace["events"]
        .as_array()
        .expect("trace events must be an array");
    assert!(!events.is_empty(), "trace must contain coordinator events");
    for (expected, event) in events.iter().enumerate() {
        assert_eq!(
            event["coordinator_ordinal"].as_u64(),
            Some(expected as u64),
            "trace ordinals must be contiguous"
        );
    }

    let encoded = serde_json::to_string(trace).unwrap();
    for forbidden in ["signature", "signed_receipt", "owreceipt", "attestation"] {
        assert!(
            !encoded.to_ascii_lowercase().contains(forbidden),
            "diagnostic trace unexpectedly contains `{forbidden}`"
        );
    }
}

fn assert_sha256_json(value: &serde_json::Value, label: &str) {
    let digest = value
        .as_str()
        .unwrap_or_else(|| panic!("{label} must be a JSON string"));
    assert_eq!(digest.len(), 64, "{label} must be a SHA-256 digest");
    assert!(
        digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} is not hexadecimal: {digest}"
    );
}

fn assert_isolated(result: &OExecutionResult, poison: &Path) {
    let fixture = fixture_path().canonicalize().unwrap();
    assert_ne!(result.provenance.workspace, fixture);
    assert_eq!(result.provenance.cwd, result.provenance.workspace);
    assert!(
        result
            .provenance
            .workspace
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("olang-ws-")),
        "unexpected workspace: {}",
        result.provenance.workspace.display()
    );
    assert!(
        !result.provenance.workspace.exists(),
        "coordinator retained temporary workspace {}",
        result.provenance.workspace.display()
    );
    assert!(
        !poison.exists(),
        "route executed outside its isolated workspace: {}",
        poison.display()
    );
}

fn assert_expected_result(result: &OExecutionResult) {
    assert_eq!(result.route_id, "main");
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.disposition, RouteExecutionDisposition::Executed);
    assert_eq!(result.stdout, EXPECTED_STDOUT);
    assert_eq!(result.stderr, EXPECTED_STDERR);
    assert_eq!(
        result.value,
        Some(serde_json::json!({
            "prepared": true,
            "result": "project-hgraph",
        }))
    );
    assert_eq!(
        result
            .artifacts
            .iter()
            .map(|artifact| artifact.path.as_str())
            .collect::<Vec<_>>(),
        ["result.txt"]
    );
    assert_eq!(result.artifacts[0].bytes_len, 22);
    assert_eq!(
        result.artifacts[0].content_hash,
        sha256(b"project-hgraph-result\n")
    );
}

#[test]
fn explicit_execution_runs_prerequisite_once_in_one_isolated_workspace() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("outside-workspace-poison");
    let audit_log = external.path().join("workspace-audit.log");
    let mut bundle = fixture_bundle(&poison);
    for route in &mut bundle.routes {
        route.environment.insert(
            "PR8A_WORKSPACE_AUDIT_LOG".into(),
            audit_log.to_string_lossy().into_owned(),
        );
    }
    let (_project, outcome) = execute_explicit(&bundle);

    assert_expected_result(&outcome.result);
    assert_isolated(&outcome.result, &poison);

    let prepare_runs = outcome
        .trace
        .events()
        .iter()
        .filter(|event| {
            event.operation_label == "run-route:prepare"
                && event.state == ProjectAttemptState::SettledSuccess
        })
        .count();
    assert_eq!(prepare_runs, 1, "prerequisite must execute exactly once");

    let prepare_terminal = outcome
        .trace
        .events()
        .iter()
        .find(|event| {
            event.operation_label == "run-route:prepare"
                && event.state == ProjectAttemptState::SettledSuccess
        })
        .unwrap();
    let prepared_artifacts = &prepare_terminal.outcome.as_ref().unwrap().artifacts;
    assert_eq!(
        prepared_artifacts
            .iter()
            .map(|artifact| artifact.path.as_str())
            .collect::<Vec<_>>(),
        ["prepare-count.txt", "prepared.txt"]
    );

    let audit = std::fs::read_to_string(&audit_log).unwrap();
    let entries = audit
        .lines()
        .map(|line| line.split_once('\t').unwrap())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2, "unexpected route executions: {audit:?}");
    assert_eq!(entries[0].0, "prepare");
    assert_eq!(entries[1].0, "main");
    assert_eq!(entries[0].1, entries[1].1, "routes changed workspace");
    assert_eq!(
        Path::new(entries[0].1),
        outcome.result.provenance.workspace,
        "trace/result workspace differs from the executed branch workspace"
    );
}

#[test]
fn default_policy_executes_its_sole_resolved_alternative() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("default-outside-workspace-poison");
    let bundle = fixture_bundle(&poison);
    let project = build_project_hgraph(&bundle, Some("application"), None).unwrap();
    assert_eq!(project.plan.policy, RoutePolicy::Default);
    assert_eq!(project.plan.alternatives, ["main"]);

    let outcome = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap();
    assert_expected_result(&outcome.result);
    assert_isolated(&outcome.result, &poison);
}

#[test]
fn deeper_prerequisite_chain_uses_the_same_branch_workspace() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("deep-chain-outside-workspace-poison");
    let audit_log = external.path().join("deep-chain-workspace-audit.log");
    let mut bundle = fixture_bundle(&poison);

    let mut bootstrap = RouteSpec::new("bootstrap", RouteProvenance::CliOverride);
    bootstrap.command = vec![
        "sh".into(),
        "-c".into(),
        r#"set -eu
case "$PWD" in
  */olang-ws-*) ;;
  *) printf '%s\n' outside > "$PR8A_EXTERNAL_POISON_MARKER"; exit 93 ;;
esac
printf '%s\n' bootstrap > bootstrap.txt
printf 'bootstrap\t%s\n' "$PWD" >> "$PR8A_WORKSPACE_AUDIT_LOG""#
            .into(),
    ];
    bootstrap.outputs = vec!["bootstrap.txt".into()];
    bootstrap.environment.insert(
        "PR8A_EXTERNAL_POISON_MARKER".into(),
        poison.to_string_lossy().into_owned(),
    );
    bootstrap.environment.insert(
        "PR8A_WORKSPACE_AUDIT_LOG".into(),
        audit_log.to_string_lossy().into_owned(),
    );
    bundle.routes.push(bootstrap);

    for route in &mut bundle.routes {
        route.environment.insert(
            "PR8A_WORKSPACE_AUDIT_LOG".into(),
            audit_log.to_string_lossy().into_owned(),
        );
    }
    let prepare = bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "prepare")
        .unwrap();
    prepare.prerequisites = vec!["bootstrap".into()];
    prepare.command[2] = format!(
        "set -eu\ntest \"$(cat bootstrap.txt)\" = bootstrap\n{}",
        prepare.command[2]
    );

    let project = explicit_project(&bundle);
    let outcome = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap();
    assert_expected_result(&outcome.result);
    assert_isolated(&outcome.result, &poison);

    let audit = std::fs::read_to_string(&audit_log).unwrap();
    let entries = audit
        .lines()
        .map(|line| line.split_once('\t').unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        entries.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        ["bootstrap", "prepare", "main"]
    );
    assert!(
        entries.iter().all(|entry| entry.1 == entries[0].1),
        "deep prerequisite chain changed branch workspaces: {audit:?}"
    );
}

#[test]
fn coordinator_rejects_source_and_projection_substitution_before_execution() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("validation-outside-workspace-poison");
    let execution_marker = external.path().join("validation-route-executed");
    let mut bundle = fixture_bundle(&poison);
    for route in &mut bundle.routes {
        route.environment.insert(
            "PR8A_EXECUTION_MARKER".into(),
            execution_marker.to_string_lossy().into_owned(),
        );
    }

    let exact = explicit_project(&bundle);
    exact.validate_source(&bundle, None, None).unwrap();

    let mut substituted = bundle.clone();
    substituted
        .routes
        .iter_mut()
        .find(|route| route.id == "main")
        .unwrap()
        .arguments
        .push("substituted-argument".to_string());
    let source_error =
        execute_project_hgraph(&substituted, &exact, &RunOptions::default()).unwrap_err();
    let source_message = format!("{source_error:#}");
    assert!(
        source_message.contains("supplied bundle") || source_message.contains("source"),
        "unexpected source validation error: {source_error:#}"
    );

    let mut forged = explicit_project(&bundle);
    forged
        .graph
        .add_node(HNode::with_value(OValue::text("forged-project-value")));
    let projection_error =
        execute_project_hgraph(&bundle, &forged, &RunOptions::default()).unwrap_err();
    let projection_message = format!("{projection_error:#}");
    assert!(
        projection_message.contains("projection")
            || projection_message.contains("inventory differs"),
        "unexpected projection validation error: {projection_error:#}"
    );
    assert!(!poison.exists());
    assert!(
        !execution_marker.exists(),
        "source/projection validation occurred after route execution"
    );
}

#[test]
fn failed_prerequisite_blocks_the_main_route() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("failed-prerequisite-outside-poison");
    let main_marker = external.path().join("main-executed");
    let mut bundle = fixture_bundle(&poison);
    let prepare = bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "prepare")
        .unwrap();
    prepare.command = vec!["sh".into(), "-c".into(), "exit 23".into()];
    let main = bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "main")
        .unwrap();
    main.command = vec![
        "sh".into(),
        "-c".into(),
        "printf '%s\\n' ran > \"$PR8A_MAIN_EXECUTED_MARKER\"".into(),
    ];
    main.environment.insert(
        "PR8A_MAIN_EXECUTED_MARKER".into(),
        main_marker.to_string_lossy().into_owned(),
    );

    let project = explicit_project(&bundle);
    let prepare_run = project
        .plan
        .operations
        .iter()
        .find(|operation| {
            matches!(
                &operation.op,
                ExecutableOp::RunRoute { route_id } if route_id == "prepare"
            )
        })
        .unwrap();
    let prepare_value = project.graph.op_for(prepare_run.id).unwrap().value_output;
    let prepare_completion = project.graph.completion_node(prepare_run.id).unwrap();
    let error = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("prepare") || message.contains("stalled"),
        "imprecise prerequisite failure: {error:#}"
    );
    let failed_attempt = error
        .downcast_ref::<ProjectExecutionError>()
        .expect("coordinator stall must retain its ProjectAttemptTrace");
    let prepare_lifecycle = failed_attempt
        .trace
        .events()
        .iter()
        .filter(|event| event.operation_label == "run-route:prepare")
        .map(|event| event.state)
        .collect::<Vec<_>>();
    assert_eq!(
        prepare_lifecycle,
        [
            ProjectAttemptState::Ready,
            ProjectAttemptState::Started,
            ProjectAttemptState::SettledFailure,
        ]
    );
    let settled_prepare = failed_attempt
        .trace
        .events()
        .iter()
        .find(|event| {
            event.operation_label == "run-route:prepare"
                && event.state == ProjectAttemptState::SettledFailure
        })
        .unwrap();
    assert_eq!(
        settled_prepare
            .outcome
            .as_ref()
            .expect("settled route must retain its normalized outcome")
            .exit_code,
        Some(23)
    );
    assert_eq!(
        failed_attempt
            .settled_result(prepare_value)
            .expect("nonzero prerequisite must publish its ordinary result")
            .exit_code,
        Some(23)
    );
    assert!(failed_attempt.is_materialized(prepare_value));
    assert!(!failed_attempt.is_failed(prepare_value));
    assert!(!failed_attempt.is_materialized(prepare_completion));
    assert!(
        failed_attempt.is_failed(prepare_completion),
        "nonzero prerequisite must withhold its success-completion token"
    );
    assert!(!failed_attempt.trace.events().iter().any(|event| {
        event.operation_label == "run-route:main"
            && matches!(
                event.state,
                ProjectAttemptState::Started
                    | ProjectAttemptState::Finished
                    | ProjectAttemptState::SettledSuccess
                    | ProjectAttemptState::SettledFailure
                    | ProjectAttemptState::Skipped
                    | ProjectAttemptState::Aborted
            )
    }));
    assert!(
        !main_marker.exists(),
        "main ran after its prerequisite failed"
    );
    assert!(!poison.exists());
}

#[test]
fn nonzero_settlement_advances_the_hostworld_successor() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("nonzero-hostworld-outside-poison");
    let mut bundle = fixture_bundle(&poison);
    bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "prepare")
        .unwrap()
        .command = vec!["sh".into(), "-c".into(), "exit 31".into()];

    let project = explicit_project(&bundle);
    let prepare_run = project
        .plan
        .operations
        .iter()
        .find(|operation| {
            matches!(
                &operation.op,
                ExecutableOp::RunRoute { route_id } if route_id == "prepare"
            )
        })
        .unwrap();
    let prepare_info = project.graph.op_for(prepare_run.id).unwrap();
    let hostworld_successors = prepare_info
        .outputs
        .iter()
        .copied()
        .filter(|output| {
            matches!(
                project.graph.node(*output).map(|node| &node.kind),
                Some(HNodeKind::ResourceState {
                    resource: ResourceKey::HostWorld,
                    ..
                })
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        hostworld_successors.len(),
        1,
        "RunRoute must have one conservative HostWorld successor"
    );

    let error = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap_err();
    let failed_attempt = error
        .downcast_ref::<ProjectExecutionError>()
        .expect("blocked successor must retain coordinator state");
    for successor in hostworld_successors {
        assert!(
            failed_attempt.is_materialized(successor),
            "nonzero settlement must advance HostWorld because effects may have occurred"
        );
        assert!(!failed_attempt.is_failed(successor));
    }
    assert!(!poison.exists());
}

#[test]
fn infrastructure_abort_publishes_no_route_outputs() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("abort-outside-workspace-poison");
    let mut bundle = fixture_bundle(&poison);
    let main = bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "main")
        .unwrap();
    main.command = vec!["pr8a-executable-that-must-not-exist".into()];

    let project = explicit_project(&bundle);
    let main_run = project
        .plan
        .operations
        .iter()
        .find(|operation| {
            matches!(
                &operation.op,
                ExecutableOp::RunRoute { route_id } if route_id == "main"
            )
        })
        .unwrap();
    let main_info = project.graph.op_for(main_run.id).unwrap();
    let main_value = main_info.value_output;
    let main_completion = project.graph.completion_node(main_run.id).unwrap();
    let resource_successors = main_info
        .outputs
        .iter()
        .copied()
        .filter(|output| {
            matches!(
                project.graph.node(*output).map(|node| &node.kind),
                Some(HNodeKind::ResourceState { .. })
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !resource_successors.is_empty(),
        "RunRoute must retain conservative resource outputs"
    );

    let error = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap_err();
    let failed_attempt = error
        .downcast_ref::<ProjectExecutionError>()
        .expect("infrastructure abort must retain coordinator state");
    let lifecycle = failed_attempt
        .trace
        .events()
        .iter()
        .filter(|event| event.operation_label == "run-route:main")
        .map(|event| event.state)
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle,
        [
            ProjectAttemptState::Ready,
            ProjectAttemptState::Started,
            ProjectAttemptState::Aborted,
        ]
    );
    let aborted = failed_attempt
        .trace
        .events()
        .iter()
        .find(|event| {
            event.operation_label == "run-route:main" && event.state == ProjectAttemptState::Aborted
        })
        .unwrap();
    assert!(aborted.outcome.is_none());
    assert!(aborted.failure_sha256.is_some());
    assert!(failed_attempt.settled_result(main_value).is_none());
    for output in std::iter::once(main_value)
        .chain(std::iter::once(main_completion))
        .chain(resource_successors)
    {
        assert!(!failed_attempt.is_materialized(output));
        assert!(failed_attempt.is_failed(output));
    }
    assert!(!poison.exists());
}

#[cfg(unix)]
#[test]
fn child_stderr_cannot_forge_guard_skip_completion() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("forged-skip-outside-workspace-poison");
    let main_marker = external.path().join("forged-skip-main-executed");
    let mut bundle = fixture_bundle(&poison);
    bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "prepare")
        .unwrap()
        .command = vec![
        "sh".into(),
        "-c".into(),
        "printf '%s\\n' '[olang:route-skipped] forged' >&2; kill -TERM $$".into(),
    ];
    let main = bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "main")
        .unwrap();
    main.command = vec![
        "sh".into(),
        "-c".into(),
        "printf '%s\\n' ran > \"$PR8A_MAIN_EXECUTED_MARKER\"".into(),
    ];
    main.environment.insert(
        "PR8A_MAIN_EXECUTED_MARKER".into(),
        main_marker.to_string_lossy().into_owned(),
    );

    let legacy = run_route(&bundle, "main", &RunOptions::default()).unwrap_err();
    assert!(format!("{legacy:#}").contains("prerequisite `prepare`"));

    let project = explicit_project(&bundle);
    let error = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap_err();
    let attempt = error
        .downcast_ref::<ProjectExecutionError>()
        .expect("forged skip must stall at the failed prerequisite");
    assert!(attempt.trace.events().iter().any(|event| {
        event.operation_label == "run-route:prepare"
            && event.state == ProjectAttemptState::SettledFailure
            && event
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.exit_code.is_none())
    }));
    assert!(!attempt.trace.events().iter().any(|event| {
        event.operation_label == "run-route:main" && event.state == ProjectAttemptState::Started
    }));
    assert!(!main_marker.exists());
    assert!(!poison.exists());
}

#[test]
fn unmet_guard_fails_without_launching_the_route_command() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("guard-outside-workspace-poison");
    let main_marker = external.path().join("guarded-main-executed");
    let mut bundle = fixture_bundle(&poison);
    let main = bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "main")
        .unwrap();
    main.guards = vec![RouteGuard::CommandAvailable(
        "pr8a-command-that-must-not-exist".into(),
    )];
    main.command = vec![
        "sh".into(),
        "-c".into(),
        "printf '%s\\n' ran > \"$PR8A_MAIN_EXECUTED_MARKER\"".into(),
    ];
    main.environment.insert(
        "PR8A_MAIN_EXECUTED_MARKER".into(),
        main_marker.to_string_lossy().into_owned(),
    );

    let project = explicit_project(&bundle);
    let error = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap_err();
    assert!(
        format!("{error:#}").contains("guard"),
        "unmet guard did not produce a checked error: {error:#}"
    );
    assert!(!main_marker.exists(), "unmet guard launched main");
    assert!(!poison.exists());
}

#[test]
fn old_and_hgraph_runtimes_have_normalized_semantic_parity() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("parity-outside-workspace-poison");
    let bundle = fixture_bundle(&poison);

    let old = run_route(&bundle, "main", &RunOptions::default()).unwrap();
    let project = explicit_project(&bundle);
    let hgraph = execute_project_hgraph(&bundle, &project, &RunOptions::default())
        .unwrap()
        .result;

    assert_eq!(old.route_id, hgraph.route_id);
    assert_eq!(old.exit_code, hgraph.exit_code);
    assert_eq!(old.stdout, hgraph.stdout);
    assert_eq!(old.stderr, hgraph.stderr);
    assert_eq!(old.value, hgraph.value);
    assert_eq!(old.artifacts, hgraph.artifacts);
    assert_eq!(old.disposition, hgraph.disposition);
    assert_eq!(old.provenance.command, hgraph.provenance.command);
    assert!(!poison.exists());
}

#[test]
fn selected_terminal_nonzero_is_a_result_with_legacy_parity() {
    let external = tempfile::tempdir().unwrap();
    let poison = external
        .path()
        .join("selected-nonzero-outside-workspace-poison");
    let mut bundle = fixture_bundle(&poison);
    let main = bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "main")
        .unwrap();
    main.command[2].push_str("\nexit 7\n");

    let old = run_route(&bundle, "main", &RunOptions::default()).unwrap();
    let project = explicit_project(&bundle);
    let hgraph = execute_project_hgraph(&bundle, &project, &RunOptions::default())
        .expect("a selected nonzero route is a settled result, not an infrastructure error");

    assert_eq!(old.exit_code, Some(7));
    assert_eq!(hgraph.result.exit_code, Some(7));
    assert_eq!(old.route_id, hgraph.result.route_id);
    assert_eq!(old.stdout, hgraph.result.stdout);
    assert_eq!(old.stderr, hgraph.result.stderr);
    assert_eq!(old.value, hgraph.result.value);
    assert_eq!(old.artifacts, hgraph.result.artifacts);
    assert_eq!(old.disposition, hgraph.result.disposition);
    assert_eq!(old.provenance.command, hgraph.result.provenance.command);

    let main_terminal = hgraph
        .trace
        .events()
        .iter()
        .find(|event| event.operation_label == "run-route:main" && event.state.is_terminal())
        .unwrap();
    assert_eq!(main_terminal.state, ProjectAttemptState::SettledFailure);
    assert_eq!(main_terminal.outcome.as_ref().unwrap().exit_code, Some(7));
    assert!(hgraph.trace.events().iter().any(|event| {
        event.operation_label.starts_with("select-route:")
            && event.state == ProjectAttemptState::Finished
    }));
    assert!(!poison.exists());
}

#[test]
fn guard_skip_has_normalized_legacy_parity() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("skip-parity-outside-workspace-poison");
    let mut bundle = fixture_bundle(&poison);
    bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "main")
        .unwrap()
        .guards = vec![RouteGuard::CommandAvailable(
        "pr8a-command-that-must-not-exist".into(),
    )];
    let opts = RunOptions {
        guard_behavior: o_lang::project::runtime::GuardBehavior::Skip,
        inherit_env: true,
    };

    let old = run_route(&bundle, "main", &opts).unwrap();
    let project = explicit_project(&bundle);
    let hgraph = execute_project_hgraph(&bundle, &project, &opts).unwrap();
    assert_eq!(old.route_id, hgraph.result.route_id);
    assert_eq!(old.exit_code, hgraph.result.exit_code);
    assert_eq!(old.stdout, hgraph.result.stdout);
    assert_eq!(old.stderr, hgraph.result.stderr);
    assert_eq!(old.value, hgraph.result.value);
    assert_eq!(old.artifacts, hgraph.result.artifacts);
    assert_eq!(old.disposition, hgraph.result.disposition);
    assert_eq!(
        hgraph.result.disposition,
        RouteExecutionDisposition::GuardSkipped
    );
    assert!(hgraph.trace.events().iter().any(|event| {
        event.operation_label == "run-route:main" && event.state == ProjectAttemptState::Skipped
    }));
    assert!(!poison.exists());
}

#[test]
fn every_operation_has_one_ready_started_and_terminal_event() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("trace-outside-workspace-poison");
    let bundle = fixture_bundle(&poison);
    let (project, outcome) = execute_explicit(&bundle);
    let events = outcome.trace.events();

    assert_eq!(events.len(), project.plan.operations.len() * 3);
    for (ordinal, event) in events.iter().enumerate() {
        assert_eq!(event.coordinator_ordinal, ordinal as u64);
    }
    for operation in &project.plan.operations {
        let operation_events = events
            .iter()
            .filter(|event| event.plan_node == operation.id)
            .collect::<Vec<_>>();
        let lifecycle = operation_events
            .iter()
            .map(|event| event.state)
            .collect::<Vec<_>>();
        let terminal_state = if matches!(operation.op, ExecutableOp::RunRoute { .. }) {
            ProjectAttemptState::SettledSuccess
        } else {
            ProjectAttemptState::Finished
        };
        assert_eq!(
            lifecycle,
            [
                ProjectAttemptState::Ready,
                ProjectAttemptState::Started,
                terminal_state,
            ],
            "bad lifecycle for plan node {}",
            operation.id.0
        );
        let identity = ProjectAttemptIdentity::from_operation(operation).unwrap();
        for event in operation_events {
            assert_eq!(event.operation_label, identity.operation_label);
            assert_eq!(event.branch, identity.branch);
            assert_eq!(event.route_id, identity.route_id);
        }
    }

    let expected_start_order = ReadySchedule::derive(&project.graph)
        .unwrap()
        .launch_order()
        .unwrap();
    let actual_start_order = events
        .iter()
        .filter(|event| event.state == ProjectAttemptState::Started)
        .map(|event| event.plan_node)
        .collect::<Vec<_>>();
    assert_eq!(
        actual_start_order, expected_start_order,
        "coordinator did not launch ready operations in stable ordinal order"
    );

    let terminal = events
        .iter()
        .find(|event| {
            event.operation_label == "run-route:main"
                && event.state == ProjectAttemptState::SettledSuccess
        })
        .unwrap();
    let normalized = terminal.outcome.as_ref().unwrap();
    assert_eq!(normalized.exit_code, Some(0));
    assert_eq!(normalized.stdout_sha256, sha256(EXPECTED_STDOUT));
    assert_eq!(normalized.stderr_sha256, sha256(EXPECTED_STDERR));
    assert_eq!(normalized.artifacts.len(), 1);
    assert_eq!(normalized.artifacts[0].path, "result.txt");
    assert_eq!(normalized.artifacts[0].bytes_len, 22);
    assert_eq!(
        normalized.artifacts[0].sha256,
        sha256(b"project-hgraph-result\n")
    );

    let replay = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap();
    assert_eq!(
        events,
        replay.trace.events(),
        "normalized coordinator lifecycle must be deterministic"
    );
    assert!(!poison.exists());
}

#[test]
fn trace_header_binds_stable_graph_context_and_fresh_attempt_identity() {
    let external = tempfile::tempdir().unwrap();
    let poison = external
        .path()
        .join("trace-header-outside-workspace-poison");
    let bundle = fixture_bundle(&poison);
    let project = explicit_project(&bundle);

    let first = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap();
    let second = execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap();
    let first_header = first.trace.header();
    let second_header = second.trace.header();

    assert_eq!(first_header.project_name, project.plan.project_name);
    assert_eq!(first_header.bundle_digest, project.plan.bundle_digest);
    assert_eq!(first_header.target, project.plan.target);
    assert_eq!(first_header.policy, project.plan.policy.token());
    let mut logical_hasher = Sha256::new();
    logical_hasher.update(b"ostadix.project-hgraph.logical/v1\0");
    logical_hasher.update(project.to_text().as_bytes());
    assert_eq!(
        first_header.logical_graph_digest,
        hex::encode(logical_hasher.finalize())
    );
    assert_eq!(first_header.project_name, second_header.project_name);
    assert_eq!(first_header.bundle_digest, second_header.bundle_digest);
    assert_eq!(first_header.target, second_header.target);
    assert_eq!(first_header.policy, second_header.policy);
    assert_eq!(
        first_header.logical_graph_digest,
        second_header.logical_graph_digest
    );
    assert_ne!(
        first_header.execution_attempt_id, second_header.execution_attempt_id,
        "each coordinator invocation needs a distinct attempt identity"
    );
    assert!(!poison.exists());
}

fn unsupported_policy_bundle(
    policy: RoutePolicy,
    alternative_count: usize,
    marker: &Path,
) -> ProjectBundle {
    let mut bundle = ProjectBundle::empty("unsupported-multipath");
    let mut routes = Vec::new();
    for id in ["first", "second"] {
        let mut route = RouteSpec::new(id, RouteProvenance::CliOverride);
        route.command = vec![
            "sh".into(),
            "-c".into(),
            "printf '%s\\n' ran >> \"$PR8A_UNSUPPORTED_EXECUTION_MARKER\"".into(),
        ];
        route.environment = BTreeMap::from([(
            "PR8A_UNSUPPORTED_EXECUTION_MARKER".into(),
            marker.to_string_lossy().into_owned(),
        )]);
        route.is_default = id == "first";
        routes.push(route);
    }
    bundle.routes = routes;
    bundle.default_route = Some("first".into());
    bundle.route_sets.push(RouteSet {
        provides: "application".into(),
        alternatives: ["first", "second"]
            .into_iter()
            .take(alternative_count)
            .map(str::to_string)
            .collect(),
        policy,
    });
    bundle
}

#[test]
fn unsupported_policy_kinds_fail_closed_with_one_or_many_alternatives() {
    let external = tempfile::tempdir().unwrap();
    for policy in [
        RoutePolicy::Fallback,
        RoutePolicy::AnySuccess,
        RoutePolicy::RaceSuccess,
        RoutePolicy::RaceSettle,
        RoutePolicy::All,
        RoutePolicy::VerifyEquivalent,
        RoutePolicy::BenchmarkAndSelect,
    ] {
        let token = policy.token();
        for alternative_count in [1, 2] {
            let marker = external
                .path()
                .join(format!("executed-{token}-{alternative_count}"));
            let bundle = unsupported_policy_bundle(policy.clone(), alternative_count, &marker);
            let project = build_project_hgraph(&bundle, Some("application"), None).unwrap();
            let error =
                execute_project_hgraph(&bundle, &project, &RunOptions::default()).unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains(&token) || message.to_ascii_lowercase().contains("unsupported"),
                "unchecked {token} error for {alternative_count} alternative(s): {error:#}"
            );
            assert!(
                !marker.exists(),
                "unsupported policy {token} executed {alternative_count} alternative(s)"
            );
        }
    }
}

#[test]
fn environment_opt_in_never_falls_back_for_an_unsupported_policy() {
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("cli-outside-workspace-poison");
    let unexpected_execution = external.path().join("unsupported-cli-executed");
    let binary = env!("CARGO_BIN_EXE_olangc");

    let supported = Command::new(binary)
        .arg(fixture_path())
        .args(["--target", "script", "--route", "application"])
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .env("PR8A_EXTERNAL_POISON_MARKER", &poison)
        .output()
        .unwrap();
    assert!(
        supported.status.success(),
        "HGraph opt-in failed: {}",
        String::from_utf8_lossy(&supported.stderr)
    );
    assert!(
        supported
            .stdout
            .windows(EXPECTED_STDOUT.len())
            .any(|window| window == EXPECTED_STDOUT),
        "missing deterministic route output: {}",
        String::from_utf8_lossy(&supported.stdout)
    );
    assert!(!poison.exists());

    let unsupported = Command::new(binary)
        .arg(fixture_path())
        .args([
            "--target",
            "script",
            "--route",
            "application",
            "--routes-policy",
            "all",
        ])
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .env("PR8A_EXTERNAL_POISON_MARKER", &poison)
        .env("PR8A_EXECUTION_MARKER", &unexpected_execution)
        .output()
        .unwrap();
    assert!(
        !unsupported.status.success(),
        "unsupported HGraph policy silently fell back: {}",
        String::from_utf8_lossy(&unsupported.stdout)
    );
    let error = String::from_utf8_lossy(&unsupported.stderr);
    assert!(
        error.contains("all") || error.to_ascii_lowercase().contains("unsupported"),
        "unchecked HGraph policy error: {error}"
    );
    assert!(
        !unexpected_execution.exists(),
        "unsupported HGraph policy executed through legacy run_selection"
    );
    assert!(!poison.exists());
}

#[test]
fn olangc_hgraph_success_writes_an_unsigned_parseable_attempt_trace() {
    let project_dir = tempfile::tempdir().unwrap();
    for name in ["olang.project.toml", "input.txt"] {
        std::fs::copy(fixture_path().join(name), project_dir.path().join(name)).unwrap();
    }
    // Deliberately place the trace beneath the captured source root. It must be
    // excluded from the bundle on both its first creation and later overwrite.
    let trace_path = project_dir.path().join("success-attempt.json");
    let external = tempfile::tempdir().unwrap();
    let poison = external.path().join("success-outside-workspace-poison");
    let execute = || {
        Command::new(env!("CARGO_BIN_EXE_olangc"))
            .arg(project_dir.path())
            .args(["--target", "script", "--route", "application"])
            .arg("--project-trace-out")
            .arg(&trace_path)
            .env("O_PROJECT_EXECUTOR", "hgraph")
            .env("PR8A_EXTERNAL_POISON_MARKER", &poison)
            .output()
            .unwrap()
    };

    let first_output = execute();
    assert!(
        first_output.status.success(),
        "first olangc HGraph execution failed: {}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    let first_trace = read_cli_trace(&trace_path);

    let second_output = execute();
    assert!(
        second_output.status.success(),
        "second olangc HGraph execution failed: {}",
        String::from_utf8_lossy(&second_output.stderr)
    );
    assert!(!poison.exists());

    let trace = read_cli_trace(&trace_path);
    assert_unsigned_diagnostic_trace(&trace);
    let header = &trace["header"];
    assert_eq!(header["project_name"], "pr8a-project-hgraph-exec");
    assert_eq!(header["target"], "application");
    assert_eq!(header["policy"], "default");
    assert_sha256_json(&header["bundle_digest"], "bundle digest");
    assert_sha256_json(&header["logical_graph_digest"], "logical graph digest");
    assert_sha256_json(&header["execution_attempt_id"], "execution attempt id");
    assert_eq!(
        first_trace["header"]["bundle_digest"], header["bundle_digest"],
        "trace output was recaptured into the source bundle"
    );
    assert_eq!(
        first_trace["header"]["logical_graph_digest"], header["logical_graph_digest"],
        "logical graph changed when an existing trace was overwritten"
    );
    assert_ne!(
        first_trace["header"]["execution_attempt_id"], header["execution_attempt_id"],
        "two executions reused one attempt identity"
    );

    let main_events = trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["operation_label"] == "run-route:main")
        .collect::<Vec<_>>();
    assert_eq!(
        main_events
            .iter()
            .map(|event| event["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ready", "started", "settled_success"]
    );
    let outcome = &main_events.last().unwrap()["outcome"];
    assert_eq!(outcome["exit_code"], 0);
    assert_sha256_json(&outcome["stdout_sha256"], "stdout fingerprint");
    assert_sha256_json(&outcome["stderr_sha256"], "stderr fingerprint");
    assert_eq!(outcome["artifacts"].as_array().unwrap().len(), 1);
    assert_sha256_json(&outcome["artifacts"][0]["sha256"], "artifact fingerprint");
}

#[test]
fn olangc_trace_out_without_hgraph_fails_before_route_execution() {
    let external = tempfile::tempdir().unwrap();
    let trace_path = external.path().join("legacy-must-not-write.json");
    let execution_marker = external.path().join("legacy-must-not-execute");

    let output = Command::new(env!("CARGO_BIN_EXE_olangc"))
        .arg(fixture_path())
        .args(["--target", "script", "--route", "application"])
        .arg("--project-trace-out")
        .arg(&trace_path)
        .env_remove("O_PROJECT_EXECUTOR")
        .env("PR8A_EXECUTION_MARKER", &execution_marker)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "legacy runtime accepted trace output"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--project-trace-out") && stderr.contains("O_PROJECT_EXECUTOR=hgraph"),
        "missing checked trace-mode diagnostic: {stderr}"
    );
    assert!(
        !trace_path.exists(),
        "legacy runtime unexpectedly created a Project HGraph trace"
    );
    assert!(
        !execution_marker.exists(),
        "route executed before the trace-mode check"
    );
}

#[test]
fn olangc_persists_settled_failure_trace_before_returning_failure() {
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        project_dir.path().join("olang.project.toml"),
        r#"[project]
name = "cli-settled-failure"
default_route = "main"

[[routes]]
id = "main"
kind = "shell"
command = ["sh", "-c", "exit 7"]
default = true
guards = { requires_command = "sh" }
"#,
    )
    .unwrap();
    std::fs::write(project_dir.path().join("input.txt"), b"captured input\n").unwrap();
    let trace_path = project_dir.path().join("settled-failure-attempt.json");

    let output = Command::new(env!("CARGO_BIN_EXE_olangc"))
        .arg(project_dir.path())
        .args(["--target", "script"])
        .arg("--project-trace-out")
        .arg(&trace_path)
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "selected nonzero route unexpectedly made olangc succeed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no route succeeded"),
        "unexpected olangc failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let trace = read_cli_trace(&trace_path);
    assert_unsigned_diagnostic_trace(&trace);
    assert_eq!(trace["header"]["project_name"], "cli-settled-failure");
    assert_eq!(trace["header"]["target"], "main");
    assert_eq!(trace["header"]["policy"], "explicit:main");

    let events = trace["events"].as_array().unwrap();
    let run = events
        .iter()
        .filter(|event| event["operation_label"] == "run-route:main")
        .collect::<Vec<_>>();
    assert_eq!(
        run.iter()
            .map(|event| event["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ready", "started", "settled_failure"]
    );
    assert_eq!(run.last().unwrap()["outcome"]["exit_code"], 7);
    assert!(events.iter().any(|event| {
        event["operation_label"]
            .as_str()
            .is_some_and(|label| label.starts_with("select-route:"))
            && event["state"] == "finished"
    }));
}

#[test]
fn declared_pure_route_retains_conservative_hostworld_dependencies() {
    let external = tempfile::tempdir().unwrap();
    let bundle = fixture_bundle(&external.path().join("pure-poison"));
    let project = explicit_project(&bundle);
    let run = project
        .plan
        .operations
        .iter()
        .find(|operation| {
            matches!(
                &operation.op,
                ExecutableOp::RunRoute { route_id } if route_id == "main"
            )
        })
        .unwrap();

    assert!(run.route_facts.as_ref().unwrap().declared_pure);
    assert!(run.effects.unknown);
    assert!(run.effects.reads.contains(&ResourceKey::HostWorld));
    assert!(run.effects.writes.contains(&ResourceKey::HostWorld));
}

#[test]
fn ordinary_oir_graph_execution_is_unchanged() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_O"))
        .args(["--executor", "graph"])
        .arg(root.join("examples/hello.O"))
        .arg(root.join("backends"))
        .env_remove("O_PROJECT_EXECUTOR")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "ordinary OIR graph execution failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[number] 2");
}
