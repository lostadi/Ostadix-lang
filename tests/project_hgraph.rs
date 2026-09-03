//! Hosted PR7 project-to-HGraph planning corpus.
//!
//! This proves deterministic logical construction and exact source
//! provenance. It does not execute project commands, perform placement, or
//! constitute native/O-core/OSTADIX Alpha evidence.

use std::path::{Path, PathBuf};
use std::process::Command;

use o_lang::effects::{EffectSummary, ResourceKey};
use o_lang::hgraph::{ExecutableOp, HNode, HNodeKind};
use o_lang::ir::PlanNodeId;
use o_lang::project::plan::ProjectDependency;
use o_lang::project::runtime::resolve_selection;
use o_lang::project::{
    self, build_project_hgraph, ProjectBundle, ProjectCancellationSemantics, RoutePolicy,
    RouteProvenance, RouteSpec,
};
use o_lang::value::OValue;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph")
}

fn fixture_bundle() -> ProjectBundle {
    project::assemble(&fixture_path(), "pr7-project-hgraph", &[]).unwrap()
}

fn fixture_plan() -> project::ProjectHGraph {
    build_project_hgraph(&fixture_bundle(), Some("main"), None).unwrap()
}

fn operation_ids(
    project: &project::ProjectHGraph,
    predicate: impl Fn(&ExecutableOp) -> bool,
) -> Vec<o_lang::ir::PlanNodeId> {
    project
        .plan
        .operations
        .iter()
        .filter(|operation| predicate(&operation.op))
        .map(|operation| operation.id)
        .collect()
}

fn route_operation<'a>(
    project: &'a project::ProjectHGraph,
    branch: usize,
    route_id: &str,
    run: bool,
) -> &'a project::ProjectPlanOperation {
    project
        .plan
        .operations
        .iter()
        .find(|operation| {
            operation.branch == Some(branch)
                && match &operation.op {
                    ExecutableOp::RunRoute {
                        route_id: candidate,
                    } if run => candidate == route_id,
                    ExecutableOp::BuildRoute {
                        route_id: candidate,
                    } if !run => candidate == route_id,
                    _ => false,
                }
        })
        .unwrap()
}

#[test]
fn real_bundle_constructs_all_five_project_operations() {
    let bundle = fixture_bundle();
    let project = build_project_hgraph(&bundle, Some("main"), None).unwrap();
    project
        .validate_source(&bundle, Some("main"), None)
        .unwrap();

    assert_eq!(
        operation_ids(&project, |op| matches!(
            op,
            ExecutableOp::MaterializeProject
        ))
        .len(),
        2
    );
    assert_eq!(
        operation_ids(&project, |op| matches!(op, ExecutableOp::BuildRoute { .. })).len(),
        4
    );
    assert_eq!(
        operation_ids(&project, |op| matches!(op, ExecutableOp::RunRoute { .. })).len(),
        4
    );
    assert_eq!(
        operation_ids(&project, |op| matches!(
            op,
            ExecutableOp::CompareRouteResults
        ))
        .len(),
        1
    );
    assert_eq!(
        operation_ids(&project, |op| matches!(
            op,
            ExecutableOp::SelectRoute { .. }
        ))
        .len(),
        1
    );
    assert_eq!(project.plan.alternatives, ["impl-a", "impl-b"]);
    assert_eq!(project.plan.policy, RoutePolicy::VerifyEquivalent);

    for operation in &project.plan.operations {
        if matches!(operation.op, ExecutableOp::RunRoute { .. }) {
            assert!(operation.effects.unknown);
            assert!(operation.effects.spawn);
            assert!(operation.effects.reads.contains(&ResourceKey::HostWorld));
            assert!(operation.effects.writes.contains(&ResourceKey::HostWorld));
        }
    }
    let declared_pure = route_operation(&project, 1, "impl-b", true);
    assert!(declared_pure.route_facts.as_ref().unwrap().declared_pure);
    assert!(
        declared_pure.effects.unknown,
        "user pure metadata must not upgrade hosted execution"
    );
}

