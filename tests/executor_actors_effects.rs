//! Compatibility actor-label and semantic effect-model tests.
//!
//! These cover stable diagnostic labels, persistent-environment identity,
//! effect-declaration parsing, and the compatibility conflict predicate.
//! Production readiness itself is derived from executable graph inputs.

use o_lang::executor::{
    effect_summary_for_plan_node, ActorResourceId, ActorTable, DeclaredPurity, EffectConfidence,
    EffectDeclaration, EffectSummary, EffectTrustPolicy, Fallibility, ResourceKey,
};
use o_lang::ir::{BackendRegistry, OIr, OIrProgram, PlanNodeId, PlanNodeKind};
use o_lang::world::{
    ArtifactId, ArtifactPublicationIdentity, AttemptGeneration, DomainGeneration, DomainId,
    DomainIdentity, NodeGeneration, NodeId, NodeIdentity, ResourceGeneration, ResourceId,
    ResourceIdentity, ResourceOwner, TaskAttemptIdentity, TaskId, WorldEpoch, WorldId,
    WorldIdentity,
};

fn shim(lang: &str) -> o_lang::ir::BackendInterface {
    BackendRegistry::global().interface_for(lang)
}

#[test]
fn ephemeral_blocks_get_unique_actor_identities() {
    // Two ephemeral (env_id == u32::MAX) Python blocks receive distinct
    // diagnostic labels; this assertion makes no scheduling claim.
    let program = OIrProgram {
        nodes: vec![
            OIr::Exec {
                lang: "python".into(),
                env_id: u32::MAX,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 1".into())],
            },
            OIr::Exec {
                lang: "python".into(),
                env_id: u32::MAX,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 2".into())],
            },
        ],
    };
    let plan = program.plan();
    let actors = ActorTable::build(&plan, |_, _| 0);

    let exec_ids: Vec<_> = plan
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, PlanNodeKind::Exec { .. }))
        .map(|n| n.id)
        .collect();
    assert_eq!(exec_ids.len(), 2);

    let a = actors.actor_for(exec_ids[0]).expect("actor for first exec");
    let b = actors
        .actor_for(exec_ids[1])
        .expect("actor for second exec");

    assert!(a.is_ephemeral() && b.is_ephemeral());
    assert_ne!(
        a.ephemeral_instance, b.ephemeral_instance,
        "each ephemeral block must get a unique instance id: {a:?} vs {b:?}"
    );
}

#[test]
fn persistent_same_env_shares_one_actor() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Exec {
                lang: "python".into(),
                env_id: 3,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 1".into())],
            },
            OIr::Exec {
                lang: "python".into(),
                env_id: 3,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 2".into())],
            },
            OIr::Exec {
                lang: "python".into(),
                env_id: 4,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 3".into())],
            },
        ],
    };
    let plan = program.plan();
    let actors = ActorTable::build(&plan, |_, _| 0);
    let exec_ids: Vec<_> = plan
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, PlanNodeKind::Exec { .. }))
        .map(|n| n.id)
        .collect();

    let a = actors.actor_for(exec_ids[0]).unwrap();
    let b = actors.actor_for(exec_ids[1]).unwrap();
    let c = actors.actor_for(exec_ids[2]).unwrap();

    // Same (lang, env) → same persistent actor identity.
    assert_eq!(a.persistent_id(), b.persistent_id());
    assert_eq!(a.persistent_id(), Some(("python".to_string(), 3)));
    // Different env of the same language → different actor.
    assert_ne!(a.persistent_id(), c.persistent_id());
}

#[test]
fn effect_declaration_parses_all_forms() {
    let decl = EffectDeclaration::parse(Some(
        "lazy, effects=unknown, reads=project:src+host:/etc/hosts, writes=env:PATH, serial=host",
    ))
    .unwrap();
    assert_eq!(decl.purity, Some(DeclaredPurity::Unknown));
    assert!(decl.reads.contains(&ResourceKey::ProjectPath("src".into())));
    assert!(decl
        .reads
        .contains(&ResourceKey::HostPath("/etc/hosts".into())));
    assert!(decl.writes.contains(&ResourceKey::EnvVar("PATH".into())));
    assert!(decl.serial_host);

    // Unknown / unrelated attributes must not break parsing.
    let plain = EffectDeclaration::parse(Some("lazy, defer")).unwrap();
    assert!(plain.is_empty());
}

#[test]
fn pure_declaration_cannot_upgrade_unknown_base() {
    let decl = EffectDeclaration::parse(Some("effects=pure")).unwrap();
    let error = decl
        .apply_checked(EffectSummary::unknown(), EffectTrustPolicy::Strict)
        .unwrap_err();
    assert!(error.contains("cannot upgrade"), "{error}");
}

#[test]
fn redundant_pure_declaration_preserves_verified_purity() {
    let decl = EffectDeclaration::parse(Some("effects=pure")).unwrap();
    let effective = decl
        .apply_checked(EffectSummary::pure(), EffectTrustPolicy::Strict)
        .unwrap();
    assert!(effective.is_verified_pure_infallible());
}

