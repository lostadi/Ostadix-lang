//! Canonical World PR8-2 deployment-intent coverage.
//!
//! These tests cover descriptive hosted intent and snapshot-derived placement.
//! They do not prove current inventory, admission, runtime instantiation,
//! authority, remote dispatch, recovery, native/O-core continuity, or G1.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use o_lang::project::{
    self, build_project_hgraph, DeploymentCompatibilityIssueV1, DeploymentOperationBindingV1,
    DeploymentPlanError, DeploymentPlanV1, DeploymentProviderBindingV1,
    DeploymentProviderSnapshotV1, LogicalOperationIdV1, PlacementSnapshotV1, ProjectBundle,
};
use o_lang::world::{
    ArtifactId, DomainGeneration, DomainId, DomainIdentity, NodeGeneration, NodeId, NodeIdentity,
    ProcessGeneration, ProcessId, ProcessIdentity, ResourceGeneration, ResourceId,
    ResourceIdentity, ResourceOwner, TaskId, TaskIdentity, WorldEpoch, WorldId, WorldIdentity,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph")
}

fn fixture_bundle() -> ProjectBundle {
    support::normalize_project_fixture_modes(
        project::assemble(&fixture_path(), "pr7-project-hgraph", &[]).unwrap(),
    )
}

fn fixture_logical() -> project::LogicalHGraphV1 {
    build_project_hgraph(&fixture_bundle(), Some("main"), None)
        .unwrap()
        .logical_v1()
        .unwrap()
}

fn fixture_supported_logical() -> project::LogicalHGraphV1 {
    build_project_hgraph(&fixture_bundle(), Some("impl-a"), None)
        .unwrap()
        .logical_v1()
        .unwrap()
}

fn artifact(label: &str) -> ArtifactId {
    ArtifactId::from_sha256(hex::encode(Sha256::digest(label.as_bytes()))).unwrap()
}

fn world(epoch: u64) -> WorldIdentity {
    WorldIdentity::new(
        WorldId::new("desk").unwrap(),
        WorldEpoch::new(epoch).unwrap(),
    )
}

fn provider(
    project_bundle: &ArtifactId,
    node_name: &str,
    node_generation: u64,
    service_generation: u64,
    admits_host_world: bool,
    complete: bool,
) -> DeploymentProviderSnapshotV1 {
    let node = NodeIdentity::new(
        WorldId::new("desk").unwrap(),
        NodeId::new(node_name).unwrap(),
        NodeGeneration::new(node_generation).unwrap(),
    );
    let domain = DomainIdentity::new(
        node.clone(),
        DomainId::new("project-host").unwrap(),
        DomainGeneration::new(3).unwrap(),
    );
    let process = ProcessIdentity::new(
        domain.clone(),
        ProcessId::new("runner").unwrap(),
        ProcessGeneration::new(4).unwrap(),
    );
    let service = ResourceIdentity::new(
        ResourceOwner::Process {
            process: process.clone(),
        },
        ResourceId::new("project/executor").unwrap(),
        ResourceGeneration::new(service_generation).unwrap(),
    );
    let mut runtime_classes = if complete {
        vec![
            "policy.verify-equivalent".to_string(),
            "project.compare-route-results".to_string(),
            "project.coordinator".to_string(),
            "project.materializer".to_string(),
            "project.route-preparer".to_string(),
            "project.runner".to_string(),
            "route.build-target".to_string(),
            "route.shell-task".to_string(),
        ]
    } else {
        vec!["project.coordinator".to_string()]
    };
    runtime_classes.sort();
    DeploymentProviderSnapshotV1 {
        binding: DeploymentProviderBindingV1 {
            node,
            domain,
            process: Some(process),
            service,
            implementation: artifact(&format!("implementation-{node_name}")),
        },
        architecture: "aarch64".to_string(),
        platform_os: "macos".to_string(),
        runtime_classes,
        executables: if complete {
            vec!["sh".to_string()]
        } else {
            Vec::new()
        },
        evaluators: Vec::new(),
        environment_keys: if complete {
            vec!["PR7_REQUIRED_ENV".to_string()]
        } else {
            Vec::new()
        },
        packages: Vec::new(),
        project_bundles: vec![project_bundle.clone()],
        project_paths: if complete {
            vec![
                project::DeploymentProjectPathV1 {
                    bundle: project_bundle.clone(),
                    artifact: project::LogicalArtifactRefV1 {
                        role: project::LogicalArtifactRoleV1::Input,
                        path: "input.txt".to_string(),
                    },
                },
                project::DeploymentProjectPathV1 {
                    bundle: project_bundle.clone(),
                    artifact: project::LogicalArtifactRefV1 {
                        role: project::LogicalArtifactRoleV1::Output,
                        path: "build/**".to_string(),
                    },
                },
                project::DeploymentProjectPathV1 {
                    bundle: project_bundle.clone(),
                    artifact: project::LogicalArtifactRefV1 {
                        role: project::LogicalArtifactRoleV1::Output,
                        path: "out/a.json".to_string(),
                    },
                },
                project::DeploymentProjectPathV1 {
                    bundle: project_bundle.clone(),
                    artifact: project::LogicalArtifactRefV1 {
                        role: project::LogicalArtifactRoleV1::Output,
                        path: "out/b.json".to_string(),
                    },
                },
            ]
        } else {
            Vec::new()
        },
        failure_domain: format!("rack-{node_name}"),
        admits_host_world,
    }
}