#[test]
fn topology_preserves_logical_branches_prerequisites_compare_and_selection() {
    let project = fixture_plan();
    let materializations = operation_ids(&project, |op| {
        matches!(op, ExecutableOp::MaterializeProject)
    });
    assert_eq!(materializations.len(), 2);

    let mut terminal_runs = Vec::new();
    for (branch, terminal_id) in [(0, "impl-a"), (1, "impl-b")] {
        let prepare_build = route_operation(&project, branch, "prepare", false);
        let prepare_run = route_operation(&project, branch, "prepare", true);
        let terminal_build = route_operation(&project, branch, terminal_id, false);
        let terminal_run = route_operation(&project, branch, terminal_id, true);
        assert_eq!(
            prepare_build.dependencies,
            [ProjectDependency::Value(materializations[branch])]
        );
        assert_eq!(
            prepare_run.dependencies,
            [ProjectDependency::Value(prepare_build.id)]
        );
        assert_eq!(
            terminal_build.dependencies,
            [ProjectDependency::Value(materializations[branch])]
        );
        assert_eq!(
            terminal_run.dependencies,
            [
                ProjectDependency::Value(terminal_build.id),
                ProjectDependency::Success(prepare_run.id),
            ]
        );

        let terminal_inputs = &project.graph.op_for(terminal_run.id).unwrap().inputs;
        let build_value = project
            .graph
            .op_for(terminal_build.id)
            .unwrap()
            .value_output;
        let prerequisite_value = project.graph.op_for(prepare_run.id).unwrap().value_output;
        let prerequisite_completion = project.graph.completion_node(prepare_run.id).unwrap();
        assert!(terminal_inputs.contains(&build_value));
        assert!(terminal_inputs.contains(&prerequisite_completion));
        assert!(!terminal_inputs.contains(&prerequisite_value));
        terminal_runs.push(terminal_run.id);
    }

    let compare = project
        .plan
        .operations
        .iter()
        .find(|operation| matches!(operation.op, ExecutableOp::CompareRouteResults))
        .unwrap();
    assert_eq!(
        compare.dependencies,
        terminal_runs
            .iter()
            .copied()
            .map(ProjectDependency::Value)
            .collect::<Vec<_>>()
    );
    let select = project
        .plan
        .operations
        .iter()
        .find(|operation| matches!(operation.op, ExecutableOp::SelectRoute { .. }))
        .unwrap();
    assert_eq!(select.dependencies, [ProjectDependency::Value(compare.id)]);
    assert_eq!(project.plan.roots, [select.id]);

    let schedule = o_lang::hgraph::ReadySchedule::derive(&project.graph).unwrap();
    let order = schedule.launch_order().unwrap();
    for operation in &project.plan.operations {
        let position = order.iter().position(|id| *id == operation.id).unwrap();
        for dependency in &operation.dependencies {
            let dependency = dependency.plan_node();
            assert!(
                order.iter().position(|id| *id == dependency).unwrap() < position,
                "dependency {} must precede {}",
                dependency.0,
                operation.id.0
            );
        }
    }

    // The plan branches are logically separate, but residual ambient effects
    // deliberately remain one conservative HostWorld chain. Preserve that
    // honesty until a trusted branch-scoped resource model exists.
    let second_materialize = project.graph.op_for(materializations[1]).unwrap();
    let ambient_input = second_materialize
        .inputs
        .iter()
        .find(|node| {
            matches!(
                project.graph.node(**node).map(|node| &node.kind),
                Some(HNodeKind::ResourceState {
                    resource: ResourceKey::HostWorld,
                    ..
                })
            )
        })
        .copied()
        .expect("second branch consumes the shared HostWorld chain");
    let ambient_producer = project.graph.node(ambient_input).unwrap().producer.unwrap();
    let predecessor = project
        .graph
        .op_map
        .values()
        .find(|operation| operation.edge == ambient_producer)
        .unwrap();
    assert_eq!(
        project.plan.operations[predecessor.plan_node.0].branch,
        Some(0),
        "residual HostWorld must conservatively serialize the logical branches"
    );
}

#[test]
fn project_plan_text_is_deterministic_and_omits_command_and_environment_values() {
    let first = fixture_plan().to_text();
    let second = fixture_plan().to_text();
    assert_eq!(first, second);
    assert!(first.contains("; ProjectExecutionPlan"));
    assert!(first.contains("policy=verify_equivalent"));
    assert!(first.contains("cancellation=none equivalence=required"));
    assert!(first.contains("guards=[env:PR7_REQUIRED_ENV]"));
    assert!(first.contains("env=[PLAN_VARIANT]"));
    assert!(first.contains("outputs=[out/a.json]"));
    assert!(first.contains("deps=[value:p"));
    assert!(first.contains("success:p"));
    assert!(!first.contains("PR7_IMPL_A_EXECUTED"));
    assert!(!first.contains("PLAN_VARIANT = \"a\""));
}

