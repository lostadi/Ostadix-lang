//! Canonical World PR8-1 project-profile LogicalHGraph coverage.
//!
//! These tests exercise a hosted logical schema only. They do not constitute
//! placement, runtime, recovery, native/O-core, Governor, or World Alpha
//! evidence.

use std::path::{Path, PathBuf};

use o_lang::project::{
    self, build_project_hgraph, LogicalAuthorityRequirementV1, LogicalDependencyKindV1,
    LogicalEffectConfidenceV1, LogicalFallibilityV1, LogicalHGraphError, LogicalHGraphV1,
    LogicalOperationIdV1, LogicalOperationKindV1, LogicalResourceV1, ProjectBundle, RoutePolicy,
};
use o_lang::world::{ArtifactId, WorldEpoch, WorldId, WorldIdentity};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph")
}

fn fixture_bundle() -> ProjectBundle {
    project::assemble(&fixture_path(), "pr7-project-hgraph", &[]).unwrap()
}

fn fixture_project() -> project::ProjectHGraph {
    build_project_hgraph(&fixture_bundle(), Some("main"), None).unwrap()
}

fn fixture_logical() -> LogicalHGraphV1 {
    fixture_project().logical_v1().unwrap()
}

fn assert_structural_rejection(changed: &LogicalHGraphV1) {
    assert!(changed.validate().is_err());
    assert!(changed.canonical_bytes().is_err());
    assert!(changed.digest().is_err());
}

#[test]
fn directory_and_lifted_project_have_identical_canonical_bytes_and_digest() {
    let directory_bundle = fixture_bundle();
    let lifted = project::lower::lower_to_o_validated(&directory_bundle).unwrap();
    let extracted = project::lower::extract_bundle_from_o(&lifted).unwrap();

    let directory = build_project_hgraph(&directory_bundle, Some("main"), None).unwrap();
    let embedded = build_project_hgraph(&extracted, Some("main"), None).unwrap();
    let directory_logical = directory.logical_v1().unwrap();
    let embedded_logical = embedded.logical_v1().unwrap();

    assert_eq!(directory_logical, embedded_logical);
    assert_eq!(
        directory_logical.canonical_bytes().unwrap(),
        embedded_logical.canonical_bytes().unwrap()
    );
    assert_eq!(
        directory_logical.digest().unwrap(),
        embedded_logical.digest().unwrap()
    );
    assert_eq!(
        directory_logical.canonical_bytes().unwrap(),
        directory.logical_v1().unwrap().canonical_bytes().unwrap(),
        "repeated construction must be deterministic"
    );
}

#[test]
fn project_profile_v1_digest_is_pinned() {
    assert_eq!(
        fixture_logical().digest().unwrap().as_sha256(),
        "5f8815019223109644bd20e765983872e133dd3a3b038d52c04155271fb96216",
        "a canonical schema change requires a new version and reviewed vector"
    );
}

#[test]
fn source_formatting_changes_the_exact_bundle_and_logical_identity() {
    let original_bundle = fixture_bundle();
    let original = build_project_hgraph(&original_bundle, Some("main"), None).unwrap();
    let original_logical = original.logical_v1().unwrap();

    let mut reformatted_bundle = original_bundle;
    let manifest = reformatted_bundle
        .files
        .iter_mut()
        .find(|file| file.path == "olang.project.toml")
        .unwrap();
    manifest
        .bytes
        .extend_from_slice(b"\n# formatting-only bundle change\n");
    manifest.content_hash = format!("{:x}", Sha256::digest(&manifest.bytes));

    let reformatted = build_project_hgraph(&reformatted_bundle, Some("main"), None).unwrap();
    let reformatted_logical = reformatted.logical_v1().unwrap();

    assert_eq!(original.plan.operations, reformatted.plan.operations);
    assert_eq!(original.plan.roots, reformatted.plan.roots);
    assert_ne!(original.plan.bundle_digest, reformatted.plan.bundle_digest);
    assert_ne!(
        original_logical.digest().unwrap(),
        reformatted_logical.digest().unwrap()
    );
}

