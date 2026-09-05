use std::collections::{BTreeMap, BTreeSet};

use o_lang::effects::EffectSummary;
use o_lang::ir::{BackendRegistry, OIr, OIrProgram, PlanNodeId};
use o_lang::value::{
    BackendAuthority, NativeBoundary, NativeCodecSafety, NativeIdentity, ONative, OValue,
    RehydratePolicy,
};
use o_lang::world::{
    GroundingError, GroundingReport, WorldEpoch, WorldId, WorldIdentity, WorldIdentityError,
};

fn world(epoch: u64) -> WorldIdentity {
    WorldIdentity::new(
        WorldId::new("desk").unwrap(),
        WorldEpoch::new(epoch).unwrap(),
    )
}

#[test]
fn report_exposes_ovalue_authority_and_residual_host_dependencies() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "runner".into(),
                expr: Box::new(OIr::Text("descriptive-placeholder".into())),
            },
            OIr::Exec {
                lang: "bash".into(),
                env_id: u32::MAX,
                attr: Some("cap=runner,network".into()),
                backend: BackendRegistry::global().interface_for("bash"),
                body: vec![OIr::Text("printf ok".into())],
            },
        ],
    };
    let plan = program.plan();
    let graph = program.hgraph_for_plan(&plan).unwrap();
    let report = GroundingReport::analyze(&plan, &graph, None).unwrap();

    assert!(!report.ovalue_flows.is_empty());
    let capability = report.capabilities.first().unwrap();
    assert_eq!(capability.backend, "bash");
    assert_eq!(capability.preferred_binding.as_deref(), Some("runner"));
    assert!(capability.ambient_fallback);
    assert_eq!(
        capability.requested_rights,
        BackendAuthority::ALL.into_iter().collect::<BTreeSet<_>>()
    );
    assert!(report
        .operations
        .iter()
        .any(|operation| operation.has_residual_host_world()));

    let text = report.to_text();
    assert!(text.contains("world none"), "{text}");
    assert!(text.contains("preferred=runner"), "{text}");
    assert!(
        text.contains("requested-rights=[fs_read,fs_write,network,process]"),
        "{text}"
    );
    assert!(text.contains("hostworld=residual"), "{text}");
    assert!(
        text.contains("granted rights remain private to the live broker"),
        "{text}"
    );
}

#[test]
fn exact_world_binding_is_generation_fenced_without_claiming_governed_lowering() {
    let program = OIrProgram {
        nodes: vec![OIr::Text("logical work".into())],
    };
    let plan = program.plan();
    let graph = program.hgraph_for_plan(&plan).unwrap();

    let report_one = GroundingReport::analyze(&plan, &graph, Some(world(1))).unwrap();
    assert_eq!(report_one.bound_world(), Some(&world(1)));
    assert!(report_one.operations.is_empty());
    report_one.require_current_world(&world(1)).unwrap();
    assert!(matches!(
        report_one.require_current_world(&world(2)),
        Err(GroundingError::Identity(
            WorldIdentityError::StaleGeneration {
                kind: "world epoch",
                expected: 2,
                got: 1
            }
        ))
    ));

    let report_two = GroundingReport::analyze(&plan, &graph, Some(world(2))).unwrap();
    report_two.require_current_world(&world(2)).unwrap();
    assert_ne!(report_one.to_text(), report_two.to_text());
    assert!(report_one.to_text().contains("governed-effects none"));

    let unbound = GroundingReport::analyze(&plan, &graph, None).unwrap();
    assert!(matches!(
        unbound.require_current_world(&world(1)),
        Err(GroundingError::UnboundWorld)
    ));
}

#[test]
fn invalid_and_mismatched_graphs_are_rejected_before_reporting() {
    let literal = OIrProgram {
        nodes: vec![OIr::Text("literal".into())],
    };
    let literal_plan = literal.plan();
    let mut invalid_graph = literal.hgraph_for_plan(&literal_plan).unwrap();
    invalid_graph.set_effect_summary(PlanNodeId(0), EffectSummary::pure());

    assert!(matches!(
        GroundingReport::analyze(&literal_plan, &invalid_graph, None),
        Err(GroundingError::InvalidExecutionGraph(reason))
            if reason.contains("non-executable plan node")
    ));

    let valid_graph = literal.hgraph_for_plan(&literal_plan).unwrap();
    let other = OIrProgram {
        nodes: vec![OIr::Store {
            name: "value".into(),
            expr: Box::new(OIr::Text("other".into())),
        }],
    };
    let other_plan = other.plan();
    assert!(matches!(
        GroundingReport::analyze(&other_plan, &valid_graph, None),
        Err(GroundingError::InvalidExecutionGraph(reason))
            if reason.contains("does not match the HGraph source plan")
    ));
}