#[test]
fn directory_and_lifted_bundle_produce_identical_project_plans() {
    let directory_bundle = fixture_bundle();
    let lifted = project::lower::lower_to_o_validated(&directory_bundle).unwrap();
    let extracted = project::lower::extract_bundle_from_o(&lifted).unwrap();
    assert_eq!(directory_bundle, extracted);

    let directory = build_project_hgraph(&directory_bundle, Some("main"), None).unwrap();
    let embedded = build_project_hgraph(&extracted, Some("main"), None).unwrap();
    assert_eq!(directory.plan, embedded.plan);
    assert_eq!(directory.to_text(), embedded.to_text());
}

#[test]
fn every_route_policy_uses_shared_resolution_and_exact_policy_metadata() {
    let mut bundle = fixture_bundle();
    bundle.route("impl-a").unwrap();
    bundle.routes.iter_mut().for_each(|route| {
        route.priority = match route.id.as_str() {
            "impl-b" => 20,
            "impl-a" => 10,
            _ => 0,
        }
    });

    let policies = [
        RoutePolicy::Default,
        RoutePolicy::Fallback,
        RoutePolicy::AnySuccess,
        RoutePolicy::RaceSuccess,
        RoutePolicy::RaceSettle,
        RoutePolicy::All,
        RoutePolicy::VerifyEquivalent,
        RoutePolicy::BenchmarkAndSelect,
        RoutePolicy::BenchmarkValidateAndSelect,
    ];
    for policy in policies {
        let resolved = resolve_selection(&bundle, Some("main"), Some(policy.clone())).unwrap();
        let project = build_project_hgraph(&bundle, Some("main"), Some(policy.clone())).unwrap();
        let logical = project.logical_v1().unwrap();
        logical.validate_trusted_project(&project).unwrap();
        assert_eq!(logical.operations.len(), project.plan.operations.len());
        assert_eq!(project.plan.target, resolved.target);
        assert_eq!(project.plan.alternatives, resolved.alternatives);
        assert_eq!(project.plan.policy, resolved.policy);
        let comparisons = operation_ids(&project, |op| {
            matches!(op, ExecutableOp::CompareRouteResults)
        });
        assert_eq!(
            comparisons.len(),
            usize::from(policy.requires_declared_output_validation())
        );
    }

    let explicit = RoutePolicy::Explicit("impl-b".to_string());
    let project = build_project_hgraph(&bundle, Some("main"), Some(explicit)).unwrap();
    let logical = project.logical_v1().unwrap();
    logical.validate_trusted_project(&project).unwrap();
    assert_eq!(project.plan.alternatives, ["impl-b"]);
    assert_eq!(project.plan.policy.token(), "explicit:impl-b");
    assert_eq!(
        operation_ids(&project, |op| matches!(
            op,
            ExecutableOp::MaterializeProject
        ))
        .len(),
        1
    );
    let terminal = route_operation(&project, 0, "impl-b", true);
    let select = project
        .plan
        .operations
        .iter()
        .find(|operation| matches!(operation.op, ExecutableOp::SelectRoute { .. }))
        .unwrap();
    assert_eq!(select.dependencies, [ProjectDependency::Value(terminal.id)]);
    let select_inputs = &project.graph.op_for(select.id).unwrap().inputs;
    assert!(select_inputs.contains(&project.graph.op_for(terminal.id).unwrap().value_output));
    assert!(!select_inputs.contains(&project.graph.completion_node(terminal.id).unwrap()));
}

#[test]
fn malformed_project_references_duplicates_and_cycles_fail_before_planning() {
    let mut missing = fixture_bundle();
    missing
        .routes
        .iter_mut()
        .find(|route| route.id == "impl-a")
        .unwrap()
        .prerequisites = vec!["absent".into()];
    assert!(build_project_hgraph(&missing, Some("main"), None)
        .unwrap_err()
        .contains("missing prerequisite"));

    let mut duplicate = fixture_bundle();
    duplicate.routes.push(duplicate.routes[0].clone());
    assert!(build_project_hgraph(&duplicate, Some("main"), None)
        .unwrap_err()
        .contains("repeats route id"));

    let mut missing_alternative = fixture_bundle();
    missing_alternative.route_sets[0]
        .alternatives
        .push("absent".into());
    assert!(
        build_project_hgraph(&missing_alternative, Some("main"), None)
            .unwrap_err()
            .contains("missing route")
    );

    let mut cycle = fixture_bundle();
    cycle
        .routes
        .iter_mut()
        .find(|route| route.id == "prepare")
        .unwrap()
        .prerequisites = vec!["impl-a".into()];
    assert!(build_project_hgraph(&cycle, Some("main"), None)
        .unwrap_err()
        .contains("cycle"));
}

