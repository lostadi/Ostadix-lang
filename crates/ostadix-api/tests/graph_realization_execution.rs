use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use ostadix_api::computation::*;
use ostadix_api::computation_core::*;
use ostadix_api::placement::*;

fn token(value: &str) -> ComputationTokenV1 {
    ComputationTokenV1::new(value).unwrap()
}
fn reference(value: &str) -> SemanticArtifactRefV1 {
    SemanticArtifactRefV1::new(
        token(&format!("test/{value}/v1")),
        artifact_id_for_bytes(value.as_bytes()),
    )
    .unwrap()
}
fn target(name: &str) -> TargetDescriptorV1 {
    TargetDescriptorV1::new(
        name,
        name,
        GenerationV1::new(1).unwrap(),
        TargetCapabilityModelV1::DownwardClosedIdeal,
        PlatformDescriptorV1::new("macos", "aarch64", "darwin", EndiannessV1::Little, 64).unwrap(),
        Vec::<CapabilityAtomV1>::new(),
        Vec::<String>::new(),
        vec![],
    )
    .unwrap()
}

fn closure(index: usize, objective: &ObjectiveV1) -> OperationPlanningRequestV1 {
    let value_type = reference("integer");
    let fidelity = reference("exact");
    let contract = OperationContractV1::new(
        OperationIdV1::new("integer/increment").unwrap(),
        1,
        reference("pre"),
        reference("post"),
        reference("state"),
        reference("effects"),
        reference("order"),
        reference("determinism"),
        fidelity.clone(),
    )
    .unwrap();
    let interface = OperationInterfaceV1::new(
        contract.operation.clone(),
        1,
        contract.id().unwrap(),
        vec![],
        vec![OperationPortV1::new(token("in"), value_type.clone()).unwrap()],
        vec![OperationPortV1::new(token("out"), value_type.clone()).unwrap()],
    )
    .unwrap();
    let representation = PhysicalRepresentationV1::new(
        token(if index.is_multiple_of(2) {
            "integer"
        } else {
            "decimal"
        }),
        value_type,
        reference(if index.is_multiple_of(2) {
            "integer-format"
        } else {
            "decimal-format"
        }),
        PhysicalStorageV1::HostMemory,
        PhysicalOwnershipV1::Owned,
        false,
    )
    .unwrap();
    let footprint =
        RequirementFootprintV1::complete([RequirementAtomV1::architecture("aarch64").unwrap()]);
    let footprint_ref = SemanticArtifactRefV1::new(
        token(REQUIREMENT_FOOTPRINT_CONTENT_SCHEMA_V1),
        artifact_id_for_bytes(&footprint.canonical_bytes().unwrap()),
    )
    .unwrap();
    let descriptor = RealizationDescriptorV1::new(
        RealizationIdV1::new(format!("increment/{index}")).unwrap(),
        interface.id().unwrap(),
        contract.id().unwrap(),
        artifact_id_for_bytes(format!("implementation-{index}").as_bytes()),
        reference("pipeline"),
        vec![RealizationPortRepresentationsV1::new(
            token("in"),
            vec![representation.semantic_ref().unwrap()],
        )
        .unwrap()],
        vec![RealizationPortRepresentationsV1::new(
            token("out"),
            vec![representation.semantic_ref().unwrap()],
        )
        .unwrap()],
        footprint_ref,
        reference("state-requirements"),
        reference("actor-requirements"),
        fidelity,
        None,
        vec![reference("validation")],
    )
    .unwrap();
    let realization_set = RealizationSetV1::new(
        interface.id().unwrap(),
        contract.id().unwrap(),
        vec![descriptor.id().unwrap()],
    )
    .unwrap();
    let geometry = reference("one-integer");
    let graph = LogicalHGraphV2::new(
        vec![LogicalOperationNodeV2 {
            id: LogicalOperationNodeIdV2(0),
            interface: interface.id().unwrap(),
            contract: contract.id().unwrap(),
            realization_set: realization_set.id().unwrap(),
            input_geometry: geometry.clone(),
        }],
        vec![],
        vec![LogicalOperationNodeIdV2(0)],
    )
    .unwrap();
    let target = target(&format!("local:adapter-{index}"));
    let selections = |name: &str| {
        vec![PortRepresentationSelectionV1 {
            port: token(name),
            representation: representation.id().unwrap(),
            residency: ValueResidencyV1::Portable,
        }]
    };
    let profile = CostProfileV1::new(
        descriptor.id().unwrap(),
        descriptor.realization.clone(),
        interface.id().unwrap(),
        contract.id().unwrap(),
        target.semantic_digest().unwrap(),
        geometry,
        selections("in"),
        selections("out"),
        CostComponentsV1 {
            compute_ns: 10,
            ..Default::default()
        },
        1,
        1,
        vec![reference("cost-evidence")],
    )
    .unwrap();
    let offers = |name: &str| {
        vec![PortRepresentationOfferV1 {
            port: token(name),
            representation: representation.clone(),
            residency: ValueResidencyV1::Portable,
        }]
    };
    let offer = CandidateTupleOfferV1::new(
        LogicalOperationNodeIdV2(0),
        descriptor.id().unwrap(),
        target,
        footprint,
        offers("in"),
        offers("out"),
        profile,
    )
    .unwrap();
    OperationPlanningRequestV1::new(
        graph,
        contract,
        interface,
        vec![descriptor],
        realization_set,
        objective.clone(),
        vec![offer],
        vec![],
    )
    .unwrap()
}