fn tasks(logical: &project::LogicalHGraphV1) -> BTreeMap<LogicalOperationIdV1, TaskIdentity> {
    logical
        .operations
        .iter()
        .map(|operation| {
            (
                operation.id,
                TaskIdentity::new(
                    WorldId::new("desk").unwrap(),
                    TaskId::new(format!("logical-{}", operation.id.0)).unwrap(),
                ),
            )
        })
        .collect()
}

fn fixture_snapshot(logical: &project::LogicalHGraphV1) -> PlacementSnapshotV1 {
    PlacementSnapshotV1::new(
        world(7),
        vec![
            provider(&logical.source.bundle, "node-b", 1, 1, false, false),
            provider(&logical.source.bundle, "node-a", 2, 5, true, true),
        ],
    )
    .unwrap()
}

#[test]
fn hosted_plan_is_canonical_explicit_and_never_mints_world_identity() {
    let unsupported = DeploymentPlanV1::hosted(&fixture_logical()).unwrap();
    assert!(unsupported.operations.iter().all(|operation| matches!(
        &operation.binding,
        DeploymentOperationBindingV1::Unresolved { issues }
            if matches!(issues.as_slice(), [DeploymentCompatibilityIssueV1::UnsupportedHostedPolicy { .. }])
    )));

    let logical = fixture_supported_logical();
    let deployment = DeploymentPlanV1::hosted(&logical).unwrap();
    deployment.validate_trusted_hosted(&logical).unwrap();

    assert!(deployment.world.is_none());
    assert!(deployment.placement_snapshot.is_none());
    assert!(deployment.selected_provider.is_none());
    assert!(deployment.operations.iter().all(|operation| {
        operation.task.is_none()
            && matches!(
                operation.binding,
                DeploymentOperationBindingV1::HostedCoordinator
                    | DeploymentOperationBindingV1::AmbientHost
            )
    }));
    assert!(deployment
        .operations
        .iter()
        .any(|operation| operation.requirements.residual_host_world));
    assert!(deployment.operations.iter().any(|operation| {
        operation
            .requirements
            .executables
            .contains(&"sh".to_string())
    }));
    assert!(deployment.operations.iter().all(|operation| {
        matches!(
            operation.requirements.architecture,
            project::DeploymentArchitectureRequirementV1::Unspecified
        ) && operation.requirements.packages.is_empty()
            && operation.requirements.failure_domains.is_empty()
            && operation.requirements.authority.is_empty()
    }));
    let run = logical
        .operations
        .iter()
        .zip(&deployment.operations)
        .find_map(|(logical_operation, deployment_operation)| {
            (matches!(
                logical_operation.kind,
                project::LogicalOperationKindV1::RunRoute { .. }
            ) && deployment_operation
                .requirements
                .environment_overlay_keys
                .iter()
                .any(|key| key == "PLAN_VARIANT"))
            .then_some(deployment_operation)
        })
        .unwrap();
    assert_eq!(run.requirements.environment_overlay_keys, ["PLAN_VARIANT"]);
    assert_eq!(run.requirements.environment_keys, ["PR7_REQUIRED_ENV"]);
    assert!(logical.operations.iter().zip(&deployment.operations).any(
        |(logical_operation, deployment_operation)| {
            matches!(
                logical_operation.kind,
                project::LogicalOperationKindV1::BuildRoute { .. }
            ) && matches!(
                deployment_operation.binding,
                DeploymentOperationBindingV1::HostedCoordinator
            )
        }
    ));
    assert!(logical.operations.iter().zip(&deployment.operations).any(
        |(logical_operation, deployment_operation)| {
            matches!(
                logical_operation.kind,
                project::LogicalOperationKindV1::RunRoute { .. }
            ) && matches!(
                deployment_operation.binding,
                DeploymentOperationBindingV1::AmbientHost
            )
        }
    ));

    let canonical = deployment.canonical_bytes().unwrap();
    assert_eq!(
        DeploymentPlanV1::decode_canonical(&canonical).unwrap(),
        deployment
    );
    let pretty = serde_json::to_vec_pretty(&deployment).unwrap();
    assert!(matches!(
        DeploymentPlanV1::decode_canonical(&pretty),
        Err(DeploymentPlanError::NonCanonicalEncoding)
    ));
}

