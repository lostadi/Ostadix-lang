//! Hosted PR6 repository-conformance corpus for governed ResourceKey state.
//!
//! These tests exercise descriptive planner vocabulary. They do not construct
//! authority, perform production governed lowering, or constitute native/QEMU
//! evidence.

use o_lang::effects::{EffectDeclaration, EffectSummary, GovernedResourceKind, ResourceKey};
use o_lang::world::{
    ArtifactId, ArtifactPublicationIdentity, AttemptGeneration, CapabilityId, CapabilityIdentity,
    DomainGeneration, DomainId, DomainIdentity, GovernorIdentity, GovernorLogIndex, GovernorTerm,
    NodeGeneration, NodeId, NodeIdentity, ObjectId, ObjectIdentity, ObjectVersion,
    ProcessGeneration, ProcessId, ProcessIdentity, ResourceGeneration, ResourceId,
    ResourceIdentity, ResourceOwner, TaskAttemptIdentity, TaskId, WorldEpoch, WorldId,
    WorldIdentity, WorldIdentityError,
};

#[derive(Clone)]
struct Corpus {
    world: WorldIdentity,
    governor: GovernorIdentity,
    node: NodeIdentity,
    domain: DomainIdentity,
    process: ProcessIdentity,
    resource: ResourceIdentity,
    other_resource: ResourceIdentity,
    object: ObjectIdentity,
    capability: CapabilityIdentity,
    task: TaskAttemptIdentity,
    artifact: ArtifactPublicationIdentity,
}

fn corpus() -> Corpus {
    let world_id = WorldId::new("desk").unwrap();
    let world = WorldIdentity::new(world_id.clone(), WorldEpoch::new(4).unwrap());
    let governor = GovernorIdentity::new(
        world.clone(),
        GovernorTerm::new(2).unwrap(),
        GovernorLogIndex::new(9).unwrap(),
    );
    let node = NodeIdentity::new(
        world_id.clone(),
        NodeId::new("node-a").unwrap(),
        NodeGeneration::new(2).unwrap(),
    );
    let domain = DomainIdentity::new(
        node.clone(),
        DomainId::new("linux-provider").unwrap(),
        DomainGeneration::new(7).unwrap(),
    );
    let process = ProcessIdentity::new(
        domain.clone(),
        ProcessId::new("runner").unwrap(),
        ProcessGeneration::new(3).unwrap(),
    );
    let resource = ResourceIdentity::new(
        ResourceOwner::Node { node: node.clone() },
        ResourceId::new("device/gpu-0").unwrap(),
        ResourceGeneration::new(5).unwrap(),
    );
    let other_resource = ResourceIdentity::new(
        ResourceOwner::Node { node: node.clone() },
        ResourceId::new("device/gpu-1").unwrap(),
        ResourceGeneration::new(5).unwrap(),
    );
    let object = ObjectIdentity::new(
        world_id.clone(),
        ObjectId::new("result").unwrap(),
        ObjectVersion::new(6).unwrap(),
    );
    let capability =
        CapabilityIdentity::new(world_id.clone(), CapabilityId::new("gpu-use").unwrap());
    let task = TaskAttemptIdentity::new(
        world_id,
        TaskId::new("build").unwrap(),
        AttemptGeneration::new(8).unwrap(),
    );
    let artifact = ArtifactPublicationIdentity::new(
        world.clone(),
        ArtifactId::from_sha256("a".repeat(64)).unwrap(),
    );

    Corpus {
        world,
        governor,
        node,
        domain,
        process,
        resource,
        other_resource,
        object,
        capability,
        task,
        artifact,
    }
}