fn request(count: usize, chain: bool) -> GraphPlanningRequestV1 {
    let objective =
        ObjectiveV1::new_minimize_predicted_total_ns(reference("objective"), None).unwrap();
    let operations = (0..count)
        .map(|index| closure(index, &objective))
        .collect::<Vec<_>>();
    let nodes = operations
        .iter()
        .enumerate()
        .map(|(index, closure)| {
            let mut node = closure.graph.operations[0].clone();
            node.id = LogicalOperationNodeIdV2(index as u64);
            node
        })
        .collect();
    let edges = if chain {
        (1..count)
            .map(|index| LogicalEdgeV2 {
                id: LogicalEdgeIdV2(index as u64 - 1),
                producer: LogicalEdgeEndpointV2 {
                    operation: LogicalOperationNodeIdV2(index as u64 - 1),
                    port: token("out"),
                },
                consumer: LogicalEdgeEndpointV2 {
                    operation: LogicalOperationNodeIdV2(index as u64),
                    port: token("in"),
                },
                value_type: reference("integer"),
            })
            .collect()
    } else {
        vec![]
    };
    let roots = if chain {
        vec![LogicalOperationNodeIdV2(count as u64 - 1)]
    } else {
        (0..count)
            .map(|index| LogicalOperationNodeIdV2(index as u64))
            .collect()
    };
    let graph = LogicalHGraphV2::new(nodes, edges, roots).unwrap();
    let transfers = graph
        .edges
        .iter()
        .map(|edge| {
            let source = operations[edge.producer.operation.0 as usize].offers[0]
                .candidate()
                .unwrap();
            let destination = operations[edge.consumer.operation.0 as usize].offers[0]
                .candidate()
                .unwrap();
            TransferPlanV1::new(
                graph.id().unwrap(),
                edge.id,
                source.target,
                destination.target,
                source.outputs[0].representation.clone(),
                destination.inputs[0].representation.clone(),
                artifact_id_for_bytes(b"integer-decimal-adapter"),
                token("integer-decimal"),
                8,
                3,
            )
            .unwrap()
        })
        .collect();
    GraphPlanningRequestV1 {
        graph,
        objective,
        operations,
        transfers,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Payload {
    Integer(i64),
    Decimal(String),
}
impl Payload {
    fn number(&self) -> i64 {
        match self {
            Self::Integer(value) => *value,
            Self::Decimal(value) => value.parse().unwrap(),
        }
    }
}

#[derive(Default)]
struct RefillProbe {
    started: Mutex<BTreeSet<u64>>,
    changed: Condvar,
    active: AtomicUsize,
    peak: AtomicUsize,
}
struct Increment {
    calls: Arc<AtomicUsize>,
    probe: Option<Arc<RefillProbe>>,
}
impl PhysicalOperationAdapterV1<Payload> for Increment {
    fn admit(
        &self,
        candidate: &RealizationCandidateTupleV1,
        descriptor: &RealizationDescriptorV1,
        _: &OperationContractV1,
        _: &OperationInterfaceV1,
    ) -> Result<()> {
        assert_eq!(candidate.descriptor, descriptor.id()?);
        Ok(())
    }
    fn execute(
        &self,
        candidate: &RealizationCandidateTupleV1,
        inputs: PhysicalPortValuesV1<Payload>,
    ) -> Result<PhysicalPortValuesV1<Payload>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(probe) = &self.probe {
            let active = probe.active.fetch_add(1, Ordering::SeqCst) + 1;
            probe.peak.fetch_max(active, Ordering::SeqCst);
            let mut started = probe.started.lock().unwrap();
            started.insert(candidate.logical_operation.0);
            probe.changed.notify_all();
            if candidate.logical_operation.0 == 0 {
                let (started, wait) = probe
                    .changed
                    .wait_timeout_while(started, Duration::from_secs(3), |ids| !ids.contains(&2))
                    .unwrap();
                if wait.timed_out() && !started.contains(&2) {
                    bail!("executor failed to refill an available worker while operation zero was active");
                }
            }
            probe.active.fetch_sub(1, Ordering::SeqCst);
        }
        let number = inputs[&token("in")].payload.number() + 1;
        let payload = if candidate.logical_operation.0.is_multiple_of(2) {
            Payload::Integer(number)
        } else {
            Payload::Decimal(number.to_string())
        };
        Ok(BTreeMap::from([(
            token("out"),
            PhysicalValueV1 {
                value_type: reference("integer"),
                representation: candidate.outputs[0].representation.clone(),
                residency: ValueResidencyV1::Portable,
                payload,
            },
        )]))
    }
}

struct Convert {
    corrupt: bool,
    reject: bool,
    checks: Arc<AtomicUsize>,
    wrong_representation: bool,
}
impl PhysicalTransferAdapterV1<Payload> for Convert {
    fn admit(&self, _: &TransferPlanV1, _: &LogicalEdgeV2) -> Result<()> {
        if self.reject {
            bail!("transfer authority refused");
        }
        Ok(())
    }
    fn observe_source(
        &self,
        _: &TransferPlanV1,
        source: &PhysicalValueV1<Payload>,
    ) -> Result<SemanticArtifactRefV1> {
        Ok(SemanticArtifactRefV1::new(
            token("test/integer-observation/v1"),
            artifact_id_for_bytes(&source.payload.number().to_be_bytes()),
        )?)
    }
    fn transfer(
        &self,
        transfer: &TransferPlanV1,
        source: &PhysicalValueV1<Payload>,
    ) -> Result<PhysicalValueV1<Payload>> {
        let number = source.payload.number() + i64::from(self.corrupt);
        let payload = match source.payload {
            Payload::Integer(_) => Payload::Decimal(number.to_string()),
            Payload::Decimal(_) => Payload::Integer(number),
        };
        Ok(PhysicalValueV1 {
            value_type: source.value_type.clone(),
            representation: if self.wrong_representation {
                source.representation.clone()
            } else {
                transfer.destination_representation.clone()
            },
            residency: ValueResidencyV1::Portable,
            payload,
        })
    }
    fn check_observation(
        &self,
        _: &TransferPlanV1,
        source_observation: &SemanticArtifactRefV1,
        destination: &PhysicalValueV1<Payload>,
    ) -> Result<()> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        if source_observation.content
            != artifact_id_for_bytes(&destination.payload.number().to_be_bytes())
        {
            bail!("integer observation does not commute across transfer");
        }
        Ok(())
    }
}