#[test]
fn placement_snapshot_is_canonical_strict_and_pinned() {
    let logical = fixture_logical();
    let snapshot = fixture_snapshot(&logical);
    let canonical = snapshot.canonical_bytes().unwrap();
    assert_eq!(
        PlacementSnapshotV1::decode_canonical(&canonical).unwrap(),
        snapshot
    );
    assert!(matches!(
        PlacementSnapshotV1::decode_canonical(&serde_json::to_vec_pretty(&snapshot).unwrap()),
        Err(DeploymentPlanError::NonCanonicalEncoding)
    ));

    let mut unknown = serde_json::to_value(&snapshot).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("unsupported".to_string(), json!(true));
    assert!(PlacementSnapshotV1::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
    let mut version = serde_json::to_value(&snapshot).unwrap();
    version["schema_version"] = json!(2);
    assert!(PlacementSnapshotV1::decode(&serde_json::to_vec(&version).unwrap()).is_err());
    assert_eq!(
        snapshot.digest().unwrap().as_sha256(),
        "a9c69cdb34fab279d2aeaca705b8f07a749acbbe2d9ca10b4ca33bdf1f52b942",
        "a canonical placement snapshot schema change requires a new version and reviewed vector"
    );
}

#[test]
fn hosted_deployment_digest_is_pinned() {
    assert_eq!(
        DeploymentPlanV1::hosted(&fixture_supported_logical())
            .unwrap()
            .digest()
            .unwrap()
            .as_sha256(),
        "2d6292fed596c3fe191591802d84f8300c7c20d32ca2678a5c7dd401f91ccbb7",
        "a canonical deployment schema change requires a new version and reviewed vector"
    );
}

#[test]
fn snapshot_placement_uses_exact_caller_tasks_and_rejects_incompatible_provider() {
    let logical = fixture_logical();
    let snapshot = fixture_snapshot(&logical);
    let tasks = tasks(&logical);
    let deployment =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &snapshot, &tasks).unwrap();
    deployment
        .validate_trusted_snapshot(&logical, &snapshot, &tasks)
        .unwrap();

    let selected = deployment.selected_provider.as_ref().unwrap();
    assert_eq!(selected.node.node().as_str(), "node-a");
    assert_eq!(deployment.rejected_providers.len(), 1);
    assert_eq!(
        deployment.rejected_providers[0]
            .provider
            .node
            .node()
            .as_str(),
        "node-b"
    );
    assert!(deployment.rejected_providers[0]
        .issues
        .iter()
        .any(|issue| matches!(
            issue.issue,
            DeploymentCompatibilityIssueV1::ResidualHostWorldDenied
        )));
    assert!(deployment.rejected_providers[0]
        .issues
        .iter()
        .any(|issue| matches!(
            issue.issue,
            DeploymentCompatibilityIssueV1::MissingProjectPath { .. }
        )));
    assert!(deployment.operations.iter().all(|operation| {
        operation.task.as_ref() == tasks.get(&operation.logical_operation)
            && matches!(
                operation.binding,
                DeploymentOperationBindingV1::ProposedProvider { .. }
            )
    }));
}