#[test]
fn canonical_decode_round_trips_and_strict_mode_rejects_noncanonical_json() {
    let logical = fixture_logical();
    let canonical = logical.canonical_bytes().unwrap();

    assert_eq!(
        LogicalHGraphV1::decode_canonical(&canonical).unwrap(),
        logical
    );
    assert_eq!(
        LogicalHGraphV1::decode_canonical(&canonical)
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        canonical
    );

    let pretty = serde_json::to_vec_pretty(&logical).unwrap();
    assert_ne!(pretty, canonical);
    assert_eq!(LogicalHGraphV1::decode(&pretty).unwrap(), logical);
    assert!(matches!(
        LogicalHGraphV1::decode_canonical(&pretty),
        Err(LogicalHGraphError::NonCanonicalEncoding)
    ));
}

#[test]
fn unknown_fields_versions_and_operation_variants_fail_closed() {
    let logical = fixture_logical();
    let canonical_value = serde_json::to_value(&logical).unwrap();

    let mut unknown_field = canonical_value.clone();
    unknown_field
        .as_object_mut()
        .unwrap()
        .insert("unsupported".to_string(), json!(true));
    assert!(LogicalHGraphV1::decode(&serde_json::to_vec(&unknown_field).unwrap()).is_err());

    let mut unknown_version = canonical_value.clone();
    unknown_version["schema_version"] = json!(2);
    let error =
        LogicalHGraphV1::decode(&serde_json::to_vec(&unknown_version).unwrap()).unwrap_err();
    assert!(error.to_string().contains("unsupported schema version 2"));

    let mut unknown_variant = canonical_value;
    unknown_variant["operations"][0]["kind"]["kind"] =
        Value::String("teleport_project".to_string());
    assert!(LogicalHGraphV1::decode(&serde_json::to_vec(&unknown_variant).unwrap()).is_err());
}

#[test]
fn hosted_effects_preserve_host_world_and_mint_no_authority_requirements() {
    let logical = fixture_logical();
    let mut unknown_effects = 0usize;
    let mut value_dependencies = 0usize;
    let mut success_dependencies = 0usize;

    for operation in &logical.operations {
        assert!(
            operation.authority_requirements.is_empty(),
            "hosted project lowering must not mint authority requirements"
        );
        for resource in operation
            .effects
            .reads
            .iter()
            .chain(&operation.effects.writes)
        {
            assert!(
                !resource.is_governed(),
                "the fixture has no trusted governed-resource lowering"
            );
        }
        if operation.effects.unknown {
            unknown_effects += 1;
            assert!(operation
                .effects
                .reads
                .contains(&LogicalResourceV1::HostWorld));
            assert!(operation
                .effects
                .writes
                .contains(&LogicalResourceV1::HostWorld));
        }
        for dependency in &operation.dependencies {
            match dependency.requirement {
                LogicalDependencyKindV1::Value => value_dependencies += 1,
                LogicalDependencyKindV1::Success => success_dependencies += 1,
            }
        }
    }

    assert!(unknown_effects > 0);
    assert!(value_dependencies > 0);
    assert!(success_dependencies > 0);
}

#[test]
fn logical_route_runtime_requirements_are_source_bound_and_digest_relevant() {
    let project = fixture_project();
    let logical = project.logical_v1().unwrap();
    let run = logical
        .operations
        .iter()
        .find(|operation| {
            matches!(
                &operation.kind,
                LogicalOperationKindV1::RunRoute { route_id } if route_id == "impl-a"
            )
        })
        .unwrap();
    let facts = run.route_facts.as_ref().unwrap();
    assert_eq!(facts.executable.as_deref(), Some("sh"));
    assert_eq!(facts.evaluator, None);

    let mut substituted = logical.clone();
    for operation in substituted.operations.iter_mut().filter(|operation| {
        matches!(
            &operation.kind,
            LogicalOperationKindV1::BuildRoute { route_id }
                | LogicalOperationKindV1::RunRoute { route_id }
                if route_id == "impl-a"
        )
    }) {
        operation.route_facts.as_mut().unwrap().executable = Some("forged-runtime".to_string());
    }
    substituted.validate().unwrap();
    assert_ne!(logical.digest().unwrap(), substituted.digest().unwrap());
    assert!(substituted.validate_trusted_project(&project).is_err());
}

