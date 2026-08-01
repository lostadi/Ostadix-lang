use o_lang::world::{
    ArtifactId, ArtifactPublicationIdentity, AttemptGeneration, DomainGeneration, DomainId,
    DomainIdentity, NodeGeneration, NodeId, NodeIdentity, ResourceId, ResourceIdentity,
    ResourceOwner, TaskAttemptIdentity, TaskId, WorldEpoch, WorldId, WorldIdentity,
    WorldIdentityError,
};

fn world_id() -> WorldId {
    WorldId::new("desk").unwrap()
}

fn world(epoch: u64) -> WorldIdentity {
    WorldIdentity::new(world_id(), WorldEpoch::new(epoch).unwrap())
}

fn node(generation: u64) -> NodeIdentity {
    NodeIdentity::new(
        world_id(),
        NodeId::new("node-a").unwrap(),
        NodeGeneration::new(generation).unwrap(),
    )
}

fn domain(node_generation: u64, generation: u64) -> DomainIdentity {
    DomainIdentity::new(
        node(node_generation),
        DomainId::new("linux-provider").unwrap(),
        DomainGeneration::new(generation).unwrap(),
    )
}

#[test]
fn identities_are_bounded_canonical_and_serializable() {
    assert!(WorldId::new("").is_err());
    assert!(WorldId::new(".").is_err());
    assert!(WorldId::new("..").is_err());
    assert!(NodeId::new(".").is_err());
    assert!(DomainId::new("..").is_err());
    assert!(TaskId::new(".").is_err());
    assert!(WorldId::new("desk/escape").is_err());
    assert!(NodeId::new("node a").is_err());
    assert!(WorldEpoch::new(0).is_err());
    assert!(NodeGeneration::new(0).is_err());
    assert!(DomainGeneration::new(0).is_err());
    assert!(AttemptGeneration::new(0).is_err());
    assert!(ResourceId::new("/cpu/slot-0").is_err());
    assert!(ResourceId::new("cpu/../slot-0").is_err());
    assert!(ArtifactId::from_sha256("A".repeat(64)).is_err());

    let identity = domain(2, 7);
    let encoded = serde_json::to_string(&identity).unwrap();
    assert_eq!(
        serde_json::from_str::<DomainIdentity>(&encoded).unwrap(),
        identity
    );

    let invalid = r#"{"world":"desk","epoch":0}"#;
    assert!(serde_json::from_str::<WorldIdentity>(invalid).is_err());
    let unknown = r#"{"world":"desk","epoch":4,"authority":"forged"}"#;
    assert!(serde_json::from_str::<WorldIdentity>(unknown).is_err());

    let artifact = ArtifactId::from_sha256("a".repeat(64)).unwrap();
    assert_eq!(
        artifact.to_string().parse::<ArtifactId>().unwrap(),
        artifact
    );
    assert!(serde_json::from_str::<ArtifactId>(&format!("\"sha256:{}\"", "a".repeat(64))).is_err());
}

#[test]
fn independent_world_node_and_domain_generations_fence_stale_references() {
    let world_one = world(1);
    let world_two = world(2);
    assert_ne!(world_one, world_two);
    assert!(matches!(
        world_two.require_current(&world_one),
        Err(WorldIdentityError::StaleGeneration {
            kind: "world epoch",
            expected: 2,
            got: 1
        })
    ));

    let node_one = node(1);
    let node_two = node(2);
    assert_ne!(node_one, node_two);
    assert!(matches!(
        node_two.require_current(&node_one),
        Err(WorldIdentityError::StaleGeneration {
            kind: "node generation",
            expected: 2,
            got: 1
        })
    ));

    let domain_one = domain(2, 1);
    let domain_two = domain(2, 2);
    assert_ne!(domain_one, domain_two);
    assert!(matches!(
        domain_two.require_current(&domain_one),
        Err(WorldIdentityError::StaleGeneration {
            kind: "domain generation",
            expected: 2,
            got: 1
        })
    ));

    let coincident_name_on_other_node = DomainIdentity::new(
        NodeIdentity::new(
            world_id(),
            NodeId::new("node-b").unwrap(),
            NodeGeneration::new(2).unwrap(),
        ),
        DomainId::new("linux-provider").unwrap(),
        DomainGeneration::new(2).unwrap(),
    );
    assert!(matches!(
        domain_two.require_current(&coincident_name_on_other_node),
        Err(WorldIdentityError::IdentityMismatch { kind: "node", .. })
    ));
}

#[test]
fn owner_scoped_resources_tasks_and_artifacts_do_not_revive() {
    let resource_one = ResourceIdentity::new(
        ResourceOwner::Node { node: node(1) },
        ResourceId::new("cpu/slot-0").unwrap(),
    );
    let resource_two = ResourceIdentity::new(
        ResourceOwner::Node { node: node(2) },
        ResourceId::new("cpu/slot-0").unwrap(),
    );
    assert!(matches!(
        resource_two.require_current(&resource_one),
        Err(WorldIdentityError::StaleGeneration {
            kind: "node generation",
            expected: 2,
            got: 1
        })
    ));

    let attempt_one = TaskAttemptIdentity::new(
        world_id(),
        TaskId::new("build").unwrap(),
        AttemptGeneration::new(1).unwrap(),
    );
    let attempt_two = TaskAttemptIdentity::new(
        world_id(),
        TaskId::new("build").unwrap(),
        AttemptGeneration::new(2).unwrap(),
    );
    assert!(matches!(
        attempt_two.require_current(&attempt_one),
        Err(WorldIdentityError::StaleGeneration {
            kind: "task attempt",
            expected: 2,
            got: 1
        })
    ));

    let digest = ArtifactId::from_sha256("a".repeat(64)).unwrap();
    let artifact_one = ArtifactPublicationIdentity::new(world(3), digest.clone());
    let artifact_two = ArtifactPublicationIdentity::new(world(4), digest);
    assert!(matches!(
        artifact_two.require_current(&artifact_one),
        Err(WorldIdentityError::StaleGeneration {
            kind: "world epoch",
            expected: 4,
            got: 3
        })
    ));
}

#[test]
fn unrelated_world_epoch_updates_do_not_stale_independent_objects() {
    let before = world(3);
    let after = world(4);
    assert_eq!(before.world(), after.world());

    let current_node = node(2);
    current_node.require_current(&current_node).unwrap();
    let current_domain = domain(2, 7);
    current_domain.require_current(&current_domain).unwrap();
    let current_attempt = TaskAttemptIdentity::new(
        world_id(),
        TaskId::new("build").unwrap(),
        AttemptGeneration::new(3).unwrap(),
    );
    current_attempt.require_current(&current_attempt).unwrap();
}

#[test]
fn displays_make_hgraph_versions_distinct_from_world_generations() {
    assert_eq!(world(4).to_string(), "desk@4");
    assert_eq!(node(2).to_string(), "desk/node:node-a@2");
    assert_eq!(
        domain(2, 7).to_string(),
        "desk/node:node-a@2/domain:linux-provider@7"
    );
}