#[test]
fn no_compatible_provider_remains_unresolved_instead_of_fabricating_placement() {
    let logical = fixture_logical();
    let snapshot = PlacementSnapshotV1::new(
        world(7),
        vec![provider(
            &logical.source.bundle,
            "node-a",
            2,
            5,
            false,
            true,
        )],
    )
    .unwrap();
    let deployment =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &snapshot, &tasks(&logical))
            .unwrap();

    assert!(deployment.selected_provider.is_none());
    assert!(deployment.operations.iter().all(|operation| matches!(
        &operation.binding,
        DeploymentOperationBindingV1::Unresolved { issues }
            if issues == &[DeploymentCompatibilityIssueV1::NoCompatibleProvider]
    )));
    assert!(deployment
        .operations
        .iter()
        .any(|operation| operation.requirements.residual_host_world));

    let mut wrong_world = deployment.clone();
    wrong_world.operations[0].task = Some(TaskIdentity::new(
        WorldId::new("other-world").unwrap(),
        TaskId::new("forged-unresolved-task").unwrap(),
    ));
    assert!(wrong_world.validate().is_err());
}

#[test]
fn snapshot_provider_generation_changes_both_source_and_deployment_digest() {
    let logical = fixture_logical();
    let tasks = tasks(&logical);
    let original_snapshot = PlacementSnapshotV1::new(
        world(7),
        vec![provider(&logical.source.bundle, "node-a", 2, 5, true, true)],
    )
    .unwrap();
    let replacement_snapshot = PlacementSnapshotV1::new(
        world(7),
        vec![provider(&logical.source.bundle, "node-a", 2, 6, true, true)],
    )
    .unwrap();
    let original =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &original_snapshot, &tasks)
            .unwrap();
    let replacement =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &replacement_snapshot, &tasks)
            .unwrap();

    assert_ne!(
        original_snapshot.digest().unwrap(),
        replacement_snapshot.digest().unwrap()
    );
    assert_ne!(original.digest().unwrap(), replacement.digest().unwrap());
    assert!(original
        .validate_trusted_snapshot(&logical, &replacement_snapshot, &tasks)
        .is_err());
}

