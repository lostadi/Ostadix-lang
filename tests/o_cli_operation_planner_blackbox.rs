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

// Rebuild the fixture's declared route-pipeline and tuple identities when a
// test deliberately changes how the same captured implementation is invoked.
// This is fixture construction; the acceptance assertions exercise the CLI.
fn observation_fixture_pipeline(route: &o_lang::project::RouteSpec) -> o_lang::world::ArtifactId {
    let value = serde_json::to_value(route).unwrap();
    let mut fields = vec![
        "\"schema\":\"ostadix.project-route-pipeline/v1\"".to_string(),
        format!("\"route_id\":{}", value["id"]),
    ];
    for key in [
        "kind",
        "command",
        "evaluator",
        "entrypoint",
        "working_directory",
        "arguments",
        "environment",
        "prerequisites",
        "inputs",
        "outputs",
        "effects",
        "failure_continuation",
        "result_codec",
        "provides",
        "guards",
    ] {
        let encoded = if key == "effects" {
            serde_json::to_string(&route.effects).unwrap()
        } else {
            value[key].to_string()
        };
        fields.push(format!("\"{key}\":{encoded}"));
    }
    artifact_id_for_bytes(format!("{{{}}}", fields.join(",")).as_bytes())
}

fn rewrite_observation_fixture(project: &Path, indirect: bool, mismatch: bool) {
    let manifest_path = project.join("olang.project.toml");
    let mut manifest: toml::Value =
        toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let mut request = OperationPlanningRequestV1::decode_json(
        &fs::read(project.join("operation-planning-request.json")).unwrap(),
    )
    .unwrap();
    if indirect {
        let route = manifest["routes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|route| route["id"].as_str() == Some("normalize_chunked"))
            .unwrap();
        route["kind"] = toml::Value::String("shell".to_string());
        route["command"] = toml::Value::Array(
            ["sh", "-c", "exec python3 normalize_chunked.py input.json"]
                .into_iter()
                .map(|part| toml::Value::String(part.to_string()))
                .collect(),
        );
    }
    let manifest_text = toml::to_string(&manifest).unwrap();
    let mut bundle = o_lang::project::bundle::bundle_dir(project, "observation-fixture").unwrap();
    o_lang::project::discover::apply_discovery(&mut bundle, project);
    o_lang::project::manifest::apply_manifest(&mut bundle, &manifest_text, "olang.project.toml")
        .unwrap();
    for descriptor in &mut request.descriptors {
        let old_id = descriptor.id().unwrap();
        let route_id = if descriptor.realization.as_str() == "normalize/python-chunked/v1" {
            "normalize_chunked"
        } else {
            "normalize_scalar"
        };
        descriptor.execution_pipeline.content =
            observation_fixture_pipeline(bundle.route(route_id).unwrap());
        let new_id = descriptor.id().unwrap();
        for member in &mut request.realization_set.realizations {
            if *member == old_id {
                *member = new_id.clone();
            }
        }
        for offer in &mut request.offers {
            if offer.descriptor != old_id {
                continue;
            }
            offer.descriptor = new_id.clone();
            offer.cost_profile.descriptor = new_id.clone();
            if mismatch && route_id == "normalize_chunked" {
                let mut target = serde_json::to_value(&offer.target).unwrap();
                target["platform"]["operating_system"] = serde_json::json!("windows");
                offer.target = serde_json::from_value(target).unwrap();
                offer.cost_profile.target = offer.target_digest().unwrap();
            }
            let candidate = offer.candidate().unwrap();
            let binding = manifest["operation"]["bindings"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|binding| {
                    binding["realization"].as_str() == Some(descriptor.realization.as_str())
                })
                .unwrap();
            for (field, digest) in [
                ("descriptor_sha256", candidate.descriptor.as_sha256()),
                ("target_sha256", candidate.target.as_sha256()),
                ("cost_profile_sha256", candidate.cost_profile.as_sha256()),
            ] {
                binding[field] = toml::Value::String(digest.to_string());
            }
        }
    }
    request.graph.operations[0].realization_set = request.realization_set.id().unwrap();
    let request = OperationPlanningRequestV1::new(
        request.graph,
        request.contract,
        request.interface,
        request.descriptors,
        request.realization_set,
        request.objective,
        request.offers,
        request.transfer_plans,
    )
    .unwrap();
    write(
        &project.join("operation-planning-request.json"),
        request.canonical_json_pretty().unwrap(),
    );
    write(&manifest_path, toml::to_string(&manifest).unwrap());
}

#[cfg(unix)]
#[test]
fn operation_execution_observation_keeps_indirect_and_platform_claims_separate() {
    let python = which::which("python3").expect("operation example requires python3");
    let python_bin = python.parent().unwrap();
    for (indirect, mismatch, artifact_use, runtime, target_platform) in [
        (
            true,
            false,
            "not_established",
            "not_established",
            "matched_local_platform",
        ),
        (
            false,
            true,
            "direct_entrypoint_submitted",
            "declared_interpreter_command_submitted",
            "mismatched_local_platform",
        ),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        let state = temporary.path().join("state");
        let project = temporary.path().join("normalize");
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/normalize"),
            &project,
        );
        rewrite_observation_fixture(&project, indirect, mismatch);
        let before = snapshot_tree(&project);
        let executed = run(o_cli(&home, &state, Some(python_bin)).args([
            "run",
            project.to_str().unwrap(),
            "--json",
        ]));
        assert_success(&executed, "execute route with conservative observation");
        let executed = single_json(&executed);
        assert_eq!(executed["disposition"], "succeeded");
        assert_eq!(executed["recording"]["status"], "recorded");
        let run_id = executed["run_id"].as_str().unwrap();
        let inspected =
            run(o_cli(&home, &state, Some(python_bin)).args(["inspect", run_id, "--json"]));
        assert_success(&inspected, "inspect conservative execution observation");
        let inspected: Value = serde_json::from_slice(&inspected.stdout).unwrap();
        let evidence = &inspected["record"]["operation_plan"]["execution"];
        assert_eq!(evidence["route"], "normalize_chunked");
        assert_eq!(evidence["artifact_use"], artifact_use);
        assert_eq!(evidence["runtime"], runtime);
        assert_eq!(evidence["target_platform"], target_platform);
        assert_eq!(evidence["operating_system"], std::env::consts::OS);
        assert_eq!(evidence["architecture"], std::env::consts::ARCH);
        let state_before_observation = snapshot_tree(&state);
        let observed = run(o_cli(&home, &state, Some(python_bin)).args([
            "observe",
            project.to_str().unwrap(),
            "--run",
            run_id,
            "--json",
        ]));
        assert_success(&observed, "observe conservative execution observation");
        let observed = single_json(&observed);
        assert_eq!(
            observed["runtime_binding"]["execution_observation"],
            *evidence
        );
        assert_eq!(snapshot_tree(&project), before);
        assert_eq!(snapshot_tree(&state), state_before_observation);
    }
}

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
    let original_record: o_lang::intent::RunRecordV1 =
        serde_json::from_value(inspection["record"].clone()).unwrap();
    original_record.validate().unwrap();
    let decision = original_record.operation_decision.as_ref().unwrap();
    let retained_plan = original_record.operation_plan.as_ref().unwrap();
    let retained_deployment = o_lang::computation::realization_plan::DeploymentPlanV2::decode_json(
        &serde_json::to_vec(&retained_plan.deployment).unwrap(),
    )
    .unwrap();
    assert_eq!(
        decision.planning_request_id,
        request.id().unwrap().as_sha256()
    );
    assert_eq!(
        decision.deployment_plan_id,
        retained_deployment.id().unwrap().as_sha256()
    );
    assert_eq!(
        retained_plan.request,
        serde_json::to_value(&request).unwrap()
    );
    assert_eq!(
        serde_json::to_value(retained_deployment.selected_candidate().unwrap()).unwrap(),
        retained_plan.selected_candidate
    );
    let execution_evidence = retained_plan.execution.as_ref().unwrap();
    assert_eq!(
        execution_evidence.artifact_use,
        "direct_entrypoint_submitted"
    );
    assert_eq!(execution_evidence.operating_system, std::env::consts::OS);
    assert_eq!(execution_evidence.architecture, std::env::consts::ARCH);
    assert_eq!(
        execution_evidence.runtime,
        "declared_interpreter_command_submitted"
    );
    assert!(inspection["record"]["operation_plan"]["execution"]
        .get("node_id")
        .is_none());
    assert!(inspection["record"]["operation_plan"]["execution"]
        .get("command")
        .is_none());

    for mutation in 0..4 {
        let mut substituted = original_record.clone();
        match mutation {
            0 => {
                substituted
                    .operation_decision
                    .as_mut()
                    .unwrap()
                    .planning_request_content_sha256 = "ab".repeat(32)
            }
            1 => {
                substituted
                    .operation_plan
                    .as_mut()
                    .unwrap()
                    .selected_candidate["target_node"] = serde_json::json!("substituted")
            }
            2 => {
                substituted.operation_plan.as_mut().unwrap().request["descriptors"][0]
                    ["implementation"] = serde_json::json!("ab".repeat(32))
            }
            _ => {
                substituted.operation_plan.as_mut().unwrap().deployment["operations"][0]
                    ["selection"] = Value::Null
            }
        }
        assert!(
            substituted.validate().is_err(),
            "accepted substituted operation binding {mutation}"
        );
    }
    // Missing optional fields retain the exact legacy encoding and cannot be
    // upgraded into an original historical decision during deserialization.
    let mut legacy_record = original_record.clone();
    legacy_record.operation_decision = None;
    legacy_record.operation_plan = None;
    legacy_record.trace =
        o_lang::intent::RunTraceBindingV1::unavailable("legacy fixture has no retained trace");
    legacy_record.validate().unwrap();
    let legacy_json = serde_json::to_value(&legacy_record).unwrap();
    assert!(legacy_json.get("operation_decision").is_none());
    assert!(legacy_json.get("operation_plan").is_none());
    let roundtrip: o_lang::intent::RunRecordV1 = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(roundtrip, legacy_record);
    let legacy_state = root.join("legacy-state");
    let legacy_store =
        o_lang::intent::RunStoreV1::open_at(legacy_state.join("ostadix/runs-v1")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            legacy_state.join("ostadix"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let legacy_seed = o_lang::intent::RunAttemptSeedV1 {
        input: legacy_record.input.clone(),
        intent: legacy_record.intent.clone(),
        plan: legacy_record.plan.clone(),
        started_unix_nanos: legacy_record.started_unix_nanos,
        operation_decision: None,
        operation_plan_ref: None,
    };
    let legacy_lease = legacy_store.begin(legacy_seed.clone()).unwrap();
    legacy_record.run_id = legacy_lease.attempt().run_id.clone();
    legacy_record.sequence = legacy_lease.attempt().sequence;
    let legacy_finalized = legacy_lease.finalize(legacy_record.clone(), None).unwrap();
    let legacy_observation = run(o_cli(&home, &legacy_state, Some(python_bin)).args([
        "observe",
        project.to_str().unwrap(),
        "--run",
        &legacy_finalized.run_id,
        "--json",
    ]));
    assert_success(&legacy_observation, "legacy operation observation");
    let legacy_observation = single_json(&legacy_observation);
    assert_eq!(
        legacy_observation["runtime_binding"]["schema"],
        "ostadix.operation-runtime-binding/v1"
    );
    assert_eq!(
        legacy_observation["status"],
        "current_binary_recomputed_plan_matched_content_verified_run"
    );
    assert!(legacy_observation["runtime_binding"]
        .get("recorded_decision")
        .is_none());

    // A terminal writer cannot strip the decision that was frozen before
    // execution and publish the result as an apparently legacy observation.
    let mut frozen_seed = legacy_seed;
    frozen_seed.operation_decision = original_record.operation_decision.clone();
    let mut initial_snapshot = original_record.operation_plan.clone().unwrap();
    initial_snapshot.execution = None;
    let frozen_lease = legacy_store
        .begin_with_operation_plan(frozen_seed.clone(), &initial_snapshot)
        .unwrap();
    let frozen_id = frozen_lease.attempt().run_id.clone();
    legacy_record.run_id = frozen_id.clone();
    legacy_record.sequence = frozen_lease.attempt().sequence;
    assert!(frozen_lease
        .finalize(legacy_record, None)
        .unwrap_err()
        .to_string()
        .contains("pre-execution attempt seed"));
    let reader = o_lang::intent::RunStoreReaderV1::open_existing(legacy_store.root()).unwrap();
    let (incomplete, _) = reader
        .read_terminal(o_lang::intent::RunSelectorV1::RunId(frozen_id), false)
        .unwrap();
    assert_eq!(
        incomplete.operation_decision,
        original_record.operation_decision
    );
    assert_eq!(
        incomplete.disposition,
        o_lang::intent::RunDispositionV1::RecordingIncomplete
    );
    assert_eq!(incomplete.operation_plan.as_ref(), Some(&initial_snapshot));
    let abandoned = legacy_store
        .begin_with_operation_plan(frozen_seed, &initial_snapshot)
        .unwrap();
    let abandoned_id = abandoned.attempt().run_id.clone();
    drop(abandoned);
    let _reopened = o_lang::intent::RunStoreV1::open_at(legacy_store.root()).unwrap();
    let (interrupted, _) = reader
        .read_terminal(o_lang::intent::RunSelectorV1::RunId(abandoned_id), false)
        .unwrap();
    assert_eq!(
        interrupted.operation_decision,
        original_record.operation_decision
    );
    assert_eq!(
        interrupted.disposition,
        o_lang::intent::RunDispositionV1::Interrupted
    );
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
    let alternate_contract_observation = run(o_cli(&home, &state, Some(python_bin))
        .env("O_PROJECT_EXECUTOR", "hgraph")
        .args([
            "observe",
            project.to_str().unwrap(),
            "--run",
            run_id.as_str(),
            "--json",
        ]));
    assert_success(
        &alternate_contract_observation,
        "historical operation observation under a changed executor setting",
    );
    assert_eq!(observation.stdout, alternate_contract_observation.stdout);
    let observation = single_json(&observation);
    assert_eq!(observation["schema"], "ostadix.operation-observation/v1");
    assert_eq!(
        observation["status"],
        "retained_original_plan_matched_content_verified_run"
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
        "ostadix.operation-runtime-binding/v2"
    );
    assert_eq!(
        observation["runtime_binding"]["status"],
        "retained_original_plan_matched_recorded_route"
    );
    assert_eq!(
        observation["runtime_binding"]["run_record"],
        observation["run"]["record"]
    );
    assert_eq!(
        observation["runtime_binding"]["recorded_decision"]["planning_request_id"],
        observation["planning_request_id"]
    );
    assert_eq!(
        observation["runtime_binding"]["recorded_decision"]["deployment_plan_id"],
        observation["deployment_plan_id"]
    );
    assert_eq!(
        observation["runtime_binding"]["recorded_selected_candidate"],
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
        "original_pre_execution_decision_and_planner_records_retained"
    );
    assert_eq!(
        observation["nonclaims"]["operation_plan_persistence"],
        "original_decision_request_and_deployment_retained"
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

#[cfg(unix)]
#[test]
fn killed_operation_recovers_durable_original_plan_without_inventing_an_outcome() {
    use o_lang::intent::{
        RunDispositionV1, RunInspectionV1, RunSelectorV1, RunStoreReaderV1, RunStoreV1,
    };
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("normalize");
    let bin = root.join("bin");
    let marker = root.join("route-started");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/normalize"),
        &project,
    );
    let wrapper = bin.join("python3");
    write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf started > '{}'\nwhile :; do /bin/sleep 1; done\n",
            marker.display()
        ),
    );
    make_executable(&wrapper);
    let planned =
        run(o_cli(&home, &state, Some(&bin)).args(["plan", project.to_str().unwrap(), "--json"]));
    assert_success(&planned, "plan before interrupted operation");
    let planned = single_json(&planned);
    let mut command = o_cli(&home, &state, Some(&bin));
    command
        .args(["run", project.to_str().unwrap(), "--json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    // Keep cleanup independent of assertions so a failed test never strands
    // this deliberately blocked route or its child process.
    struct KillGroup(u32);
    impl Drop for KillGroup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.0 as i32), libc::SIGKILL);
            }
        }
    }
    let group = KillGroup(child.id());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() && Instant::now() < deadline {
        assert!(
            child.try_wait().unwrap().is_none(),
            "operation exited before the side effect"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists(), "route did not reach its side effect");
    let store_root = state.join("ostadix/runs-v1");
    let reader = RunStoreReaderV1::open_existing(&store_root).unwrap();
    let RunInspectionV1::Running { attempt } =
        reader.inspect(RunSelectorV1::LastRun, false).unwrap()
    else {
        panic!("blocked operation did not retain a running attempt");
    };
    let reference = attempt
        .seed
        .operation_plan_ref
        .as_ref()
        .expect("planner payload was not durable before side effects");
    let snapshot_path = store_root.join("objects/records").join(&reference.sha256);
    let original_bytes = fs::read(&snapshot_path).unwrap();
    assert_eq!(original_bytes.len() as u64, reference.bytes_len);
    let run_id = attempt.run_id.clone();
    // Maintenance must retain snapshots referenced by live attempts.
    RunStoreV1::open_at(&store_root).unwrap();
    assert_eq!(fs::read(&snapshot_path).unwrap(), original_bytes);
    let mut corrupted = original_bytes.clone();
    corrupted[0] ^= 1;
    fs::write(&snapshot_path, corrupted).unwrap();
    assert!(reader
        .inspect(RunSelectorV1::RunId(run_id.clone()), false)
        .is_err());
    fs::write(&snapshot_path, &original_bytes).unwrap();
    drop(group);
    assert!(!child.wait().unwrap().success());

    RunStoreV1::open_at(&store_root).unwrap();
    let (record, _) = reader
        .read_terminal(RunSelectorV1::RunId(run_id.clone()), false)
        .unwrap();
    assert_eq!(record.disposition, RunDispositionV1::Interrupted);
    assert_eq!(record.operation_decision, attempt.seed.operation_decision);
    let snapshot = record
        .operation_plan
        .as_ref()
        .expect("orphan reconciliation lost original planner payload");
    assert_eq!(
        snapshot.request,
        serde_json::from_slice::<Value>(
            &fs::read(project.join("operation-planning-request.json")).unwrap()
        )
        .unwrap()
    );
    assert_eq!(snapshot.deployment, planned["deployment"]);
    assert_eq!(
        snapshot.selected_candidate,
        planned["deployment"]["operations"][0]["selection"]
    );
    assert!(snapshot.execution.is_none());
    assert!(record.route_results.is_empty());
    let observed = run(o_cli(&home, &state, Some(&bin)).args([
        "observe",
        project.to_str().unwrap(),
        "--run",
        &run_id,
        "--json",
    ]));
    assert_success(&observed, "observe recovered original planner payload");
    let observed = single_json(&observed);
    assert_eq!(
        observed["status"],
        "retained_original_plan_matched_content_verified_run"
    );
    assert_eq!(observed["selected_candidate"], snapshot.selected_candidate);
    assert_eq!(
        observed["deployment_plan_id"],
        planned["deployment_plan_id"]
    );
    assert_eq!(observed["execution"], Value::Null);
    assert_eq!(
        observed["runtime_graph"]["observations"][0]["state"],
        "proposed"
    );
    assert_eq!(
        observed["runtime_graph"]["observations"][0]["metrics"]["execution_ns"],
        Value::Null
    );
    let replanned = run(o_cli(&home, &state, Some(&bin)).args([
        "replan",
        project.to_str().unwrap(),
        "--run",
        &run_id,
        "--without-target",
        "ambient-python-primary",
        "--json",
    ]));
    assert_success(&replanned, "replan unobserved operation outcome");
    let replanned = single_json(&replanned);
    assert_eq!(
        replanned["recovery_plan_status"],
        "not_applicable_source_outcome_unobserved"
    );
}