#[test]
fn materialized_native_capsule_remains_unresolved_without_origin_generation() {
    let program = OIrProgram {
        nodes: vec![OIr::Text("materialized slot".into())],
    };
    let plan = program.plan();
    let mut graph = program.hgraph_for_plan(&plan).unwrap();
    let value = graph
        .node_ids()
        .into_iter()
        .find(|id| {
            graph
                .node(*id)
                .is_some_and(|node| node.plan_node == Some(PlanNodeId(0)))
        })
        .unwrap();
    graph.node_mut(value).unwrap().value = Some(OValue::Native {
        v: ONative {
            lang: "python".into(),
            implementation: Some("cpython".into()),
            version: Some("3".into()),
            type_name: "opaque".into(),
            identity: NativeIdentity {
                stable: None,
                live: Some("process-local".into()),
            },
            codec: "none".into(),
            payload: None,
            boundary: NativeBoundary::Referential,
            safety: NativeCodecSafety::LiveHandle,
            capabilities: Vec::new(),
            metadata: BTreeMap::new(),
            rehydrate: RehydratePolicy::SameProcess,
        },
    });

    let report = GroundingReport::analyze(&plan, &graph, None).unwrap();
    assert_eq!(report.capsules.len(), 1);
    assert_eq!(report.capsules[0].rehydrate, RehydratePolicy::SameProcess);
    assert!(!report.capsules[0].origin_generation_grounded);
    assert!(report
        .to_text()
        .contains("rehydrate=same-process origin-generation=unresolved"));
}

#[test]
fn grounding_reuses_runtime_block_attribute_validation() {
    let program = OIrProgram {
        nodes: vec![OIr::Exec {
            lang: "bash".into(),
            env_id: u32::MAX,
            attr: Some("cap=runner,cap=other".into()),
            backend: BackendRegistry::global().interface_for("bash"),
            body: vec![OIr::Text("printf ok".into())],
        }],
    };
    let plan = program.plan();
    let graph = program.hgraph_for_plan(&plan).unwrap();
    assert!(matches!(
        GroundingReport::analyze(&plan, &graph, None),
        Err(GroundingError::InvalidBlockAttributes {
            plan_node: 0,
            reason
        }) if reason.contains("exactly one backend capability binding")
    ));
}

#[test]
fn capability_binding_without_requested_rights_is_not_reported_as_authority() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Store {
                name: "runner".into(),
                expr: Box::new(OIr::Text("descriptive-placeholder".into())),
            },
            OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: Some("cap=runner".into()),
                backend: BackendRegistry::global().interface_for("html"),
                body: vec![OIr::Text("ok".into())],
            },
        ],
    };
    let plan = program.plan();
    let graph = program.hgraph_for_plan(&plan).unwrap();
    let report = GroundingReport::analyze(&plan, &graph, None).unwrap();
    assert!(report.capabilities.is_empty());
    assert!(report.to_text().contains("capability-flow none"));
}

#[test]
fn concurrent_project_ambient_marker_remains_residual_host_access() {
    use o_lang::effects::ResourceKey;
    use o_lang::world::OperationGrounding;
    let mut grounding = OperationGrounding {
        plan_node: PlanNodeId(0),
        governed_reads: BTreeSet::new(),
        governed_writes: BTreeSet::new(),
        ambient_reads: BTreeSet::from([ResourceKey::ConcurrentProjectBranch(0)]),
        ambient_writes: BTreeSet::new(),
        actor_affinity: None,
    };
    assert!(grounding.has_residual_host_world());
    std::mem::swap(&mut grounding.ambient_reads, &mut grounding.ambient_writes);
    assert!(grounding.has_residual_host_world());
}