#[test]
fn stale_world_task_substitution_and_provider_hierarchy_fail_closed() {
    let logical = fixture_logical();
    let snapshot = fixture_snapshot(&logical);
    let deployment =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &snapshot, &tasks(&logical))
            .unwrap();
    deployment.require_current_world(&world(7)).unwrap();
    assert!(deployment.require_current_world(&world(8)).is_err());

    let mut wrong_tasks = tasks(&logical);
    let first = logical.operations[0].id;
    wrong_tasks.insert(
        first,
        TaskIdentity::new(
            WorldId::new("other-world").unwrap(),
            TaskId::new("forged-task").unwrap(),
        ),
    );
    assert!(
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &snapshot, &wrong_tasks).is_err()
    );

    let mut forged = provider(&logical.source.bundle, "node-a", 2, 5, true, true);
    forged.binding.domain = DomainIdentity::new(
        NodeIdentity::new(
            WorldId::new("desk").unwrap(),
            NodeId::new("node-other").unwrap(),
            NodeGeneration::new(2).unwrap(),
        ),
        DomainId::new("project-host").unwrap(),
        DomainGeneration::new(3).unwrap(),
    );
    assert!(PlacementSnapshotV1::new(world(7), vec![forged]).is_err());

    let current = provider(&logical.source.bundle, "node-a", 2, 5, true, true);
    let conflicting_generation = provider(&logical.source.bundle, "node-a", 3, 6, true, true);
    assert!(PlacementSnapshotV1::new(world(7), vec![current, conflicting_generation],).is_err());

    let mut duplicate_identity = provider(&logical.source.bundle, "node-a", 2, 5, true, true);
    duplicate_identity.binding.implementation = artifact("conflicting-implementation");
    assert!(PlacementSnapshotV1::new(
        world(7),
        vec![
            provider(&logical.source.bundle, "node-a", 2, 5, true, true),
            duplicate_identity,
        ],
    )
    .is_err());

    let exact = provider(&logical.source.bundle, "node-a", 2, 5, true, true);
    let mut domain_conflict = provider(&logical.source.bundle, "node-a", 2, 6, true, true);
    let conflicting_domain = DomainIdentity::new(
        domain_conflict.binding.node.clone(),
        DomainId::new("project-host").unwrap(),
        DomainGeneration::new(9).unwrap(),
    );
    let conflicting_process = ProcessIdentity::new(
        conflicting_domain.clone(),
        ProcessId::new("runner").unwrap(),
        ProcessGeneration::new(4).unwrap(),
    );
    domain_conflict.binding.domain = conflicting_domain;
    domain_conflict.binding.process = Some(conflicting_process.clone());
    domain_conflict.binding.service = ResourceIdentity::new(
        ResourceOwner::Process {
            process: conflicting_process,
        },
        ResourceId::new("project/executor").unwrap(),
        ResourceGeneration::new(6).unwrap(),
    );
    assert!(PlacementSnapshotV1::new(world(7), vec![exact.clone(), domain_conflict]).is_err());

    let mut process_conflict = provider(&logical.source.bundle, "node-a", 2, 6, true, true);
    let conflicting_process = ProcessIdentity::new(
        process_conflict.binding.domain.clone(),
        ProcessId::new("runner").unwrap(),
        ProcessGeneration::new(9).unwrap(),
    );
    process_conflict.binding.process = Some(conflicting_process.clone());
    process_conflict.binding.service = ResourceIdentity::new(
        ResourceOwner::Process {
            process: conflicting_process,
        },
        ResourceId::new("project/executor").unwrap(),
        ResourceGeneration::new(6).unwrap(),
    );
    assert!(PlacementSnapshotV1::new(world(7), vec![exact.clone(), process_conflict]).is_err());

    let resource_conflict = provider(&logical.source.bundle, "node-a", 2, 6, true, true);
    assert!(PlacementSnapshotV1::new(world(7), vec![exact, resource_conflict]).is_err());
}

