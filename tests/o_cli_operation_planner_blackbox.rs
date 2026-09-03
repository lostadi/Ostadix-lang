//! Compiled-binary acceptance test for the marked operation-project slice.
//!
//! One test owns the retained-run state end to end so subprocess cases cannot
//! race each other. Read-only commands are checked before and after execution,
//! while the one `run` case must dispatch exactly the explicitly bound route.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use o_lang::computation::realization_plan::{OperationPlanningRequestV1, ValueResidencyV1};
use o_lang::computation_core::artifact_id_for_bytes;
use serde_json::Value;

#[derive(Debug, Eq, PartialEq)]
enum TreeSnapshot {
    Missing,
    Present(BTreeMap<PathBuf, Vec<u8>>),
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    let mut children = fs::read_dir(source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        let target = destination.join(child.file_name().unwrap());
        if child.is_dir() {
            copy_tree(&child, &target);
        } else {
            fs::copy(&child, target).unwrap();
        }
    }
}

fn snapshot_tree(root: &Path) -> TreeSnapshot {
    if !root.exists() {
        return TreeSnapshot::Missing;
    }
    let mut files = BTreeMap::new();
    snapshot_tree_inner(root, root, &mut files);
    TreeSnapshot::Present(files)
}

fn snapshot_tree_inner(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.file_type().is_symlink() {
        files.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
        );
        return;
    }
    if metadata.is_file() {
        files.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            fs::read(path).unwrap(),
        );
        return;
    }
    let mut children = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        snapshot_tree_inner(root, &child, files);
    }
}

fn deterministic_path(extra: Option<&Path>) -> OsString {
    let mut entries = Vec::new();
    if let Some(extra) = extra {
        entries.push(extra.to_path_buf());
    }
    entries.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    std::env::join_paths(entries).unwrap()
}

fn o_cli(home: &Path, state: &Path, extra_path: Option<&Path>) -> Command {
    let temporary = home.join("tmp");
    fs::create_dir_all(home).unwrap();
    fs::create_dir_all(&temporary).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_STATE_HOME", state)
        .env("TMPDIR", temporary)
        .env("PATH", deterministic_path(extra_path))
        .env("LANG", "C")
        .env("LC_ALL", "C");
    command
}