fn governed_keys(corpus: &Corpus) -> Vec<ResourceKey> {
    vec![
        ResourceKey::WorldState(corpus.world.clone()),
        ResourceKey::GovernorState(corpus.governor.clone()),
        ResourceKey::NodeState(corpus.node.clone()),
        ResourceKey::DomainState(corpus.domain.clone()),
        ResourceKey::ProcessState(corpus.process.clone()),
        ResourceKey::GovernedResource(corpus.resource.clone()),
        ResourceKey::ObjectState(corpus.object.clone()),
        ResourceKey::CapabilityState(corpus.capability.clone()),
        ResourceKey::NamespaceState(corpus.world.clone()),
        ResourceKey::TaskState(corpus.task.clone()),
        ResourceKey::ArtifactState(corpus.artifact.clone()),
        ResourceKey::DeviceState(corpus.resource.clone()),
        ResourceKey::AcceleratorState(corpus.resource.clone()),
    ]
}

#[test]
fn roadmap_vocabulary_has_one_typed_classification_and_stable_display() {
    let corpus = corpus();
    let keys = governed_keys(&corpus);
    let kinds = keys
        .iter()
        .map(|key| key.governed_kind().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(kinds, GovernedResourceKind::ALL);
    assert_eq!(
        kinds.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "world",
            "governor",
            "node",
            "domain",
            "process",
            "resource",
            "object",
            "capability",
            "namespace",
            "task",
            "artifact",
            "device",
            "accelerator",
        ]
    );
    for key in &keys {
        assert!(key.is_governed_resource(), "{key}");
        assert!(!key.is_host_resource(), "{key}");
    }

    let rendered = keys.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(rendered[0], "world-state:desk@4");
    assert_eq!(rendered[1], "governor-state:desk@4/governor:term-2@9");
    assert_eq!(rendered[2], "node-state:desk/node:node-a@2");
    assert_eq!(
        rendered[3],
        "domain-state:desk/node:node-a@2/domain:linux-provider@7"
    );
    assert_eq!(
        rendered[4],
        "process-state:desk/node:node-a@2/domain:linux-provider@7/process:runner@3"
    );
    assert_eq!(
        rendered[5],
        "governed-resource:desk/node:node-a@2/resource:device/gpu-0@5"
    );
    assert_eq!(rendered[6], "object-state:desk/object:result@6");
    assert_eq!(rendered[7], "capability-state:desk/capability:gpu-use");
    assert_eq!(rendered[8], "namespace-state:desk@4");
    assert_eq!(rendered[9], "task-state:desk/task:build@8");
    assert_eq!(
        rendered[11],
        "device-state:desk/node:node-a@2/resource:device/gpu-0@5"
    );
    assert_eq!(
        rendered[12],
        "accelerator-state:desk/node:node-a@2/resource:device/gpu-0@5"
    );
}

#[test]
fn device_and_accelerator_views_share_the_generic_resource_dependency() {
    let corpus = corpus();
    let generic = ResourceKey::GovernedResource(corpus.resource.clone());
    let device = ResourceKey::DeviceState(corpus.resource.clone());
    let accelerator = ResourceKey::AcceleratorState(corpus.resource.clone());

    let mut device_writer = EffectSummary::pure();
    device_writer.writes.insert(device.clone());
    assert_eq!(
        device_writer.accessed_resources(),
        [generic.clone(), device].into_iter().collect()
    );

    let mut accelerator_reader = EffectSummary::pure();
    accelerator_reader.reads.insert(accelerator.clone());
    assert_eq!(
        accelerator_reader.accessed_resources(),
        [generic.clone(), accelerator].into_iter().collect()
    );

    let mut generic_reader = EffectSummary::pure();
    generic_reader.reads.insert(generic);
    assert!(device_writer.conflicts_with(&generic_reader));
    assert!(generic_reader.conflicts_with(&device_writer));
    assert!(device_writer.conflicts_with(&accelerator_reader));

    let mut other_reader = EffectSummary::pure();
    other_reader
        .reads
        .insert(ResourceKey::DeviceState(corpus.other_resource));
    assert!(!device_writer.conflicts_with(&other_reader));
    assert!(!EffectSummary::unknown().conflicts_with(&generic_reader));
}