#[test]
fn project_source_validation_rejects_bundle_and_policy_substitution() {
    let bundle = fixture_bundle();
    let project = build_project_hgraph(&bundle, Some("main"), None).unwrap();

    let mut substituted = bundle.clone();
    substituted
        .routes
        .iter_mut()
        .find(|route| route.id == "impl-a")
        .unwrap()
        .command = vec!["false".into()];
    assert!(project
        .validate_source(&substituted, Some("main"), None)
        .unwrap_err()
        .contains("does not match"));
    assert!(project
        .validate_source(&bundle, Some("main"), Some(RoutePolicy::All))
        .unwrap_err()
        .contains("does not match"));
}

#[test]
fn project_projection_rejects_dependency_and_effect_forgery() {
    let mut dependency_kind_forgery = fixture_plan().plan;
    let terminal = dependency_kind_forgery
        .operations
        .iter_mut()
        .find(|operation| {
            operation.branch == Some(0)
                && matches!(
                    &operation.op,
                    ExecutableOp::RunRoute { route_id } if route_id == "impl-a"
                )
        })
        .unwrap();
    let prerequisite = terminal.dependencies[1].plan_node();
    terminal.dependencies[1] = ProjectDependency::Value(prerequisite);
    assert!(dependency_kind_forgery
        .validate()
        .unwrap_err()
        .contains("run dependencies differ"));

    let mut dependency_forgery = fixture_plan();
    let compare = operation_ids(&dependency_forgery, |op| {
        matches!(op, ExecutableOp::CompareRouteResults)
    })[0];
    let info = dependency_forgery.graph.op_for(compare).unwrap();
    let edge = info.edge;
    let removed_input = info.inputs[0];
    dependency_forgery
        .graph
        .exec_edges
        .get_mut(&edge)
        .unwrap()
        .ports
        .retain(|port| port.node != removed_input);
    assert!(dependency_forgery
        .plan
        .validate_projection(&dependency_forgery.graph)
        .is_err());

    let mut effect_forgery = fixture_plan();
    let run = operation_ids(&effect_forgery, |op| {
        matches!(op, ExecutableOp::RunRoute { .. })
    })[0];
    effect_forgery
        .graph
        .effect_summaries
        .insert(run, EffectSummary::pure());
    assert!(effect_forgery
        .plan
        .validate_projection(&effect_forgery.graph)
        .unwrap_err()
        .contains("hosted project operation"));

    let mut identity_forgery = fixture_plan();
    let run = operation_ids(&identity_forgery, |op| {
        matches!(op, ExecutableOp::RunRoute { .. })
    })[0];
    let value = identity_forgery.graph.op_for(run).unwrap().value_output;
    identity_forgery.graph.node_mut(value).unwrap().plan_node = Some(PlanNodeId(999));
    assert!(identity_forgery
        .plan
        .validate_projection(&identity_forgery.graph)
        .unwrap_err()
        .contains("source identity"));

    let mut inventory_forgery = fixture_plan();
    inventory_forgery
        .graph
        .add_node(HNode::with_value(OValue::text("unrelated-project-value")));
    assert!(inventory_forgery
        .plan
        .validate_projection(&inventory_forgery.graph)
        .unwrap_err()
        .contains("inventory differs"));

    let mut topology_forgery = fixture_plan().plan;
    topology_forgery.cancellation = ProjectCancellationSemantics::CancelLosers;
    assert!(topology_forgery
        .validate()
        .unwrap_err()
        .contains("cancellation"));

    let mut default_forgery =
        build_project_hgraph(&fixture_bundle(), Some("main"), Some(RoutePolicy::All))
            .unwrap()
            .plan;
    default_forgery.policy = RoutePolicy::Default;
    let select = default_forgery
        .operations
        .iter_mut()
        .find(|operation| matches!(operation.op, ExecutableOp::SelectRoute { .. }))
        .unwrap();
    select.op = ExecutableOp::SelectRoute {
        policy: RoutePolicy::Default.token(),
    };
    assert!(default_forgery
        .validate()
        .unwrap_err()
        .contains("exactly one selected alternative"));
}