fn run(command: &mut Command) -> Output {
    command.output().expect("launch compiled o-cli")
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn single_json(output: &Output) -> Value {
    let stdout = std::str::from_utf8(&output.stdout).expect("JSON stdout must be UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count(),
        1,
        "expected exactly one compact JSON value\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn operation_error(output: &Output, command: &str, message_fragment: &str) -> Value {
    assert!(
        !output.status.success(),
        "operation command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let error = single_json(output);
    assert_eq!(error["schema"], "ostadix.operation-command-error/v1");
    assert_eq!(error["status"], "error");
    assert_eq!(error["command"], command);
    assert_eq!(error["error"]["kind"], "validation_failed");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains(message_fragment),
        "error message did not contain {message_fragment:?}: {error}",
    );
    error
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

#[test]
fn marked_normalize_project_plans_runs_observes_and_replans_exactly() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("normalize");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/normalize"),
        &project,
    );
    let python = which::which("python3").expect("operation example requires python3");
    let python_bin = python.parent().expect("python3 path must have a parent");
    let project_before = snapshot_tree(&project);

    let request = OperationPlanningRequestV1::decode_json(
        &fs::read(project.join("operation-planning-request.json")).unwrap(),
    )
    .expect("checked-in operation planning request must validate");
    for (realization, script) in [
        ("normalize/python-scalar/v1", "normalize_scalar.py"),
        ("normalize/python-chunked/v1", "normalize_chunked.py"),
    ] {
        let descriptor = request
            .descriptors
            .iter()
            .find(|descriptor| descriptor.realization.as_str() == realization)
            .unwrap_or_else(|| panic!("missing descriptor {realization}"));
        assert_eq!(
            descriptor.implementation,
            artifact_id_for_bytes(&fs::read(project.join(script)).unwrap()),
            "descriptor implementation identity drifted from {script}",
        );
    }

    let execute_example = |script: &str| {
        Command::new(&python)
            .current_dir(&project)
            .args([script, "input.json"])
            .env_clear()
            .env("PATH", deterministic_path(Some(python_bin)))
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .output()
            .expect("execute normalize realization")
    };
    let scalar = execute_example("normalize_scalar.py");
    let chunked = execute_example("normalize_chunked.py");
    assert_success(&scalar, "scalar normalize realization");
    assert_success(&chunked, "chunked normalize realization");
    assert_eq!(scalar.stdout, chunked.stdout);
    assert_eq!(scalar.stdout, b"{\"values\":[0.2,0.4,0.6,0.8,1.0]}\n");
    assert!(scalar.stderr.is_empty());
    assert!(chunked.stderr.is_empty());

    let describe = run(o_cli(&home, &state, Some(python_bin)).args([
        "operation",
        project.to_str().unwrap(),
        "--json",
    ]));
    assert_success(&describe, "operation description");
    let description = single_json(&describe);
    assert_eq!(
        description["schema"],
        "ostadix.operation-project-description/v1"
    );
    assert_eq!(description["status"], "valid_marked_operation_project");
    assert_eq!(description["logical_operation"], "tensor/normalize");
    assert_eq!(description["candidate_offers"], 2);
    assert_eq!(description["route_bindings"], 2);
    assert_eq!(description["nonclaims"]["dispatch"], "not_run");

    let realizations_output = run(o_cli(&home, &state, Some(python_bin)).args([
        "realizations",
        project.to_str().unwrap(),
        "--json",
    ]));
    assert_success(&realizations_output, "realization catalog");
    let realizations = single_json(&realizations_output);
    assert_eq!(
        realizations["schema"],
        "ostadix.operation-realization-catalog/v1"
    );
    assert_eq!(realizations["realizations"].as_array().unwrap().len(), 2);
    assert_eq!(realizations["unavailable_targets"][0]["target"], "gpu-1");
    assert!(realizations["realizations"]
        .as_array()
        .unwrap()
        .iter()
        .all(
            |offer| offer["availability"] == "declared_offer_with_explicit_route"
                && offer["live_eligibility"] == "not_observed"
                && offer["implementation"].as_str().is_some()
                && offer["implementation_sha256"].as_str().is_some()
                && offer["execution_pipeline_schema"] == "ostadix.project-route-pipeline/v1"
                && offer["route_pipeline_sha256"].as_str().is_some()
                && offer["cost_profile_sha256"].as_str().is_some()
                && offer["target_semantics"]
                    == "descriptive_execution_context_not_verified_physical_node_or_failure_domain"
        ));

    let expanded_project = root.join("expanded-full-key");
    copy_tree(&project, &expanded_project);
    let mut expanded_request_parts = request.clone();
    let mut additional_offer = expanded_request_parts
        .offers
        .iter()
        .find(|offer| {
            offer.candidate().is_ok_and(|candidate| {
                candidate.realization.as_str() == "normalize/python-chunked/v1"
            })
        })
        .unwrap()
        .clone();
    let input_artifact = artifact_id_for_bytes(&fs::read(project.join("input.json")).unwrap());
    additional_offer.inputs[0].residency =
        ValueResidencyV1::ContentArtifact(input_artifact.clone());
    additional_offer.cost_profile.inputs[0].residency =
        ValueResidencyV1::ContentArtifact(input_artifact);
    additional_offer.cost_profile.components.compute_ns += 50_000;
    expanded_request_parts.offers.push(additional_offer);
    let expanded_request = OperationPlanningRequestV1::new(
        expanded_request_parts.graph,
        expanded_request_parts.contract,
        expanded_request_parts.interface,
        expanded_request_parts.descriptors,
        expanded_request_parts.realization_set,
        expanded_request_parts.objective,
        expanded_request_parts.offers,
        expanded_request_parts.transfer_plans,
    )
    .expect("distinct residency must form a valid distinct conceptual tuple");
    let additional_candidate = expanded_request
        .offers
        .iter()
        .map(|offer| offer.candidate().unwrap())
        .find(|candidate| {
            candidate.realization.as_str() == "normalize/python-chunked/v1"
                && matches!(
                    candidate.inputs[0].residency,
                    ValueResidencyV1::ContentArtifact(_)
                )
        })
        .unwrap();
    write(
        &expanded_project.join("operation-planning-request.json"),
        expanded_request.canonical_json_pretty().unwrap(),
    );
    let mut expanded_manifest =
        fs::read_to_string(expanded_project.join("olang.project.toml")).unwrap();
    expanded_manifest.push_str(&format!(
        "\n[[operation.bindings]]\ndescriptor_sha256 = \"{}\"\nrealization = \"{}\"\ntarget_sha256 = \"{}\"\ntarget = \"{}\"\ncost_profile_sha256 = \"{}\"\nroute = \"normalize_chunked\"\nimplementation = \"normalize_chunked.py\"\n",
        additional_candidate.descriptor.as_sha256(),
        additional_candidate.realization,
        additional_candidate.target.as_sha256(),
        additional_candidate.target_display_name,
        additional_candidate.cost_profile.as_sha256(),
    ));
    write(
        &expanded_project.join("olang.project.toml"),
        &expanded_manifest,
    );
    let expanded_catalog = run(o_cli(&home, &state, Some(python_bin)).args([
        "realizations",
        expanded_project.to_str().unwrap(),
        "--json",
    ]));
    assert_success(&expanded_catalog, "full tuple-key realization catalog");
    let expanded_catalog = single_json(&expanded_catalog);
    assert_eq!(
        expanded_catalog["realizations"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        expanded_catalog["realizations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|offer| offer["route"] == "normalize_chunked")
            .count(),
        2,
        "one route should be reusable by multiple exact representation/residency tuples",
    );

    let aliased_project = root.join("aliased-full-key");
    copy_tree(&expanded_project, &aliased_project);
    let portable_cost = request
        .offers
        .iter()
        .map(|offer| offer.candidate().unwrap())
        .find(|candidate| candidate.realization.as_str() == "normalize/python-chunked/v1")
        .unwrap()
        .cost_profile;
    let aliased_manifest = expanded_manifest.replacen(
        additional_candidate.cost_profile.as_sha256(),
        portable_cost.as_sha256(),
        1,
    );
    write(
        &aliased_project.join("olang.project.toml"),
        aliased_manifest,
    );
    let aliased_catalog = run(o_cli(&home, &state, Some(python_bin)).args([
        "realizations",
        aliased_project.to_str().unwrap(),
        "--json",
    ]));
    operation_error(
        &aliased_catalog,
        "realizations",
        "repeat descriptor/target/cost-profile tuple",
    );

    let malformed_project = root.join("malformed-request");
    copy_tree(&project, &malformed_project);
    write(
        &malformed_project.join("operation-planning-request.json"),
        b"{\"schema\":\"ostadix.operation-planning-request/v1\"}\n",
    );
    let malformed = run(o_cli(&home, &state, Some(python_bin)).args([
        "realizations",
        malformed_project.to_str().unwrap(),
        "--json",
    ]));
    operation_error(
        &malformed,
        "realizations",
        "failed to validate operation planning request",
    );

    let swapped_implementation_project = root.join("swapped-implementation");
    copy_tree(&project, &swapped_implementation_project);
    let swapped_implementation_manifest =
        fs::read_to_string(swapped_implementation_project.join("olang.project.toml"))
            .unwrap()
            .replace(
                "implementation = \"normalize_chunked.py\"",
                "implementation = \"__scalar_implementation__\"",
            )
            .replace(
                "implementation = \"normalize_scalar.py\"",
                "implementation = \"normalize_chunked.py\"",
            )
            .replace(
                "implementation = \"__scalar_implementation__\"",
                "implementation = \"normalize_scalar.py\"",
            );
    write(
        &swapped_implementation_project.join("olang.project.toml"),
        swapped_implementation_manifest,
    );
    let swapped_implementation = run(o_cli(&home, &state, Some(python_bin)).args([
        "realizations",
        swapped_implementation_project.to_str().unwrap(),
        "--json",
    ]));
    operation_error(
        &swapped_implementation,
        "realizations",
        "implementation digest does not match captured",
    );

    let swapped_pipeline_project = root.join("swapped-route-pipeline");
    copy_tree(&project, &swapped_pipeline_project);
    let swapped_pipeline_manifest =
        fs::read_to_string(swapped_pipeline_project.join("olang.project.toml"))
            .unwrap()
            .replace(
                "command = [\"python3\", \"normalize_chunked.py\", \"input.json\"]",
                "command = [\"python3\", \"__scalar_command__\", \"input.json\"]",
            )
            .replace(
                "command = [\"python3\", \"normalize_scalar.py\", \"input.json\"]",
                "command = [\"python3\", \"normalize_chunked.py\", \"input.json\"]",
            )
            .replace("__scalar_command__", "normalize_scalar.py");
    write(
        &swapped_pipeline_project.join("olang.project.toml"),
        swapped_pipeline_manifest,
    );
    let pipeline_state = root.join("pipeline-state");
    let poison_pipeline_bin = root.join("pipeline-bin");
    let poison_pipeline_marker = root.join("swapped-pipeline-executed");
    write(
        &poison_pipeline_bin.join("python3"),
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 97\n",
            poison_pipeline_marker.display()
        ),
    );
    make_executable(&poison_pipeline_bin.join("python3"));
    let swapped_pipeline = run(
        o_cli(&home, &pipeline_state, Some(&poison_pipeline_bin)).args([
            "run",
            swapped_pipeline_project.to_str().unwrap(),
            "--json",
        ]),
    );
    assert!(!swapped_pipeline.status.success());
    let swapped_pipeline = single_json(&swapped_pipeline);
    assert_eq!(swapped_pipeline["disposition"], "preflight_failed");
    assert!(swapped_pipeline["failure"]["message"]
        .as_str()
        .unwrap()
        .contains("execution pipeline does not match the exact"));
    assert!(!poison_pipeline_marker.exists());
    assert_eq!(snapshot_tree(&pipeline_state), TreeSnapshot::Missing);

    let human_plan = run(o_cli(&home, &state, Some(python_bin)).args([
        "plan",
        project.to_str().unwrap(),
        "--explain",
    ]));
    assert_success(&human_plan, "explained operation plan");
    let human_plan_again = run(o_cli(&home, &state, Some(python_bin)).args([
        "plan",
        project.to_str().unwrap(),
        "--explain",
    ]));
    assert_success(&human_plan_again, "repeated explained operation plan");
    assert_eq!(human_plan.stdout, human_plan_again.stdout);
    let plan_text = String::from_utf8(human_plan.stdout).unwrap();
    assert!(plan_text.contains("Ostadix operation plan (read-only)"));
    assert!(plan_text.contains("Selected realization: normalize/python-chunked/v1"));
    assert!(plan_text.contains("Bound route: normalize_chunked"));
    assert!(plan_text.contains("Because:"));
    assert!(plan_text.contains("dispatch=not_run"));

    let plan_output = run(o_cli(&home, &state, Some(python_bin)).args([
        "plan",
        project.to_str().unwrap(),
        "--explain",
        "--json",
    ]));
    assert_success(&plan_output, "JSON operation plan");
    let plan_output_again = run(o_cli(&home, &state, Some(python_bin)).args([
        "plan",
        project.to_str().unwrap(),
        "--explain",
        "--json",
    ]));
    assert_success(&plan_output_again, "repeated JSON operation plan");
    assert_eq!(plan_output.stdout, plan_output_again.stdout);
    let plan = single_json(&plan_output);
    assert_eq!(plan["schema"], "ostadix.operation-plan-summary/v1");
    assert_eq!(plan["status"], "planned_without_dispatch");
    assert_eq!(
        plan["selected"]["realization"],
        "normalize/python-chunked/v1"
    );
    assert_eq!(plan["selected"]["route"], "normalize_chunked");
    assert_eq!(
        plan["selected"]["execution_pipeline_schema"],
        "ostadix.project-route-pipeline/v1"
    );
    assert!(plan["selected"]["implementation_sha256"].as_str().is_some());
    assert!(plan["selected"]["cost_profile_sha256"].as_str().is_some());
    assert!(plan["selected"]["route_plan_sha256"].as_str().is_some());
    assert!(plan["selected"]["route_deployment_sha256"]
        .as_str()
        .is_some());
    assert_eq!(plan["deployment"]["schema"], "ostadix.deployment-plan/v2");
    assert_eq!(plan["nonclaims"]["authority"], "none");
    assert_eq!(
        plan["nonclaims"]["target_failure_domain_independence"],
        "not_established"
    );
    assert_eq!(snapshot_tree(&project), project_before);
    assert_eq!(snapshot_tree(&state), TreeSnapshot::Missing);

    let execution = run(o_cli(&home, &state, Some(python_bin)).args([
        "run",
        project.to_str().unwrap(),
        "--json",
    ]));
    assert_success(&execution, "bound operation execution");
    let execution = single_json(&execution);
    assert_eq!(execution["schema"], "ostadix.run-summary/v1");
    assert_eq!(execution["disposition"], "succeeded");
    assert_eq!(execution["recording"]["status"], "recorded");
    let run_id = execution["run_id"].as_str().unwrap().to_owned();

    let inspection =
        run(o_cli(&home, &state, Some(python_bin)).args(["inspect", run_id.as_str(), "--json"]));
    assert_success(&inspection, "operation run inspection");
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    let route_results = inspection["record"]["route_results"].as_array().unwrap();
    assert_eq!(
        route_results.len(),
        1,
        "operation run dispatched more than one route"
    );
    assert_eq!(route_results[0]["route_id"], "normalize_chunked");
    assert_eq!(
        route_results[0]["value"]["values"],
        serde_json::json!([0.2, 0.4, 0.6, 0.8, 1.0])
    );

    let state_after_run = snapshot_tree(&state);
    let observation = run(o_cli(&home, &state, Some(python_bin)).args([
        "observe",
        project.to_str().unwrap(),
        "--run",
        run_id.as_str(),
        "--json",
    ]));
    assert_success(&observation, "operation observation");
    let observation_again = run(o_cli(&home, &state, Some(python_bin)).args([
        "observe",
        project.to_str().unwrap(),
        "--run",
        run_id.as_str(),
        "--json",
    ]));
    assert_success(&observation_again, "repeated operation observation");
    assert_eq!(observation.stdout, observation_again.stdout);
    let observation = single_json(&observation);
    assert_eq!(observation["schema"], "ostadix.operation-observation/v1");
    assert_eq!(
        observation["status"],
        "current_binary_recomputed_plan_matched_content_verified_run"
    );
    assert_eq!(observation["run"]["id"], run_id);
    assert_eq!(observation["run"]["record"]["kind"], "record");
    assert!(observation["run"]["record"]["sha256"].as_str().is_some());
    assert!(observation["run"]["record"]["bytes_len"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));
    assert_eq!(observation["selected"]["route"], "normalize_chunked");
    assert_eq!(
        observation["runtime_graph"]["schema"],
        "ostadix.runtime-graph/v2"
    );
    assert!(observation["runtime_graph_id"].as_str().is_some());
    assert_eq!(
        observation["runtime_binding"]["schema"],
        "ostadix.operation-runtime-binding/v1"
    );
    assert_eq!(
        observation["runtime_binding"]["status"],
        "current_binary_recomputed_plan_matched_recorded_route"
    );
    assert_eq!(
        observation["runtime_binding"]["run_record"],
        observation["run"]["record"]
    );
    assert_eq!(
        observation["runtime_binding"]["recomputed_planning_request_id"],
        observation["planning_request_id"]
    );
    assert_eq!(
        observation["runtime_binding"]["recomputed_deployment_plan_id"],
        observation["deployment_plan_id"]
    );
    assert_eq!(
        observation["runtime_binding"]["recomputed_selected_candidate"],
        observation["selected_candidate"]
    );
    assert_eq!(
        observation["runtime_binding"]["recorded_route_plan_sha256"],
        observation["runtime_binding"]["recomputed_route_plan_sha256"]
    );
    assert_eq!(
        observation["runtime_binding"]["recorded_route_deployment_sha256"],
        observation["runtime_binding"]["recomputed_route_deployment_sha256"]
    );
    assert_eq!(
        observation["runtime_binding"]["exact_route_pipeline_sha256"],
        observation["selected"]["route_pipeline_sha256"]
    );
    assert_eq!(
        observation["binding"]["operation_plan"],
        "current_binary_recomputed_not_persisted_in_run_record_v1"
    );
    assert_eq!(
        observation["nonclaims"]["operation_plan_persistence"],
        "not_stored_in_run_record_v1"
    );
    assert_eq!(
        observation["execution"]["value"]["values"],
        serde_json::json!([0.2, 0.4, 0.6, 0.8, 1.0])
    );
    assert_eq!(snapshot_tree(&state), state_after_run);

    let input_path = project.join("input.json");
    let original_input = fs::read(&input_path).unwrap();
    write(&input_path, b"[2.0,4.0]\n");
    let changed_observation = run(o_cli(&home, &state, Some(python_bin)).args([
        "observe",
        project.to_str().unwrap(),
        "--run",
        run_id.as_str(),
        "--json",
    ]));
    operation_error(
        &changed_observation,
        "observe",
        "operation project changed after run",
    );
    assert_eq!(snapshot_tree(&state), state_after_run);
    write(&input_path, original_input);
    assert_eq!(snapshot_tree(&project), project_before);

    let unavailable_replan = run(o_cli(&home, &state, Some(python_bin)).args([
        "replan",
        project.to_str().unwrap(),
        "--run",
        run_id.as_str(),
        "--without-target",
        "gpu-1",
        "--json",
    ]));
    assert_success(&unavailable_replan, "unavailable-target replan");
    let unavailable_replan_again = run(o_cli(&home, &state, Some(python_bin)).args([
        "replan",
        project.to_str().unwrap(),
        "--run",
        run_id.as_str(),
        "--without-target",
        "gpu-1",
        "--json",
    ]));
    assert_success(
        &unavailable_replan_again,
        "repeated unavailable-target replan",
    );
    assert_eq!(unavailable_replan.stdout, unavailable_replan_again.stdout);
    let unavailable_replan = single_json(&unavailable_replan);
    assert_eq!(unavailable_replan["schema"], "ostadix.operation-replan/v1");
    assert_eq!(unavailable_replan["status"], "planned_without_dispatch");
    assert_eq!(unavailable_replan["excluded_offer_count"], 0);
    assert_eq!(unavailable_replan["selection_changed"], false);
    assert_eq!(unavailable_replan["selected"]["route"], "normalize_chunked");
    assert_eq!(
        unavailable_replan["alternative_target_basis"],
        "not_applicable_selection_unchanged"
    );
    assert_eq!(
        unavailable_replan["recovery_plan_status"],
        "not_applicable_source_succeeded"
    );
    assert_eq!(unavailable_replan["recovery_plan"], Value::Null);

    let all_excluded = run(o_cli(&home, &state, Some(python_bin)).args([
        "replan",
        project.to_str().unwrap(),
        "--run",
        run_id.as_str(),
        "--without-target",
        "Ambient Python Primary",
        "--without-target",
        "Ambient Python Fallback",
        "--json",
    ]));
    operation_error(
        &all_excluded,
        "replan",
        "operation planner found no rankable candidate",
    );
    assert_eq!(snapshot_tree(&state), state_after_run);

    let selected_target = plan["selected"]["target"].as_str().unwrap();
    let changed_replan = run(o_cli(&home, &state, Some(python_bin)).args([
        "replan",
        project.to_str().unwrap(),
        "--run",
        run_id.as_str(),
        "--without-target",
        selected_target,
        "--json",
    ]));
    assert_success(&changed_replan, "selected-target exclusion replan");
    let changed_replan = single_json(&changed_replan);
    assert_eq!(changed_replan["excluded_offer_count"], 1);
    assert_eq!(changed_replan["selection_changed"], true);
    assert_eq!(
        changed_replan["alternative_target_basis"],
        "statically_compatible_descriptive_offer_not_an_independent_failure_domain"
    );
    assert_eq!(
        changed_replan["selected"]["realization"],
        "normalize/python-scalar/v1"
    );
    assert_eq!(changed_replan["selected"]["route"], "normalize_scalar");
    assert_eq!(
        changed_replan["recovery_plan_status"],
        "not_applicable_source_succeeded"
    );
    assert_eq!(changed_replan["recovery_plan"], Value::Null);
    assert_eq!(snapshot_tree(&state), state_after_run);
    assert_eq!(snapshot_tree(&project), project_before);

    let failed_project = root.join("failed-normalize");
    copy_tree(&project, &failed_project);
    let failed_home = root.join("failed-home");
    let failed_state = root.join("failed-state");
    let failed_bin = root.join("failed-bin");
    let failed_python = failed_bin.join("python3");
    write(&failed_python, b"#!/bin/sh\nexit 41\n");
    make_executable(&failed_python);
    let failed_execution = run(o_cli(&failed_home, &failed_state, Some(&failed_bin)).args([
        "run",
        failed_project.to_str().unwrap(),
        "--json",
    ]));
    assert!(!failed_execution.status.success());
    let failed_execution = single_json(&failed_execution);
    assert_eq!(failed_execution["disposition"], "execution_failed");
    assert_eq!(failed_execution["recording"]["status"], "recorded");
    let failed_run_id = failed_execution["run_id"].as_str().unwrap();
    let failed_state_after_run = snapshot_tree(&failed_state);
    let recovery = run(o_cli(&failed_home, &failed_state, Some(&failed_bin)).args([
        "replan",
        failed_project.to_str().unwrap(),
        "--run",
        failed_run_id,
        "--without-target",
        "Ambient Python Primary",
        "--json",
    ]));
    assert_success(&recovery, "failed-run recovery planning");
    let recovery = single_json(&recovery);
    assert_eq!(recovery["selection_changed"], true);
    assert_eq!(recovery["selected"]["route"], "normalize_scalar");
    assert_eq!(recovery["recovery_plan_status"], "descriptive");
    assert_eq!(recovery["recovery_execution"], "not_performed");
    assert_eq!(
        recovery["alternative_target_basis"],
        "statically_compatible_descriptive_offer_not_an_independent_failure_domain"
    );
    assert!(recovery["recovery_plan_id"].as_str().is_some());
    assert_eq!(
        recovery["recovery_plan"]["schema"],
        "ostadix.recovery-plan/v1"
    );
    assert_eq!(recovery["nonclaims"]["dispatch"], "not_run");
    assert_eq!(recovery["nonclaims"]["recovery_plan"], "descriptive");
    assert_eq!(recovery["nonclaims"]["recovery_execution"], "not_performed");
    assert_eq!(
        recovery["nonclaims"]["target_failure_domain_independence"],
        "not_established"
    );
    assert_eq!(snapshot_tree(&failed_state), failed_state_after_run);

    let wrong_run = run(o_cli(&failed_home, &failed_state, Some(&failed_bin)).args([
        "observe",
        project.to_str().unwrap(),
        "--run",
        failed_run_id,
        "--json",
    ]));
    operation_error(&wrong_run, "observe", "selected run input");
    assert_eq!(snapshot_tree(&failed_state), failed_state_after_run);

    let invalid_project = root.join("invalid-mapping");
    copy_tree(&project, &invalid_project);
    let invalid_manifest_path = invalid_project.join("olang.project.toml");
    let invalid_manifest = fs::read_to_string(&invalid_manifest_path)
        .unwrap()
        .replace("route = \"normalize_chunked\"", "route = \"missing_route\"");
    write(&invalid_manifest_path, invalid_manifest);
    let invalid_home = root.join("invalid-home");
    let invalid_state = root.join("invalid-state");
    let poison_bin = root.join("poison-bin");
    let poison_python = poison_bin.join("python3");
    let poison_marker = root.join("invalid-mapping-executed");
    write(
        &poison_python,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 97\n",
            poison_marker.display()
        ),
    );
    make_executable(&poison_python);
    let invalid_run = run(
        o_cli(&invalid_home, &invalid_state, Some(&poison_bin)).args([
            "run",
            invalid_project.to_str().unwrap(),
            "--json",
        ]),
    );
    assert!(!invalid_run.status.success());
    let invalid_summary = single_json(&invalid_run);
    assert_eq!(invalid_summary["disposition"], "preflight_failed");
    assert_eq!(invalid_summary["run_id"], Value::Null);
    assert_eq!(invalid_summary["recording"]["status"], "not_started");
    assert!(!poison_marker.exists(), "invalid mapping reached dispatch");
    assert_eq!(snapshot_tree(&invalid_state), TreeSnapshot::Missing);

    let unmarked = root.join("unmarked");
    write(
        &unmarked.join("olang.project.toml"),
        b"[project]\nname = \"unmarked\"\n",
    );
    let rejected_unmarked = run(o_cli(&home, &state, Some(python_bin)).args([
        "operation",
        unmarked.to_str().unwrap(),
        "--json",
    ]));
    operation_error(
        &rejected_unmarked,
        "operation",
        "is not an existing marked operation project",
    );
    let nonexistent_name = run(o_cli(&home, &state, Some(python_bin)).args([
        "realizations",
        "normalize-registry-name",
        "--json",
    ]));
    operation_error(
        &nonexistent_name,
        "realizations",
        "is not an existing marked operation project",
    );
}