fn registry(
    request: &GraphPlanningRequestV1,
    calls: &Arc<AtomicUsize>,
    transfer: Option<Convert>,
    probe: Option<Arc<RefillProbe>>,
) -> PhysicalAdapterRegistryV1<Payload> {
    let mut registry = PhysicalAdapterRegistryV1::default();
    for closure in &request.operations {
        let candidate = closure.offers[0].candidate().unwrap();
        registry
            .register_operation(
                candidate.descriptor,
                candidate.target,
                Arc::new(Increment {
                    calls: calls.clone(),
                    probe: probe.clone(),
                }),
            )
            .unwrap();
    }
    if let Some(transfer) = transfer {
        registry
            .register_transfer(
                artifact_id_for_bytes(b"integer-decimal-adapter"),
                Arc::new(transfer),
            )
            .unwrap();
    }
    registry
}

fn external(request: &GraphPlanningRequestV1) -> GraphExternalInputsV1<Payload> {
    request
        .graph
        .operations
        .iter()
        .filter(|operation| {
            !request
                .graph
                .edges
                .iter()
                .any(|edge| edge.consumer.operation == operation.id)
        })
        .map(|operation| {
            let closure = &request.operations[operation.id.0 as usize];
            (
                LogicalEdgeEndpointV2 {
                    operation: operation.id,
                    port: token("in"),
                },
                PhysicalValueV1 {
                    value_type: reference("integer"),
                    representation: closure.offers[0].inputs[0].representation.id().unwrap(),
                    residency: ValueResidencyV1::Portable,
                    payload: Payload::Integer(40),
                },
            )
        })
        .collect()
}