#[test]
fn logical_effect_resources_match_the_scheduler_expansion() {
    let mut bundle = fixture_bundle();
    bundle
        .routes
        .iter_mut()
        .find(|route| route.id == "impl-a")
        .unwrap()
        .effects
        .reads
        .push("network:api.example".to_string());
    let project = build_project_hgraph(&bundle, Some("main"), None).unwrap();
    let logical = project.logical_v1().unwrap();

    for (planned, encoded) in project.plan.operations.iter().zip(&logical.operations) {
        let mut expected_reads = planned
            .effects
            .reads
            .iter()
            .map(LogicalResourceV1::from)
            .collect::<Vec<_>>();
        let mut expected_writes = planned
            .effects
            .writes
            .iter()
            .map(LogicalResourceV1::from)
            .collect::<Vec<_>>();
        let expected_scheduler_resources = planned
            .effects
            .accessed_resources()
            .iter()
            .map(LogicalResourceV1::from)
            .collect::<Vec<_>>();
        expected_reads.sort();
        expected_reads.dedup();
        expected_writes.sort();
        expected_writes.dedup();
        assert_eq!(encoded.effects.reads, expected_reads);
        assert_eq!(encoded.effects.writes, expected_writes);
        assert_eq!(
            encoded.effects.scheduler_resources,
            expected_scheduler_resources
        );
    }

    let impl_a = logical
        .operations
        .iter()
        .find(|operation| {
            matches!(
                &operation.kind,
                LogicalOperationKindV1::RunRoute { route_id } if route_id == "impl-a"
            )
        })
        .unwrap();
    assert!(impl_a.effects.reads.contains(&LogicalResourceV1::Network {
        endpoint: "api.example".to_string(),
    }));
    assert!(impl_a
        .effects
        .scheduler_resources
        .contains(&LogicalResourceV1::NetworkUnknown));
}

#[test]
fn hosted_profile_rejects_forged_governed_resources_and_authority() {
    let logical = fixture_logical();
    let world = WorldIdentity::new(
        WorldId::new("forged-world").unwrap(),
        WorldEpoch::new(1).unwrap(),
    );
    let governed = LogicalResourceV1::WorldState { world };

    let mut resource_forgery = logical.clone();
    resource_forgery.operations[0]
        .effects
        .reads
        .push(governed.clone());
    resource_forgery.operations[0].effects.reads.sort();
    resource_forgery.operations[0].effects.reads.dedup();
    assert_structural_rejection(&resource_forgery);

    let mut authority_forgery = logical;
    authority_forgery.operations[0]
        .authority_requirements
        .push(LogicalAuthorityRequirementV1 {
            resource: governed,
            right: "write".to_string(),
        });
    assert_structural_rejection(&authority_forgery);
}