#[test]
fn source_effect_declarations_cannot_mint_any_governed_class() {
    for kind in GovernedResourceKind::ALL {
        let mut spellings = vec![kind.name().to_string(), format!("{}-state", kind.name())];
        if kind == GovernedResourceKind::Resource {
            spellings.push("governed-resource".to_string());
        }
        for spelling in spellings {
            let error =
                EffectDeclaration::parse(Some(&format!("reads={spelling}:forged"))).unwrap_err();
            assert!(error.contains("requires trusted lowering"), "{error}");
            assert!(
                error.contains("cannot mint governed state or authority"),
                "{error}"
            );
        }
    }
}

#[test]
fn typed_payloads_reject_caller_supplied_stale_references() {
    let corpus = corpus();
    let stale_world = WorldIdentity::new(corpus.world.world().clone(), WorldEpoch::new(3).unwrap());
    assert!(matches!(
        corpus.world.require_current(&stale_world),
        Err(WorldIdentityError::StaleGeneration {
            kind: "world epoch",
            expected: 4,
            got: 3
        })
    ));

    let stale_governor = GovernorIdentity::new(
        corpus.world.clone(),
        GovernorTerm::new(1).unwrap(),
        corpus.governor.log_index(),
    );
    assert!(matches!(
        corpus.governor.require_current(&stale_governor),
        Err(WorldIdentityError::StaleGeneration {
            kind: "governor term",
            expected: 2,
            got: 1
        })
    ));

    let stale_node = NodeIdentity::new(
        corpus.node.world().clone(),
        corpus.node.node().clone(),
        NodeGeneration::new(1).unwrap(),
    );
    assert!(matches!(
        corpus.node.require_current(&stale_node),
        Err(WorldIdentityError::StaleGeneration {
            kind: "node generation",
            expected: 2,
            got: 1
        })
    ));

    let stale_domain = DomainIdentity::new(
        corpus.node.clone(),
        corpus.domain.domain().clone(),
        DomainGeneration::new(6).unwrap(),
    );
    assert!(matches!(
        corpus.domain.require_current(&stale_domain),
        Err(WorldIdentityError::StaleGeneration {
            kind: "domain generation",
            expected: 7,
            got: 6
        })
    ));

    let stale_process = ProcessIdentity::new(
        corpus.domain.clone(),
        corpus.process.process().clone(),
        ProcessGeneration::new(2).unwrap(),
    );
    assert!(matches!(
        corpus.process.require_current(&stale_process),
        Err(WorldIdentityError::StaleGeneration {
            kind: "process generation",
            expected: 3,
            got: 2
        })
    ));

    let stale_resource = ResourceIdentity::new(
        corpus.resource.owner().clone(),
        corpus.resource.resource().clone(),
        ResourceGeneration::new(4).unwrap(),
    );
    assert!(matches!(
        corpus.resource.require_current(&stale_resource),
        Err(WorldIdentityError::StaleGeneration {
            kind: "resource generation",
            expected: 5,
            got: 4
        })
    ));

    let stale_object = ObjectIdentity::new(
        corpus.object.world().clone(),
        corpus.object.object().clone(),
        ObjectVersion::new(5).unwrap(),
    );
    assert!(matches!(
        corpus.object.require_current(&stale_object),
        Err(WorldIdentityError::StaleGeneration {
            kind: "object version",
            expected: 6,
            got: 5
        })
    ));

    let stale_task = TaskAttemptIdentity::new(
        corpus.task.world().clone(),
        corpus.task.task().clone(),
        AttemptGeneration::new(7).unwrap(),
    );
    assert!(matches!(
        corpus.task.require_current(&stale_task),
        Err(WorldIdentityError::StaleGeneration {
            kind: "task attempt",
            expected: 8,
            got: 7
        })
    ));

    let stale_artifact =
        ArtifactPublicationIdentity::new(stale_world, corpus.artifact.artifact().clone());
    assert!(matches!(
        corpus.artifact.require_current(&stale_artifact),
        Err(WorldIdentityError::StaleGeneration {
            kind: "world epoch",
            expected: 4,
            got: 3
        })
    ));

    // CapabilityIdentity is deliberately equality-only descriptive metadata;
    // it carries neither a grant nor current revocation state.
    assert_eq!(corpus.capability.world(), corpus.world.world());
}