#[test]
fn effect_conflict_predicate_reports_resource_hazards() {
    // Read/read is not a write hazard in the compatibility predicate.
    let mut read_a = EffectSummary::pure();
    read_a.reads.insert(ResourceKey::ProjectPath("data".into()));
    let mut read_b = EffectSummary::pure();
    read_b.reads.insert(ResourceKey::ProjectPath("data".into()));
    assert!(!read_a.conflicts_with(&read_b));

    // Write vs read on the same resource is a conflict.
    let mut write = EffectSummary::pure();
    write.writes.insert(ResourceKey::ProjectPath("data".into()));
    assert!(write.conflicts_with(&read_a));

    // Unknown conflicts conservatively.
    let unknown = EffectSummary::unknown();
    assert!(unknown.conflicts_with(&EffectSummary::unknown()));

    // HostWorld is an umbrella over precise host resources.
    assert!(unknown.conflicts_with(&read_a));

    // Two pure, resource-free summaries never conflict.
    assert!(!EffectSummary::pure().conflicts_with(&EffectSummary::pure()));
}

#[test]
fn unknown_summary_has_explicit_host_world_transition() {
    let summary = EffectSummary::unknown();
    assert_eq!(summary.confidence, EffectConfidence::Conservative);
    assert_eq!(summary.fallibility, Fallibility::MayFail);
    assert!(summary.unknown);
    assert!(summary.reads.contains(&ResourceKey::HostWorld));
    assert!(summary.writes.contains(&ResourceKey::HostWorld));
}

#[test]
fn actor_state_is_typed_and_added_to_both_access_sets() {
    let actor = ActorResourceId::new("python", 7);
    let summary = EffectSummary::unknown().with_actor_state(actor.clone());
    let resource = ResourceKey::ActorState(actor.clone());
    assert_eq!(summary.actor_state, Some(actor));
    assert!(summary.reads.contains(&resource));
    assert!(summary.writes.contains(&resource));
    assert!(summary.accessed_resources().contains(&resource));
    assert_eq!(resource.to_string(), "actor:python[7]");
}

#[test]
fn unknown_declaration_downgrades_verified_pure_base() {
    let decl = EffectDeclaration::parse(Some("effects=unknown")).unwrap();
    let summary = decl
        .apply_checked(EffectSummary::pure(), EffectTrustPolicy::Strict)
        .unwrap();
    assert_eq!(summary.confidence, EffectConfidence::Conservative);
    assert_eq!(summary.fallibility, Fallibility::MayFail);
    assert!(summary.unknown);
    assert!(summary.reads.contains(&ResourceKey::HostWorld));
    assert!(summary.writes.contains(&ResourceKey::HostWorld));
}

#[test]
fn user_host_resources_add_constraints_without_losing_world_umbrella() {
    let decl =
        EffectDeclaration::parse(Some("reads=project:input, writes=env:OUTPUT, serial=host"))
            .unwrap();
    let summary = decl
        .apply_checked(EffectSummary::pure(), EffectTrustPolicy::Strict)
        .unwrap();

    assert_eq!(summary.confidence, EffectConfidence::UserDeclared);
    assert!(!summary.unknown);
    assert!(summary
        .reads
        .contains(&ResourceKey::ProjectPath("input".into())));
    assert!(summary
        .writes
        .contains(&ResourceKey::EnvVar("OUTPUT".into())));
    assert!(summary.reads.contains(&ResourceKey::HostWorld));
    assert!(summary.writes.contains(&ResourceKey::HostWorld));
    assert!(!summary.is_verified_pure_infallible());
}

#[test]
fn malformed_effect_resource_syntax_is_rejected() {
    for attr in [
        "effects=trusted",
        "serial=actor",
        "reads=project:/absolute",
        "writes=env:bad-name",
        "reads=actor:python[*]",
        "reads=",
    ] {
        assert!(
            EffectDeclaration::parse(Some(attr)).is_err(),
            "{attr} should be rejected"
        );
    }
}

#[test]
fn plan_node_effects_distinguish_scope_pure_and_control_state() {
    let text = effect_summary_for_plan_node(PlanNodeId(0), &PlanNodeKind::Text).unwrap();
    assert!(text.is_verified_pure_infallible());

    let load =
        effect_summary_for_plan_node(PlanNodeId(1), &PlanNodeKind::Load { name: "x".into() })
            .unwrap();
    assert_eq!(load.confidence, EffectConfidence::Verified);
    assert_eq!(load.fallibility, Fallibility::MayFail);
    assert!(load.reads.contains(&ResourceKey::ScopeBinding("x".into())));

    let store =
        effect_summary_for_plan_node(PlanNodeId(2), &PlanNodeKind::Store { name: "x".into() })
            .unwrap();
    assert_eq!(store.fallibility, Fallibility::Infallible);
    assert!(store
        .writes
        .contains(&ResourceKey::ScopeBinding("x".into())));

    let call = effect_summary_for_plan_node(
        PlanNodeId(3),
        &PlanNodeKind::Call {
            fn_name: "scope".into(),
            mode: o_lang::ir::InvokeMode::Eager,
            arg_count: 0,
        },
    )
    .unwrap();
    assert!(call.reads.contains(&ResourceKey::HostWorld));
    assert!(call.reads.contains(&ResourceKey::EvaluatorState));
    assert!(call.writes.contains(&ResourceKey::HostWorld));
    assert!(call.writes.contains(&ResourceKey::EvaluatorState));
}