#[test]
fn governed_route_effect_spelling_is_rejected_and_pure_metadata_keeps_hostworld() {
    let mut bundle = ProjectBundle::empty("effects");
    let mut route = RouteSpec::new("main", RouteProvenance::CliOverride);
    route.command = vec!["true".into()];
    route.is_default = true;
    route.effects.unknown = false;
    route.effects.reads = vec!["world:desk".into()];
    bundle.default_route = Some("main".into());
    bundle.routes.push(route);
    let error = build_project_hgraph(&bundle, None, None).unwrap_err();
    assert!(error.contains("requires trusted lowering"), "{error}");

    let mut pure = bundle;
    pure.routes[0].effects.reads.clear();
    pure.routes[0].effects.pure = true;
    let project = build_project_hgraph(&pure, None, None).unwrap();
    let run = project
        .plan
        .operations
        .iter()
        .find(|operation| matches!(operation.op, ExecutableOp::RunRoute { .. }))
        .unwrap();
    assert!(run.effects.unknown);
    assert!(run.effects.reads.contains(&ResourceKey::HostWorld));
    assert!(run.effects.writes.contains(&ResourceKey::HostWorld));
}

#[test]
fn real_cli_plans_directory_and_lifted_project_without_execution() {
    let binary = env!("CARGO_BIN_EXE_olangc");
    let args = ["--target", "ir", "--route", "main"];
    let temp = tempfile::tempdir().unwrap();
    let nonexecution_marker = temp.path().join("project-command-executed");
    let first = Command::new(binary)
        .arg(fixture_path())
        .args(args)
        .env("PR7_NONEXEC_MARKER", &nonexecution_marker)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = Command::new(binary)
        .arg(fixture_path())
        .args(args)
        .env("PR7_NONEXEC_MARKER", &nonexecution_marker)
        .output()
        .unwrap();
    assert_eq!(first.stdout, second.stdout);
    let text = String::from_utf8(first.stdout).unwrap();
    for marker in [
        "kind=materialize-project",
        "kind=build-route:prepare",
        "kind=run-route:impl-a",
        "kind=compare-route-results",
        "kind=select-route:verify_equivalent",
        "op=MaterializeProject",
        "op=CompareRouteResults",
    ] {
        assert!(text.contains(marker), "missing {marker}\n{text}");
    }
    assert!(!text.contains("PR7_IMPL_A_EXECUTED"));

    let bundle = fixture_bundle();
    let lifted = project::lower::lower_to_o_validated(&bundle).unwrap();
    let lifted_path = temp.path().join("fixture.O");
    std::fs::write(&lifted_path, lifted).unwrap();
    let lifted_output = Command::new(binary)
        .arg(&lifted_path)
        .args(args)
        .env("PR7_NONEXEC_MARKER", &nonexecution_marker)
        .output()
        .unwrap();
    assert!(lifted_output.status.success());
    assert_eq!(second.stdout, lifted_output.stdout);

    let grounding = Command::new(binary)
        .arg(fixture_path())
        .args(["--target", "ir", "--route", "main", "--grounding"])
        .env("PR7_NONEXEC_MARKER", &nonexecution_marker)
        .output()
        .unwrap();
    assert!(!grounding.status.success());
    assert!(String::from_utf8_lossy(&grounding.stderr)
        .contains("deferred to the PR9 project-grounding view"));

    let dot_first = Command::new(binary)
        .arg(fixture_path())
        .args(["--target", "dot", "--route", "main"])
        .env("PR7_NONEXEC_MARKER", &nonexecution_marker)
        .output()
        .unwrap();
    let dot_second = Command::new(binary)
        .arg(fixture_path())
        .args(["--target", "dot", "--route", "main"])
        .env("PR7_NONEXEC_MARKER", &nonexecution_marker)
        .output()
        .unwrap();
    assert!(dot_first.status.success());
    assert!(dot_second.status.success());
    assert_eq!(dot_first.stdout, dot_second.stdout);
    let dot = String::from_utf8(dot_first.stdout).unwrap();
    assert!(dot.contains("materialize-project"));
    assert!(dot.contains("compare-route-results"));
    assert!(dot.contains("select-route:verify_equivalent"));

    let invalid_policy = Command::new(binary)
        .arg(fixture_path())
        .args([
            "--target",
            "ir",
            "--route",
            "main",
            "--routes-policy",
            "definitely-not-a-policy",
        ])
        .env("PR7_NONEXEC_MARKER", &nonexecution_marker)
        .output()
        .unwrap();
    assert!(!invalid_policy.status.success());
    assert!(String::from_utf8_lossy(&invalid_policy.stderr).contains("unknown route policy"));
    assert!(
        !nonexecution_marker.exists(),
        "project planning executed a route outside its disposable workspace"
    );
}