#[test]
fn typed_payloads_reject_logical_substitution_and_governor_log_drift() {
    let corpus = corpus();

    let other_world = WorldIdentity::new(WorldId::new("other").unwrap(), corpus.world.epoch());
    assert!(matches!(
        corpus.world.require_current(&other_world),
        Err(WorldIdentityError::IdentityMismatch { kind: "world", .. })
    ));

    let other_governor_position = GovernorIdentity::new(
        corpus.world.clone(),
        corpus.governor.term(),
        GovernorLogIndex::new(8).unwrap(),
    );
    assert!(matches!(
        corpus.governor.require_current(&other_governor_position),
        Err(WorldIdentityError::StaleGeneration {
            kind: "governor log index",
            expected: 9,
            got: 8
        })
    ));

    let other_node = NodeIdentity::new(
        corpus.node.world().clone(),
        NodeId::new("node-b").unwrap(),
        corpus.node.generation(),
    );
    assert!(matches!(
        corpus.node.require_current(&other_node),
        Err(WorldIdentityError::IdentityMismatch { kind: "node", .. })
    ));

    let other_domain = DomainIdentity::new(
        corpus.node.clone(),
        DomainId::new("other-provider").unwrap(),
        corpus.domain.generation(),
    );
    assert!(matches!(
        corpus.domain.require_current(&other_domain),
        Err(WorldIdentityError::IdentityMismatch { kind: "domain", .. })
    ));

    let other_process = ProcessIdentity::new(
        corpus.domain.clone(),
        ProcessId::new("other-runner").unwrap(),
        corpus.process.generation(),
    );
    assert!(matches!(
        corpus.process.require_current(&other_process),
        Err(WorldIdentityError::IdentityMismatch {
            kind: "process",
            ..
        })
    ));

    let other_resource = ResourceIdentity::new(
        corpus.resource.owner().clone(),
        ResourceId::new("device/gpu-other").unwrap(),
        corpus.resource.generation(),
    );
    assert!(matches!(
        corpus.resource.require_current(&other_resource),
        Err(WorldIdentityError::IdentityMismatch {
            kind: "resource",
            ..
        })
    ));

    let other_object = ObjectIdentity::new(
        corpus.object.world().clone(),
        ObjectId::new("other-result").unwrap(),
        corpus.object.version(),
    );
    assert!(matches!(
        corpus.object.require_current(&other_object),
        Err(WorldIdentityError::IdentityMismatch { kind: "object", .. })
    ));

    let other_task = TaskAttemptIdentity::new(
        corpus.task.world().clone(),
        TaskId::new("other-build").unwrap(),
        corpus.task.attempt(),
    );
    assert!(matches!(
        corpus.task.require_current(&other_task),
        Err(WorldIdentityError::IdentityMismatch { kind: "task", .. })
    ));

    let other_artifact = ArtifactPublicationIdentity::new(
        corpus.world.clone(),
        ArtifactId::from_sha256("b".repeat(64)).unwrap(),
    );
    assert!(matches!(
        corpus.artifact.require_current(&other_artifact),
        Err(WorldIdentityError::IdentityMismatch {
            kind: "artifact",
            ..
        })
    ));

    let other_capability = CapabilityIdentity::new(
        corpus.world.world().clone(),
        CapabilityId::new("other-gpu-use").unwrap(),
    );
    assert_ne!(corpus.capability, other_capability);
}