#[test]
fn plan_node_effects_trust_only_known_inline_renderers() {
    let html = PlanNodeKind::Exec {
        lang: "html".into(),
        env_id: u32::MAX,
        attr: None,
        backend: shim("html"),
    };
    let summary = effect_summary_for_plan_node(PlanNodeId(0), &html).unwrap();
    assert!(summary.is_verified_pure_infallible());

    let deferred_html = PlanNodeKind::Exec {
        lang: "html".into(),
        env_id: u32::MAX,
        attr: Some("defer".into()),
        backend: shim("html"),
    };
    let summary = effect_summary_for_plan_node(PlanNodeId(1), &deferred_html).unwrap();
    assert_eq!(summary.confidence, EffectConfidence::Conservative);
    assert!(summary.reads.contains(&ResourceKey::HostWorld));
    assert!(summary.reads.contains(&ResourceKey::EvaluatorState));
}

#[test]
fn persistent_shim_uses_canonical_actor_resource() {
    let python = PlanNodeKind::Exec {
        lang: "py".into(),
        env_id: 9,
        attr: None,
        backend: shim("py"),
    };
    let summary = effect_summary_for_plan_node(PlanNodeId(0), &python).unwrap();
    let actor = ActorResourceId::new("python", 9);
    let resource = ResourceKey::ActorState(actor.clone());
    assert_eq!(summary.actor_state, Some(actor));
    assert!(summary.reads.contains(&resource));
    assert!(summary.writes.contains(&resource));
    assert!(summary.reads.contains(&ResourceKey::HostWorld));
    assert!(summary.reads.contains(&ResourceKey::EvaluatorState));
}

#[test]
fn explicit_inline_environment_is_conservatively_actor_stateful() {
    let inline = PlanNodeKind::Exec {
        lang: "html".into(),
        env_id: 7,
        attr: None,
        backend: shim("html"),
    };
    let summary = effect_summary_for_plan_node(PlanNodeId(5), &inline).unwrap();
    let actor = ActorResourceId::new("html", 7);
    let resource = ResourceKey::ActorState(actor.clone());
    assert_eq!(summary.actor_state, Some(actor));
    assert!(summary.reads.contains(&resource));
    assert!(summary.writes.contains(&resource));
    assert!(!summary.is_verified_pure_infallible());
}

#[test]
fn governed_vocabulary_is_precise_and_does_not_alias_ambient_hostworld() {
    let world = WorldIdentity::new(WorldId::new("desk").unwrap(), WorldEpoch::new(4).unwrap());
    let node = NodeIdentity::new(
        world.world().clone(),
        NodeId::new("node-a").unwrap(),
        NodeGeneration::new(2).unwrap(),
    );
    let domain = DomainIdentity::new(
        node.clone(),
        DomainId::new("linux-provider").unwrap(),
        DomainGeneration::new(7).unwrap(),
    );
    let artifact = ArtifactPublicationIdentity::new(
        world.clone(),
        ArtifactId::from_sha256("a".repeat(64)).unwrap(),
    );
    let resource = ResourceIdentity::new(
        ResourceOwner::Node { node: node.clone() },
        ResourceId::new("cpu/slot-0").unwrap(),
        ResourceGeneration::new(1).unwrap(),
    );
    let task = TaskAttemptIdentity::new(
        world.world().clone(),
        TaskId::new("build").unwrap(),
        AttemptGeneration::new(3).unwrap(),
    );
    let governed = [
        ResourceKey::WorldState(world),
        ResourceKey::NodeState(node),
        ResourceKey::DomainState(domain),
        ResourceKey::GovernedResource(resource),
        ResourceKey::TaskState(task),
        ResourceKey::ArtifactState(artifact),
    ];

    for resource in &governed {
        assert!(resource.is_governed_resource(), "{resource}");
        assert!(!resource.is_host_resource(), "{resource}");
    }
    assert_eq!(governed[0].to_string(), "world-state:desk@4");
    assert_eq!(governed[1].to_string(), "node-state:desk/node:node-a@2");
    assert_eq!(
        governed[2].to_string(),
        "domain-state:desk/node:node-a@2/domain:linux-provider@7"
    );

    let mut precise_model = EffectSummary::pure();
    precise_model.reads.insert(governed[2].clone());
    precise_model.writes.insert(governed[2].clone());
    assert!(!precise_model.reads.contains(&ResourceKey::HostWorld));
    assert!(!precise_model.conflicts_with(&EffectSummary::unknown()));

    let mut same_domain = EffectSummary::pure();
    same_domain.reads.insert(governed[2].clone());
    assert!(precise_model.conflicts_with(&same_domain));

    assert!(EffectDeclaration::parse(Some("reads=world:desk@4")).is_err());
}