#[test]
fn hosted_profile_rejects_host_world_removal_and_effect_flag_tampering() {
    let logical = fixture_logical();
    let unknown_index = logical
        .operations
        .iter()
        .position(|operation| operation.effects.unknown)
        .unwrap();

    let mut missing_host_world = logical.clone();
    missing_host_world.operations[unknown_index]
        .effects
        .reads
        .retain(|resource| resource != &LogicalResourceV1::HostWorld);
    assert_structural_rejection(&missing_host_world);

    let mut missing_host_world_write = logical.clone();
    missing_host_world_write.operations[unknown_index]
        .effects
        .writes
        .retain(|resource| resource != &LogicalResourceV1::HostWorld);
    assert_structural_rejection(&missing_host_world_write);

    let mut mutations: Vec<Box<dyn FnOnce(&mut LogicalHGraphV1)>> = vec![
        Box::new(move |graph| graph.operations[unknown_index].effects.unknown = false),
        Box::new(move |graph| graph.operations[unknown_index].effects.deterministic = true),
        Box::new(move |graph| graph.operations[unknown_index].effects.network = true),
        Box::new(move |graph| graph.operations[unknown_index].effects.spawn = true),
        Box::new(move |graph| graph.operations[unknown_index].effects.clock = true),
        Box::new(move |graph| {
            graph.operations[unknown_index].effects.fallibility = LogicalFallibilityV1::Infallible
        }),
        Box::new(move |graph| {
            graph.operations[unknown_index].effects.confidence = LogicalEffectConfidenceV1::Verified
        }),
    ];
    for mutate in mutations.drain(..) {
        let mut forged = logical.clone();
        mutate(&mut forged);
        assert_structural_rejection(&forged);
    }
}

#[test]
fn trusted_project_comparison_rejects_a_digest_substitution() {
    let project = fixture_project();
    let logical = project.logical_v1().unwrap();
    logical.validate_trusted_project(&project).unwrap();

    let mut substituted = logical.clone();
    substituted.source.bundle = ArtifactId::from_sha256("ab".repeat(32)).unwrap();
    substituted.validate().unwrap();
    assert!(substituted.validate_trusted_project(&project).is_err());
    assert_ne!(logical.digest().unwrap(), substituted.digest().unwrap());
}

#[test]
fn structural_substitutions_fail_closed_while_valid_source_and_policy_changes_rehash() {
    let logical = fixture_logical();

    let mut operation_change = logical.clone();
    let operation = operation_change
        .operations
        .iter_mut()
        .find(|operation| matches!(operation.kind, LogicalOperationKindV1::BuildRoute { .. }))
        .unwrap();
    let route_id = match &operation.kind {
        LogicalOperationKindV1::BuildRoute { route_id } => route_id.clone(),
        _ => unreachable!(),
    };
    operation.kind = LogicalOperationKindV1::RunRoute { route_id };
    assert_structural_rejection(&operation_change);

    let mut dependency_change = logical.clone();
    let dependency = dependency_change
        .operations
        .iter_mut()
        .find_map(|operation| operation.dependencies.first_mut())
        .unwrap();
    dependency.requirement = match dependency.requirement {
        LogicalDependencyKindV1::Value => LogicalDependencyKindV1::Success,
        LogicalDependencyKindV1::Success => LogicalDependencyKindV1::Value,
    };
    assert_structural_rejection(&dependency_change);

    let mut effect_change = logical.clone();
    effect_change.operations[0].effects.clock = !effect_change.operations[0].effects.clock;
    assert_structural_rejection(&effect_change);

    let mut resource_change = logical.clone();
    let effects = &mut resource_change
        .operations
        .iter_mut()
        .find(|operation| operation.effects.unknown)
        .unwrap()
        .effects;
    effects.reads.push(LogicalResourceV1::Stdio);
    effects.reads.sort();
    effects.reads.dedup();
    assert_structural_rejection(&resource_change);

    let mut source_change = logical.clone();
    source_change.source.project_name.push_str("-variant");
    source_change.validate().unwrap();
    assert_ne!(logical.digest().unwrap(), source_change.digest().unwrap());
    assert!(source_change
        .validate_trusted_project(&fixture_project())
        .is_err());

    let all_project =
        build_project_hgraph(&fixture_bundle(), Some("main"), Some(RoutePolicy::All)).unwrap();
    let all_logical = all_project.logical_v1().unwrap();
    assert_ne!(logical.source.policy, all_logical.source.policy);
    assert_ne!(logical.digest().unwrap(), all_logical.digest().unwrap());

    let mut root_substitution = logical.clone();
    root_substitution.roots = vec![LogicalOperationIdV1(0)];
    assert!(root_substitution.validate().is_err());
    assert!(root_substitution.digest().is_err());
}