fn conversion(checks: &Arc<AtomicUsize>) -> Convert {
    Convert {
        corrupt: false,
        reject: false,
        checks: checks.clone(),
        wrong_representation: false,
    }
}

#[test]
fn three_operations_execute_real_representation_transfers_before_consumers() {
    let request = request(3, true);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    // V1 ranking includes each cost profile's one-nanosecond uncertainty.
    assert_eq!(plan.predicted_total_ns(), 39);
    assert_eq!(plan.tasks().len(), 5);
    assert_eq!(plan.deployment().transfers.len(), 2);
    let calls = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let registry = registry(&request, &calls, Some(conversion(&checks)), None);
    let result = execute_graph_realizations_v1(&plan, &registry, &external(&request), 4).unwrap();
    assert_eq!(
        result.outputs[&LogicalOperationNodeIdV2(2)][&token("out")].payload,
        Payload::Integer(43)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    for edge in &request.graph.edges {
        let transfer_done = result
            .observations
            .iter()
            .position(|observation| {
                observation.task == PhysicalTaskIdV1::Transfer(edge.id)
                    && observation.transition == PhysicalTaskTransitionV1::Succeeded
            })
            .unwrap();
        let consumer_start = result
            .observations
            .iter()
            .position(|observation| {
                observation.task == PhysicalTaskIdV1::Operation(edge.consumer.operation)
                    && observation.transition == PhysicalTaskTransitionV1::Started
            })
            .unwrap();
        assert!(transfer_done < consumer_start);
    }
}

#[test]
fn corrupt_transfer_observation_blocks_consumer_execution() {
    let request = request(3, true);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let mut transfer = conversion(&checks);
    transfer.corrupt = true;
    let registry = registry(&request, &calls, Some(transfer), None);
    let failure =
        execute_graph_realizations_v1(&plan, &registry, &external(&request), 4).unwrap_err();
    assert!(failure.message.contains("does not commute"), "{failure}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(checks.load(Ordering::SeqCst), 1);
    assert_eq!(
        failure.observations.last().unwrap().transition,
        PhysicalTaskTransitionV1::Failed
    );
}

#[test]
fn wrong_transfer_envelope_blocks_consumer_even_when_payload_is_equal() {
    let request = request(2, true);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let mut transfer = conversion(&checks);
    transfer.wrong_representation = true;
    let registry = registry(&request, &calls, Some(transfer), None);
    let failure =
        execute_graph_realizations_v1(&plan, &registry, &external(&request), 2).unwrap_err();
    assert!(failure.message.contains("representation"), "{failure}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn all_transfer_adapters_are_admitted_before_any_operation_executes() {
    let request = request(2, true);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    for transfer in [
        None,
        Some(Convert {
            reject: true,
            ..conversion(&checks)
        }),
    ] {
        let registry = registry(&request, &calls, transfer, None);
        let failure =
            execute_graph_realizations_v1(&plan, &registry, &external(&request), 2).unwrap_err();
        assert!(failure.observations.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn forged_transfer_coordinates_ports_types_and_missing_edges_fail_planning() {
    for case in 0..7 {
        let mut request = request(3, true);
        match case {
            0 => {
                request.transfers.pop();
            }
            1 => {
                request.transfers[0].source_target =
                    request.transfers[0].destination_target.clone();
            }
            2 => {
                request.transfers[0].destination_representation =
                    request.transfers[0].source_representation.clone();
            }
            3 => {
                request.graph.edges[0].consumer.port = token("missing");
            }
            4 => {
                request.graph.edges[0].value_type = reference("different-type");
            }
            5 => {
                request.transfers[0].edge = LogicalEdgeIdV2(99);
            }
            6 => {
                request.transfers[1] = request.transfers[0].clone();
            }
            _ => unreachable!(),
        }
        // Keep graph identities coherent for the port/type cases so rejection
        // exercises semantic endpoint validation, not merely a stale digest.
        if case == 3 || case == 4 {
            let graph_id = request.graph.id().unwrap();
            for transfer in &mut request.transfers {
                transfer.logical_hgraph = graph_id.clone();
            }
        }
        assert!(
            plan_graph_realizations_v1(&request).is_err(),
            "forgery case {case}"
        );
    }
}

#[test]
fn graph_cycles_and_external_edge_override_are_rejected() {
    let mut cyclic = request(3, true);
    cyclic.graph.edges.push(LogicalEdgeV2 {
        id: LogicalEdgeIdV2(2),
        producer: LogicalEdgeEndpointV2 {
            operation: LogicalOperationNodeIdV2(2),
            port: token("out"),
        },
        consumer: LogicalEdgeEndpointV2 {
            operation: LogicalOperationNodeIdV2(0),
            port: token("in"),
        },
        value_type: reference("integer"),
    });
    cyclic.graph.roots.clear();
    assert!(plan_graph_realizations_v1(&cyclic)
        .unwrap_err()
        .to_string()
        .contains("cycle"));
    let request = request(2, true);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let registry = registry(&request, &calls, Some(conversion(&checks)), None);
    let mut inputs = external(&request);
    inputs.insert(
        request.graph.edges[0].consumer.clone(),
        inputs.values().next().unwrap().clone(),
    );
    let failure = execute_graph_realizations_v1(&plan, &registry, &inputs, 2).unwrap_err();
    assert!(failure.message.contains("replace an admitted transfer"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn graph_objective_includes_transfer_cost_and_rejects_overflow() {
    let mut bounded = request(3, true);
    bounded.objective.maximum_total_ns = Some(38);
    for closure in &mut bounded.operations {
        closure.objective = bounded.objective.clone();
    }
    assert!(plan_graph_realizations_v1(&bounded)
        .unwrap_err()
        .to_string()
        .contains("whole graph predicted cost"));
    let mut overflow = request(2, true);
    overflow.transfers[0].estimated_cost_ns = u64::MAX;
    assert!(plan_graph_realizations_v1(&overflow)
        .unwrap_err()
        .to_string()
        .contains("overflow"));
}

#[test]
fn fixed_transfer_coordinates_filter_a_cheaper_incompatible_target() {
    let mut request = request(2, true);
    let mut cheaper = request.operations[0].offers[0].clone();
    cheaper.target = target("local:unconnected");
    cheaper.cost_profile.target = cheaper.target.semantic_digest().unwrap();
    cheaper.cost_profile.components.compute_ns = 0;
    request.operations[0].offers.push(cheaper);
    let closure = &mut request.operations[0];
    *closure = OperationPlanningRequestV1::new(
        closure.graph.clone(),
        closure.contract.clone(),
        closure.interface.clone(),
        closure.descriptors.clone(),
        closure.realization_set.clone(),
        closure.objective.clone(),
        closure.offers.clone(),
        vec![],
    )
    .unwrap();
    let plan = plan_graph_realizations_v1(&request).unwrap();
    assert_eq!(
        plan.deployment().operations[0]
            .selection
            .as_ref()
            .unwrap()
            .target,
        request.transfers[0].source_target
    );
    assert_eq!(plan.predicted_total_ns(), 25);
}

#[test]
fn independent_operations_refill_workers_without_a_wave_barrier() {
    let request = request(3, false);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let probe = Arc::new(RefillProbe::default());
    let registry = registry(&request, &calls, None, Some(probe.clone()));
    let result = execute_graph_realizations_v1(&plan, &registry, &external(&request), 2).unwrap();
    assert_eq!(result.outputs.len(), 3);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn transfer_cannot_replace_its_source_observation_by_mutating_an_aliased_handle() {
    type Handle = Arc<Mutex<i64>>;
    struct HandleOperation;
    impl PhysicalOperationAdapterV1<Handle> for HandleOperation {
        fn admit(
            &self,
            _: &RealizationCandidateTupleV1,
            _: &RealizationDescriptorV1,
            _: &OperationContractV1,
            _: &OperationInterfaceV1,
        ) -> Result<()> {
            Ok(())
        }
        fn execute(
            &self,
            candidate: &RealizationCandidateTupleV1,
            inputs: PhysicalPortValuesV1<Handle>,
        ) -> Result<PhysicalPortValuesV1<Handle>> {
            Ok(BTreeMap::from([(
                token("out"),
                PhysicalValueV1 {
                    value_type: reference("integer"),
                    representation: candidate.outputs[0].representation.clone(),
                    residency: ValueResidencyV1::Portable,
                    payload: inputs[&token("in")].payload.clone(),
                },
            )]))
        }
    }
    struct MutatesSource;
    impl PhysicalTransferAdapterV1<Handle> for MutatesSource {
        fn admit(&self, _: &TransferPlanV1, _: &LogicalEdgeV2) -> Result<()> {
            Ok(())
        }
        fn observe_source(
            &self,
            _: &TransferPlanV1,
            source: &PhysicalValueV1<Handle>,
        ) -> Result<SemanticArtifactRefV1> {
            Ok(SemanticArtifactRefV1::new(
                token("test/integer-observation/v1"),
                artifact_id_for_bytes(&source.payload.lock().unwrap().to_be_bytes()),
            )?)
        }
        fn transfer(
            &self,
            transfer: &TransferPlanV1,
            source: &PhysicalValueV1<Handle>,
        ) -> Result<PhysicalValueV1<Handle>> {
            *source.payload.lock().unwrap() += 1;
            Ok(PhysicalValueV1 {
                representation: transfer.destination_representation.clone(),
                ..source.clone()
            })
        }
        fn check_observation(
            &self,
            _: &TransferPlanV1,
            before: &SemanticArtifactRefV1,
            destination: &PhysicalValueV1<Handle>,
        ) -> Result<()> {
            if before.content
                != artifact_id_for_bytes(&destination.payload.lock().unwrap().to_be_bytes())
            {
                bail!("source observation changed through a shared native handle");
            }
            Ok(())
        }
    }
    let request = request(2, true);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    let mut registry = PhysicalAdapterRegistryV1::default();
    for closure in &request.operations {
        let candidate = closure.offers[0].candidate().unwrap();
        registry
            .register_operation(
                candidate.descriptor,
                candidate.target,
                Arc::new(HandleOperation),
            )
            .unwrap();
    }
    registry
        .register_transfer(
            artifact_id_for_bytes(b"integer-decimal-adapter"),
            Arc::new(MutatesSource),
        )
        .unwrap();
    let inputs = external(&request)
        .into_iter()
        .map(|(endpoint, value)| {
            (
                endpoint,
                PhysicalValueV1 {
                    value_type: value.value_type,
                    representation: value.representation,
                    residency: value.residency,
                    payload: Arc::new(Mutex::new(40)),
                },
            )
        })
        .collect();
    let failure = execute_graph_realizations_v1(&plan, &registry, &inputs, 2).unwrap_err();
    assert!(
        failure.message.contains("source observation changed"),
        "{failure}"
    );
    assert!(!failure
        .observations
        .iter()
        .any(|observation| observation.task
            == PhysicalTaskIdV1::Operation(LogicalOperationNodeIdV2(1))));
}

#[cfg(unix)]
#[test]
fn transfer_moves_actual_payload_through_a_process_pipe_before_consumer_execution() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    struct PipeTransfer {
        conversion: Convert,
        transferred_bytes: Arc<AtomicUsize>,
    }
    impl PhysicalTransferAdapterV1<Payload> for PipeTransfer {
        fn admit(&self, transfer: &TransferPlanV1, edge: &LogicalEdgeV2) -> Result<()> {
            self.conversion.admit(transfer, edge)
        }
        fn observe_source(
            &self,
            transfer: &TransferPlanV1,
            source: &PhysicalValueV1<Payload>,
        ) -> Result<SemanticArtifactRefV1> {
            self.conversion.observe_source(transfer, source)
        }
        fn transfer(
            &self,
            transfer: &TransferPlanV1,
            source: &PhysicalValueV1<Payload>,
        ) -> Result<PhysicalValueV1<Payload>> {
            // Use an explicit executable and real OS pipe. Consumer readiness
            // follows received bytes and the observation check, never a record
            // that merely names a transfer mechanism.
            let mut child = Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(&source.payload.number().to_be_bytes())?;
            let output = child.wait_with_output()?;
            if !output.status.success() {
                bail!("pipe transfer process failed");
            }
            let bytes: [u8; 8] = output.stdout.as_slice().try_into()?;
            self.transferred_bytes
                .fetch_add(bytes.len(), Ordering::SeqCst);
            let number = i64::from_be_bytes(bytes);
            let received = PhysicalValueV1 {
                payload: match source.payload {
                    Payload::Integer(_) => Payload::Integer(number),
                    Payload::Decimal(_) => Payload::Decimal(number.to_string()),
                },
                ..source.clone()
            };
            self.conversion.transfer(transfer, &received)
        }
        fn check_observation(
            &self,
            transfer: &TransferPlanV1,
            before: &SemanticArtifactRefV1,
            destination: &PhysicalValueV1<Payload>,
        ) -> Result<()> {
            self.conversion
                .check_observation(transfer, before, destination)
        }
    }
    let request = request(3, true);
    let plan = plan_graph_realizations_v1(&request).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let mut registry = registry(&request, &calls, None, None);
    registry
        .register_transfer(
            artifact_id_for_bytes(b"integer-decimal-adapter"),
            Arc::new(PipeTransfer {
                conversion: conversion(&checks),
                transferred_bytes: bytes.clone(),
            }),
        )
        .unwrap();
    let report = execute_graph_realizations_v1(&plan, &registry, &external(&request), 3).unwrap();
    assert_eq!(
        report.outputs[&LogicalOperationNodeIdV2(2)][&token("out")].payload,
        Payload::Integer(43)
    );
    assert_eq!(bytes.load(Ordering::SeqCst), 16);
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