#[test]
fn project_paths_are_bundle_scoped_and_provider_choice_is_canonical() {
    let logical = fixture_logical();
    let wrong_bundle = artifact("unrelated-project-bundle");
    let snapshot = PlacementSnapshotV1::new(
        world(7),
        vec![provider(&wrong_bundle, "node-a", 2, 5, true, true)],
    )
    .unwrap();
    let rejected =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &snapshot, &tasks(&logical))
            .unwrap();
    assert!(rejected.selected_provider.is_none());
    assert!(rejected.rejected_providers[0]
        .issues
        .iter()
        .any(|issue| matches!(
            &issue.issue,
            DeploymentCompatibilityIssueV1::MissingProjectBundle { bundle }
                if bundle == &logical.source.bundle
        )));
    assert!(rejected.rejected_providers[0]
        .issues
        .iter()
        .any(|issue| matches!(
            &issue.issue,
            DeploymentCompatibilityIssueV1::MissingProjectPath { path }
                if path.bundle == logical.source.bundle
        )));

    let two = PlacementSnapshotV1::new(
        world(7),
        vec![
            provider(&logical.source.bundle, "node-z", 2, 5, true, true),
            provider(&logical.source.bundle, "node-a", 2, 5, true, true),
        ],
    )
    .unwrap();
    let mut deployment =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &two, &tasks(&logical)).unwrap();
    assert_eq!(
        deployment
            .selected_provider
            .as_ref()
            .unwrap()
            .node
            .node()
            .as_str(),
        "node-a"
    );
    let original_selected = deployment.selected_provider.take().unwrap();
    let later = std::mem::replace(&mut deployment.eligible_alternatives[0], original_selected);
    deployment.selected_provider = Some(later.clone());
    for operation in &mut deployment.operations {
        operation.binding = DeploymentOperationBindingV1::ProposedProvider {
            provider: later.clone(),
        };
    }
    assert!(deployment.validate().is_err());

    let mut contradictory =
        DeploymentPlanV1::from_snapshot_single_provider(&logical, &two, &tasks(&logical)).unwrap();
    let alternative = &mut contradictory.eligible_alternatives[0];
    let conflicting_node = NodeIdentity::new(
        WorldId::new("desk").unwrap(),
        NodeId::new("node-a").unwrap(),
        NodeGeneration::new(99).unwrap(),
    );
    let conflicting_domain = DomainIdentity::new(
        conflicting_node.clone(),
        DomainId::new("project-host").unwrap(),
        DomainGeneration::new(3).unwrap(),
    );
    let conflicting_process = ProcessIdentity::new(
        conflicting_domain.clone(),
        ProcessId::new("runner").unwrap(),
        ProcessGeneration::new(4).unwrap(),
    );
    alternative.node = conflicting_node;
    alternative.domain = conflicting_domain;
    alternative.process = Some(conflicting_process.clone());
    alternative.service = ResourceIdentity::new(
        ResourceOwner::Process {
            process: conflicting_process,
        },
        ResourceId::new("project/executor-alt").unwrap(),
        ResourceGeneration::new(5).unwrap(),
    );
    assert!(contradictory.validate().is_err());
}

#[test]
fn unknown_fields_versions_and_semantic_substitutions_are_rejected() {
    let logical = fixture_logical();
    let deployment = DeploymentPlanV1::hosted(&logical).unwrap();
    let mut value = serde_json::to_value(&deployment).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("unsupported".to_string(), json!(true));
    assert!(DeploymentPlanV1::decode(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut version = serde_json::to_value(&deployment).unwrap();
    version["schema_version"] = json!(2);
    assert!(DeploymentPlanV1::decode(&serde_json::to_vec(&version).unwrap()).is_err());

    let mut substituted = deployment.clone();
    substituted.logical_hgraph = artifact("different-logical-graph");
    substituted.validate().unwrap();
    assert!(substituted.validate_trusted_hosted(&logical).is_err());
    assert_ne!(deployment.digest().unwrap(), substituted.digest().unwrap());

    let residual = substituted
        .operations
        .iter_mut()
        .find(|operation| operation.requirements.residual_host_world)
        .unwrap();
    residual.requirements.residual_host_world = false;
    assert!(substituted.validate_trusted_hosted(&logical).is_err());

    let mut mixed_bundle = deployment.clone();
    mixed_bundle.operations[0].requirements.project_bundle = artifact("different-bundle");
    assert!(mixed_bundle.validate().is_err());
}
