use o_lang::world::{
    ArtifactId, ArtifactPublicationIdentity, AttemptGeneration, AttemptIdentity, CapabilityId,
    CapabilityIdentity, CheckpointId, CheckpointIdentity, DomainGeneration, DomainId,
    DomainIdentity, GovernorIdentity, GovernorLogIndex, GovernorTerm, LeaseId, LeaseIdentity,
    NodeGeneration, NodeId, NodeIdentity, ObjectId, ObjectIdentity, ObjectVersion,
    ProcessGeneration, ProcessId, ProcessIdentity, ReceiptId, ReceiptIdentity, ResourceGeneration,
    ResourceId, ResourceIdentity, ResourceOwner, TaskAttemptIdentity, TaskId, TaskIdentity,
    WorldEpoch, WorldId, WorldIdentity, WorldIdentityError,
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
fn world_artifact_id_remains_an_exact_shared_identity_reexport() {
    let shared = o_lang::resource_identity::ArtifactId::from_sha256("a".repeat(64)).unwrap();
    let through_world: o_lang::world::ArtifactId = shared.clone();
    let through_world_identity_module: o_lang::world::identity::ArtifactId = shared.clone();
    let shared_again: o_lang::resource_identity::ArtifactId = through_world;

    assert_eq!(shared_again, shared);
    assert_eq!(through_world_identity_module, shared);
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
    assert!(GovernorTerm::new(0).is_err());
    assert!(GovernorLogIndex::new(0).is_err());
    assert!(ProcessGeneration::new(0).is_err());
    assert!(ResourceGeneration::new(0).is_err());
    assert!(ObjectVersion::new(0).is_err());
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
        ResourceGeneration::new(1).unwrap(),
    );
    let resource_two = ResourceIdentity::new(
        ResourceOwner::Node { node: node(2) },
        ResourceId::new("cpu/slot-0").unwrap(),
        ResourceGeneration::new(1).unwrap(),
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

#[test]
fn every_counter_is_nonzero_and_checked_on_successor() {
    macro_rules! check_counter {
        ($counter:ty) => {{
            assert!(<$counter>::new(0).is_err());
            assert_eq!(<$counter>::new(1).unwrap().next().unwrap().get(), 2);
            assert!(<$counter>::new(u64::MAX).unwrap().next().is_err());
        }};
    }

    check_counter!(WorldEpoch);
    check_counter!(GovernorTerm);
    check_counter!(GovernorLogIndex);
    check_counter!(NodeGeneration);
    check_counter!(DomainGeneration);
    check_counter!(ProcessGeneration);
    check_counter!(ResourceGeneration);
    check_counter!(ObjectVersion);
    check_counter!(AttemptGeneration);
}

#[test]
fn roadmap_composites_are_typed_serializable_and_generation_bound() {
    let governor = GovernorIdentity::new(
        world(9),
        GovernorTerm::new(2).unwrap(),
        GovernorLogIndex::new(17).unwrap(),
    );
    let process = ProcessIdentity::new(
        domain(2, 7),
        ProcessId::new("proc-a").unwrap(),
        ProcessGeneration::new(3).unwrap(),
    );
    let object = ObjectIdentity::new(
        world_id(),
        ObjectId::new("object-a").unwrap(),
        ObjectVersion::new(5).unwrap(),
    );
    let capability = CapabilityIdentity::new(world_id(), CapabilityId::new("cap-a").unwrap());
    let lease = LeaseIdentity::new(world_id(), LeaseId::new("lease-a").unwrap());
    let task = TaskIdentity::new(world_id(), TaskId::new("task-a").unwrap());
    let attempt = AttemptIdentity::new(
        world_id(),
        TaskId::new("task-a").unwrap(),
        AttemptGeneration::new(4).unwrap(),
    );
    let checkpoint =
        CheckpointIdentity::new(attempt.clone(), CheckpointId::new("checkpoint-a").unwrap());
    let receipt = ReceiptIdentity::new(world_id(), ReceiptId::new("receipt-a").unwrap());

    let old_name: TaskAttemptIdentity = attempt.clone();
    assert_eq!(old_name, attempt);
    assert_eq!(checkpoint.attempt(), &old_name);
    assert_eq!(governor.term().get(), 2);
    assert_eq!(process.generation().get(), 3);
    assert_eq!(object.version().get(), 5);
    assert_eq!(capability.capability().as_str(), "cap-a");
    assert_eq!(lease.lease().as_str(), "lease-a");
    assert_eq!(task.task().as_str(), "task-a");
    assert_eq!(receipt.receipt().as_str(), "receipt-a");

    let encoded = serde_json::to_string(&process).unwrap();
    assert_eq!(
        serde_json::from_str::<ProcessIdentity>(&encoded).unwrap(),
        process
    );
    let encoded = serde_json::to_string(&checkpoint).unwrap();
    assert_eq!(
        serde_json::from_str::<CheckpointIdentity>(&encoded).unwrap(),
        checkpoint
    );
}

#[test]
fn composite_current_checks_separate_stale_generations_from_logical_mismatches() {
    let governor = GovernorIdentity::new(
        world(9),
        GovernorTerm::new(2).unwrap(),
        GovernorLogIndex::new(17).unwrap(),
    );
    let stale_governor = GovernorIdentity::new(
        world(9),
        GovernorTerm::new(1).unwrap(),
        GovernorLogIndex::new(17).unwrap(),
    );
    assert!(matches!(
        governor.require_current(&stale_governor),
        Err(WorldIdentityError::StaleGeneration {
            kind: "governor term",
            expected: 2,
            got: 1
        })
    ));

    let process = ProcessIdentity::new(
        domain(2, 7),
        ProcessId::new("proc-a").unwrap(),
        ProcessGeneration::new(3).unwrap(),
    );
    let stale_process = ProcessIdentity::new(
        domain(2, 7),
        ProcessId::new("proc-a").unwrap(),
        ProcessGeneration::new(2).unwrap(),
    );
    assert!(matches!(
        process.require_current(&stale_process),
        Err(WorldIdentityError::StaleGeneration {
            kind: "process generation",
            expected: 3,
            got: 2
        })
    ));
    let different_process = ProcessIdentity::new(
        domain(2, 7),
        ProcessId::new("proc-b").unwrap(),
        ProcessGeneration::new(3).unwrap(),
    );
    assert!(matches!(
        process.require_current(&different_process),
        Err(WorldIdentityError::IdentityMismatch {
            kind: "process",
            ..
        })
    ));

    let object = ObjectIdentity::new(
        world_id(),
        ObjectId::new("object-a").unwrap(),
        ObjectVersion::new(5).unwrap(),
    );
    let stale_object = ObjectIdentity::new(
        world_id(),
        ObjectId::new("object-a").unwrap(),
        ObjectVersion::new(4).unwrap(),
    );
    assert!(matches!(
        object.require_current(&stale_object),
        Err(WorldIdentityError::StaleGeneration {
            kind: "object version",
            expected: 5,
            got: 4
        })
    ));

    let checkpoint = CheckpointIdentity::new(
        AttemptIdentity::new(
            world_id(),
            TaskId::new("task-a").unwrap(),
            AttemptGeneration::new(4).unwrap(),
        ),
        CheckpointId::new("checkpoint-a").unwrap(),
    );
    let stale_checkpoint = CheckpointIdentity::new(
        AttemptIdentity::new(
            world_id(),
            TaskId::new("task-a").unwrap(),
            AttemptGeneration::new(3).unwrap(),
        ),
        CheckpointId::new("checkpoint-a").unwrap(),
    );
    assert!(matches!(
        checkpoint.require_current(&stale_checkpoint),
        Err(WorldIdentityError::StaleGeneration {
            kind: "task attempt",
            expected: 4,
            got: 3
        })
    ));
    let different_checkpoint = CheckpointIdentity::new(
        checkpoint.attempt().clone(),
        CheckpointId::new("checkpoint-b").unwrap(),
    );
    assert!(matches!(
        checkpoint.require_current(&different_checkpoint),
        Err(WorldIdentityError::IdentityMismatch {
            kind: "checkpoint",
            ..
        })
    ));
}

#[test]
fn resources_require_explicit_generation_and_support_process_owners() {
    let process = ProcessIdentity::new(
        domain(2, 7),
        ProcessId::new("proc-a").unwrap(),
        ProcessGeneration::new(3).unwrap(),
    );
    let first = ResourceIdentity::new(
        ResourceOwner::Process {
            process: process.clone(),
        },
        ResourceId::new("fd/stdout").unwrap(),
        ResourceGeneration::new(1).unwrap(),
    );
    let second = ResourceIdentity::new(
        ResourceOwner::Process { process },
        ResourceId::new("fd/stdout").unwrap(),
        ResourceGeneration::new(2).unwrap(),
    );
    assert!(matches!(
        second.require_current(&first),
        Err(WorldIdentityError::StaleGeneration {
            kind: "resource generation",
            expected: 2,
            got: 1
        })
    ));
    assert_eq!(second.generation().get(), 2);

    let encoded = serde_json::to_string(&second).unwrap();
    assert_eq!(
        serde_json::from_str::<ResourceIdentity>(&encoded).unwrap(),
        second
    );
    let old_unversioned = r#"{
        "owner":{"scope":"node","node":{"world":"desk","node":"node-a","generation":1}},
        "resource":"cpu/slot-0"
    }"#;
    assert!(serde_json::from_str::<ResourceIdentity>(old_unversioned).is_err());
}

#[test]
fn identifier_limits_accept_the_boundary_and_reject_one_byte_more() {
    let simple_at_limit = "a".repeat(128);
    assert!(WorldId::new(simple_at_limit.clone()).is_ok());
    assert!(ProcessId::new(simple_at_limit.clone()).is_ok());
    assert!(ObjectId::new(simple_at_limit.clone()).is_ok());
    assert!(CapabilityId::new(simple_at_limit.clone()).is_ok());
    assert!(LeaseId::new(simple_at_limit.clone()).is_ok());
    assert!(CheckpointId::new(simple_at_limit.clone()).is_ok());
    assert!(ReceiptId::new(simple_at_limit).is_ok());
    assert!(WorldId::new("a".repeat(129)).is_err());

    let resource_at_limit = format!("{}/{}", "a".repeat(127), "b".repeat(128));
    assert_eq!(resource_at_limit.len(), 256);
    assert!(ResourceId::new(resource_at_limit).is_ok());
    let resource_over_limit = format!("{}/{}", "a".repeat(128), "b".repeat(128));
    assert_eq!(resource_over_limit.len(), 257);
    assert!(ResourceId::new(resource_over_limit).is_err());
}
