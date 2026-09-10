//! Automatic physical projection of an admitted local OIR execution.
//!
//! The ordinary coordinator remains the sole operation/admission authority.
//! Every producer-backed input becomes a planned transfer; consumers prepare
//! from delivered values only after exact canonical observation checks. The
//! transport is local: native references retain their existing owner and no
//! remote placement or authority is inferred from planning records.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{graph_realization_plan::*, realization_plan::*};
use crate::computation_core::*;
use crate::eval::Evaluator;
use crate::eval_core::{GraphEvalFrame, GraphExecutionBoundary, GraphInputRestore};
use crate::evidence::AdmittedExecution;
use crate::execution_contract::Policy;
use crate::hgraph::{HNodeKind, NodeId, ReadyInputPolicy, ReadyOp, ReadySchedule};
use crate::ir::{OIrProgram, PlanNodeId};
use crate::placement::*;
use crate::value::OValue;

/// A byte transport, not an execution authority. The bridge independently
/// verifies decoded content against the observation frozen before this call.
pub trait OirPhysicalTransportV1: Send + Sync {
    fn transfer(&self, bytes: &[u8]) -> Result<Vec<u8>>;
}

/// Move each input through a kernel socket to a separate receiving thread.
/// The frame is length-bounded by the source buffer and both ends time out.
#[derive(Clone, Debug)]
pub struct LocalSocketTransportV1 {
    pub timeout: Duration,
}

impl Default for LocalSocketTransportV1 {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl OirPhysicalTransportV1 for LocalSocketTransportV1 {
    fn transfer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        #[cfg(unix)]
        {
            let (mut sender, mut receiver) = std::os::unix::net::UnixStream::pair()?;
            sender.set_write_timeout(Some(self.timeout))?;
            receiver.set_read_timeout(Some(self.timeout))?;
            std::thread::scope(|scope| -> Result<Vec<u8>> {
                let receive = scope.spawn(|| -> Result<Vec<u8>> {
                    let mut delivered = vec![0; bytes.len()];
                    receiver.read_exact(&mut delivered)?;
                    Ok(delivered)
                });
                let written = sender.write_all(bytes);
                drop(sender);
                let delivered = receive
                    .join()
                    .map_err(|_| anyhow!("physical receiver panicked"))?;
                written?;
                delivered
            })
        }
        #[cfg(not(unix))]
        {
            let _ = bytes;
            bail!("local socket transport currently requires Unix")
        }
    }
}

#[derive(Clone, Debug)]
pub struct OirInputTransferObservationV1 {
    /// None means an initially materialized input or ambient lexical scope.
    pub edge: Option<LogicalEdgeIdV2>,
    pub consumer: PlanNodeId,
    pub port: ComputationTokenV1,
    pub bytes: usize,
    pub observation: SemanticArtifactRefV1,
}

#[derive(Clone, Debug)]
pub struct OirPhysicalExecutionReportV1 {
    pub value: OValue,
    /// None when the admitted program only materializes literals (or is empty).
    /// Such a program has no physical operations or transfer tasks.
    pub plan: Option<GraphRealizationPlanV1>,
    pub operations: Vec<PlanNodeId>,
    pub tasks: Vec<PhysicalTaskObservationV1>,
    pub transfers: Vec<OirInputTransferObservationV1>,
}

#[derive(Default)]
struct ExecutionObservations {
    tasks: Vec<PhysicalTaskObservationV1>,
    transfers: Vec<OirInputTransferObservationV1>,
}

fn token(value: impl Into<String>) -> Result<ComputationTokenV1> {
    Ok(ComputationTokenV1::new(value)?)
}

fn reference(name: &str, bytes: &[u8]) -> Result<SemanticArtifactRefV1> {
    Ok(SemanticArtifactRefV1::new(
        token(format!("oir-physical/{name}/v1"))?,
        artifact_id_for_bytes(bytes),
    )?)
}

fn input_port(node: NodeId) -> Result<ComputationTokenV1> {
    token(format!("node-{}", node.0))
}

fn representation(value: bool) -> Result<PhysicalRepresentationV1> {
    let name = if value { "ovalue" } else { "execution-token" };
    Ok(PhysicalRepresentationV1::new(
        token(name)?,
        reference(name, name.as_bytes())?,
        reference(
            "json-carrier",
            b"serde OValue or graph-bound token; canonical equality required",
        )?,
        PhysicalStorageV1::HostMemory,
        PhysicalOwnershipV1::Owned,
        false,
    )?)
}

/// Generate a descriptive plan from the exact, already-admitted operation
/// graph. A local execution has one bound realization per operation; unknown
/// costs remain explicitly unmeasured and confer no performance claim.
/// Returns None for an admitted graph with no executable operations.
pub fn plan_admitted_oir_physical_v1(
    admitted: &AdmittedExecution<'_>,
) -> Result<Option<GraphRealizationPlanV1>> {
    let schedule = ReadySchedule::derive(admitted.graph()).map_err(anyhow::Error::msg)?;
    if schedule.ops.is_empty() {
        return Ok(None);
    }
    let graph = admitted.graph();
    let binding = admitted.admission().admission_sha256().as_bytes();
    let objective = ObjectiveV1::new_minimize_predicted_total_ns(
        reference("local-bound-realization", binding)?,
        None,
    )?;
    let target = TargetDescriptorV1::new(
        "local:admitted-oir",
        "Admitted local evaluator",
        GenerationV1::new(1)?,
        TargetCapabilityModelV1::DownwardClosedIdeal,
        PlatformDescriptorV1::new(
            std::env::consts::OS,
            std::env::consts::ARCH,
            "native",
            if cfg!(target_endian = "little") {
                EndiannessV1::Little
            } else {
                EndiannessV1::Big
            },
            usize::BITS as u16,
        )?,
        Vec::<CapabilityAtomV1>::new(),
        Vec::<String>::new(),
        vec![],
    )?;
    let requirements = RequirementFootprintV1::complete([RequirementAtomV1::architecture(
        std::env::consts::ARCH,
    )?]);
    let requirement_ref = SemanticArtifactRefV1::new(
        token(REQUIREMENT_FOOTPRINT_CONTENT_SCHEMA_V1)?,
        artifact_id_for_bytes(&requirements.canonical_bytes()?),
    )?;
    let value_rep = representation(true)?;
    let token_rep = representation(false)?;
    let coordinator_implementation =
        artifact_id_for_bytes(include_bytes!("../executor/coordinator.rs"));
    let transfer_implementation =
        artifact_id_for_bytes(include_bytes!("oir_physical_execution.rs"));
    let rep = |node: NodeId| -> &PhysicalRepresentationV1 {
        if matches!(
            graph.node(node).expect("validated node").kind,
            HNodeKind::Value
        ) {
            &value_rep
        } else {
            &token_rep
        }
    };
    let mut closures = Vec::new();
    for op in &schedule.ops {
        if op.input_policy(graph).map_err(anyhow::Error::msg)? != ReadyInputPolicy::All {
            bail!("OIR physical execution requires conjunctive readiness");
        }
        let evidence = admitted
            .admission()
            .operations()
            .iter()
            .find(|item| item.plan_node == op.plan_node)
            .ok_or_else(|| anyhow!("operation lacks admission"))?;
        // The authority-bearing canonical admission already commits all node
        // evidence. Bind a node coordinate to that identity, without treating
        // the human-readable Debug rendering as canonical evidence.
        let evidence_bytes = format!(
            "{}:P{}",
            admitted.admission().admission_sha256(),
            evidence.plan_node.0
        )
        .into_bytes();
        let bound = reference("admitted-operation", &evidence_bytes)?;
        let fidelity = reference(
            "canonical-ovalue-equality",
            b"all canonical OValue structure; token identity; owner references unchanged",
        )?;
        let contract = OperationContractV1::new(
            OperationIdV1::new(format!("oir/operation/{}", op.plan_node.0))?,
            1,
            reference("admission", binding)?,
            bound.clone(),
            bound.clone(),
            bound.clone(),
            bound.clone(),
            bound.clone(),
            fidelity.clone(),
        )?;
        let mut inputs = op
            .inputs
            .iter()
            .map(|&node| {
                Ok(OperationPortV1::new(
                    input_port(node)?,
                    rep(node).value_type.clone(),
                )?)
            })
            .collect::<Result<Vec<_>>>()?;
        inputs.push(OperationPortV1::new(
            token("ambient-scope")?,
            value_rep.value_type.clone(),
        )?);
        let outputs = op
            .outputs
            .iter()
            .map(|&node| {
                Ok(OperationPortV1::new(
                    input_port(node)?,
                    rep(node).value_type.clone(),
                )?)
            })
            .collect::<Result<Vec<_>>>()?;
        let interface = OperationInterfaceV1::new(
            contract.operation.clone(),
            1,
            contract.id()?,
            vec![],
            inputs,
            outputs,
        )?;
        let offers_for = |ports: &[OperationPortV1]| -> Result<Vec<PortRepresentationOfferV1>> {
            ports
                .iter()
                .map(|port| {
                    Ok(PortRepresentationOfferV1 {
                        port: port.name.clone(),
                        representation: if port.value_type == value_rep.value_type {
                            value_rep.clone()
                        } else {
                            token_rep.clone()
                        },
                        residency: ValueResidencyV1::Portable,
                    })
                })
                .collect()
        };
        let inputs = offers_for(&interface.inputs)?;
        let outputs = offers_for(&interface.outputs)?;
        let descriptor_ports = |offers: &[PortRepresentationOfferV1]| -> Result<Vec<RealizationPortRepresentationsV1>> {
            offers.iter().map(|offer| Ok(RealizationPortRepresentationsV1::new(offer.port.clone(), vec![offer.representation.semantic_ref()?])?)).collect()
        };
        let descriptor = RealizationDescriptorV1::new(
            RealizationIdV1::new(format!("admitted-local-oir/{}", op.plan_node.0))?,
            interface.id()?,
            contract.id()?,
            coordinator_implementation.clone(),
            reference("coordinator-pipeline", binding)?,
            descriptor_ports(&inputs)?,
            descriptor_ports(&outputs)?,
            requirement_ref.clone(),
            bound.clone(),
            bound.clone(),
            fidelity,
            None,
            vec![bound],
        )?;
        let set = RealizationSetV1::new(interface.id()?, contract.id()?, vec![descriptor.id()?])?;
        let preparation_start = Instant::now();
        let geometry_bytes = serde_json::to_vec(&interface.inputs)?;
        let serialization_ns = u64::try_from(preparation_start.elapsed().as_nanos())
            .context("local preparation sample exceeds nanosecond range")?;
        let geometry = reference("input-geometry", &geometry_bytes)?;
        let local = LogicalHGraphV2::new(
            vec![LogicalOperationNodeV2 {
                id: LogicalOperationNodeIdV2(0),
                interface: interface.id()?,
                contract: contract.id()?,
                realization_set: set.id()?,
                input_geometry: geometry.clone(),
            }],
            vec![],
            vec![LogicalOperationNodeIdV2(0)],
        )?;
        let selections =
            |offers: &[PortRepresentationOfferV1]| -> Result<Vec<PortRepresentationSelectionV1>> {
                offers
                    .iter()
                    .map(|offer| {
                        Ok(PortRepresentationSelectionV1 {
                            port: offer.port.clone(),
                            representation: offer.representation.id()?,
                            residency: offer.residency.clone(),
                        })
                    })
                    .collect()
            };
        let profile = CostProfileV1::new(
            descriptor.id()?,
            descriptor.realization.clone(),
            interface.id()?,
            contract.id()?,
            target.semantic_digest()?,
            geometry,
            selections(&inputs)?,
            selections(&outputs)?,
            CostComponentsV1 { startup_ns: serialization_ns, ..Default::default() },
            0,
            1,
            vec![reference(
                "partial-preparation-cost",
                format!("one local interface-serialization sample: {serialization_ns}ns; backend and transfer costs unmeasured; no comparative performance claim").as_bytes(),
            )?],
        )?;
        let offer = CandidateTupleOfferV1::new(
            LogicalOperationNodeIdV2(0),
            descriptor.id()?,
            target.clone(),
            requirements.clone(),
            inputs,
            outputs,
            profile,
        )?;
        closures.push(OperationPlanningRequestV1::new(
            local,
            contract,
            interface,
            vec![descriptor],
            set,
            objective.clone(),
            vec![offer],
            vec![],
        )?);
    }
    let producers = schedule
        .ops
        .iter()
        .enumerate()
        .flat_map(|(index, op)| op.outputs.iter().map(move |node| (*node, index)))
        .collect::<HashMap<_, _>>();
    let mut edges = Vec::new();
    for (index, op) in schedule.ops.iter().enumerate() {
        for node in &op.inputs {
            if let Some(&producer) = producers.get(node) {
                edges.push(LogicalEdgeV2 {
                    id: LogicalEdgeIdV2(edges.len() as u64),
                    producer: LogicalEdgeEndpointV2 {
                        operation: LogicalOperationNodeIdV2(producer as u64),
                        port: input_port(*node)?,
                    },
                    consumer: LogicalEdgeEndpointV2 {
                        operation: LogicalOperationNodeIdV2(index as u64),
                        port: input_port(*node)?,
                    },
                    value_type: rep(*node).value_type.clone(),
                });
            }
        }
    }
    let has_successor = edges
        .iter()
        .map(|edge| edge.producer.operation)
        .collect::<BTreeSet<_>>();
    let operations = closures
        .iter()
        .enumerate()
        .map(|(index, closure)| {
            let mut node = closure.graph.operations[0].clone();
            node.id = LogicalOperationNodeIdV2(index as u64);
            node
        })
        .collect::<Vec<_>>();
    let roots = operations
        .iter()
        .map(|op| op.id)
        .filter(|id| !has_successor.contains(id))
        .collect();
    let logical = LogicalHGraphV2::new(operations, edges, roots)?;
    let transfers = logical
        .edges
        .iter()
        .map(|edge| {
            let representation = if edge.value_type == value_rep.value_type {
                &value_rep
            } else {
                &token_rep
            };
            Ok(TransferPlanV1::new(
                logical.id()?,
                edge.id,
                target.semantic_digest()?,
                target.semantic_digest()?,
                representation.id()?,
                representation.id()?,
                transfer_implementation.clone(),
                token("local-checked-wire")?,
                0,
                0,
            )?)
        })
        .collect::<Result<Vec<_>>>()?;
    plan_graph_realizations_v1(&GraphPlanningRequestV1 {
        graph: logical,
        objective,
        operations: closures,
        transfers,
    })
    .map(Some)
}

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum WireInput {
    Value { value: Box<OValue> },
    Token { graph: String, node: u64 },
}

impl WireInput {
    fn observation(&self) -> Result<SemanticArtifactRefV1> {
        let bytes = match self {
            Self::Value { value } => value.canonical_bytes(),
            Self::Token { graph, node } => format!("{graph}:{node}").into_bytes(),
        };
        reference(
            if matches!(self, Self::Value { .. }) {
                "value-observation"
            } else {
                "token-observation"
            },
            &bytes,
        )
    }
}

struct OirPhysicalSession {
    plan: Option<GraphRealizationPlanV1>,
    ops: Vec<ReadyOp>,
    graph_digest: String,
    value_nodes: HashMap<NodeId, PlanNodeId>,
    incoming: HashMap<(PlanNodeId, ComputationTokenV1), LogicalEdgeIdV2>,
    completed: BTreeSet<usize>,
    transport: Arc<dyn OirPhysicalTransportV1>,
    observations: Arc<Mutex<ExecutionObservations>>,
}

impl OirPhysicalSession {
    fn new(
        admitted: &AdmittedExecution<'_>,
        transport: Arc<dyn OirPhysicalTransportV1>,
        observations: Arc<Mutex<ExecutionObservations>>,
    ) -> Result<Self> {
        let plan = plan_admitted_oir_physical_v1(admitted)?;
        let ops = ReadySchedule::derive(admitted.graph())
            .map_err(anyhow::Error::msg)?
            .ops;
        let mut incoming = HashMap::new();
        // The task graph and all transfer coordinates were generated from this
        // same validated input order; retain that exact edge-to-input map.
        let produced = ops
            .iter()
            .flat_map(|op| op.outputs.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut edge = 0;
        for op in &ops {
            for input in &op.inputs {
                if produced.contains(input) {
                    incoming.insert((op.plan_node, input_port(*input)?), LogicalEdgeIdV2(edge));
                    edge += 1;
                }
            }
        }
        let value_nodes = admitted
            .graph()
            .node_ids()
            .into_iter()
            .filter_map(|id| {
                let node = admitted.graph().node(id)?;
                matches!(node.kind, HNodeKind::Value)
                    .then_some(node.plan_node.map(|plan| (id, plan)))
                    .flatten()
            })
            .collect();
        Ok(Self {
            plan,
            ops,
            graph_digest: admitted.admission().admitted_graph_sha256().into(),
            value_nodes,
            incoming,
            completed: BTreeSet::new(),
            transport,
            observations,
        })
    }

    fn deliver(
        &mut self,
        consumer: PlanNodeId,
        port: ComputationTokenV1,
        input: WireInput,
    ) -> Result<WireInput> {
        let edge = self.incoming.get(&(consumer, port.clone())).copied();
        let observation = input.observation()?;
        let bytes = serde_json::to_vec(&input)?;
        if let Some(edge) = edge {
            self.task(
                PhysicalTaskIdV1::Transfer(edge),
                PhysicalTaskTransitionV1::Started,
            );
        }
        let result = (|| -> Result<WireInput> {
            let delivered = self.transport.transfer(&bytes)?;
            let decoded: WireInput =
                serde_json::from_slice(&delivered).context("decode physical input")?;
            if decoded.observation()? != observation {
                bail!("physical transfer changed its source observation");
            }
            // Canonical identity intentionally normalizes some containers.
            // Admission does not authorize their reordering: renderers and
            // foreign runtimes can observe EntriesMap/Set vector order. The
            // derived typed equality also retains numeric representations:
            // ONumber compares decimal coefficient/exponent and float bytes,
            // including NaN payloads, rather than floating-point arithmetic.
            if decoded != input {
                bail!("physical transfer changed its source carrier structure");
            }
            Ok(decoded)
        })();
        if let Some(edge) = edge {
            self.task(
                PhysicalTaskIdV1::Transfer(edge),
                if result.is_ok() {
                    PhysicalTaskTransitionV1::Succeeded
                } else {
                    PhysicalTaskTransitionV1::Failed
                },
            );
        }
        if result.is_ok() {
            self.observations
                .lock()
                .expect("observation lock")
                .transfers
                .push(OirInputTransferObservationV1 {
                    edge,
                    consumer,
                    port,
                    bytes: bytes.len(),
                    observation,
                });
        }
        result
    }

    fn task(&self, task: PhysicalTaskIdV1, transition: PhysicalTaskTransitionV1) {
        self.observations
            .lock()
            .expect("observation lock")
            .tasks
            .push(PhysicalTaskObservationV1 { task, transition });
    }
}

impl GraphExecutionBoundary for OirPhysicalSession {
    fn prepare_inputs(
        &mut self,
        index: usize,
        frame: &mut GraphEvalFrame,
    ) -> Result<GraphInputRestore> {
        let op = self.ops[index].clone();
        if !op
            .blocked_by
            .iter()
            .all(|producer| self.completed.contains(producer))
        {
            bail!("physical consumer preceded an operation dependency");
        }
        // Stage all delivered values before changing the frame. A rejected
        // transfer cannot partially corrupt either source values or scope.
        let mut replacements = Vec::new();
        for node in &op.inputs {
            let input = if let Some(&id) = self.value_nodes.get(node) {
                WireInput::Value {
                    value: Box::new(frame.value(id)?.clone()),
                }
            } else {
                WireInput::Token {
                    graph: self.graph_digest.clone(),
                    node: node.0,
                }
            };
            let delivered = self.deliver(op.plan_node, input_port(*node)?, input)?;
            if let (Some(&id), WireInput::Value { value }) = (self.value_nodes.get(node), delivered)
            {
                replacements.push((id, *value));
            }
        }
        let WireInput::Value { value } = self.deliver(
            op.plan_node,
            token("ambient-scope")?,
            WireInput::Value {
                value: Box::new(OValue::Scope {
                    bindings: frame.base_scope.clone(),
                }),
            },
        )?
        else {
            bail!("physical ambient scope changed carrier");
        };
        let OValue::Scope { bindings } = *value else {
            bail!("physical ambient scope changed carrier");
        };
        Ok(GraphInputRestore::replace(frame, replacements, bindings))
    }

    fn started(&self, index: usize) {
        self.task(
            PhysicalTaskIdV1::Operation(LogicalOperationNodeIdV2(index as u64)),
            PhysicalTaskTransitionV1::Started,
        );
    }

    fn completed(&mut self, index: usize, success: bool) {
        if success {
            self.completed.insert(index);
        }
        self.task(
            PhysicalTaskIdV1::Operation(LogicalOperationNodeIdV2(index as u64)),
            if success {
                PhysicalTaskTransitionV1::Succeeded
            } else {
                PhysicalTaskTransitionV1::Failed
            },
        );
    }
}

/// Lower and execute a normal OIR program without caller-built operation or
/// edge records. The explicit policy is installed for admission and restored
/// afterward. Parsed `.O` syntax lowers through `OIrProgram::lower` unchanged.
pub fn execute_oir_physical_v1(
    evaluator: &mut Evaluator,
    program: &OIrProgram,
    scope: &mut HashMap<String, OValue>,
    policy: Policy,
    transport: Arc<dyn OirPhysicalTransportV1>,
) -> Result<OirPhysicalExecutionReportV1> {
    if evaluator.physical_attempt_adapter().is_some() {
        bail!(
            "local OIR physical transport cannot override an explicitly selected remote provider"
        );
    }
    let previous = evaluator.set_policy(policy);
    let result = (|| {
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).map_err(anyhow::Error::msg)?;
        crate::hgraph::solve::solve_types(&mut graph)?;
        let runtime = evaluator.try_admission_runtime_binding(&plan)?;
        let evidence = crate::evidence::analyze_execution(program, &plan, &graph, runtime.clone())?;
        let admitted =
            crate::evidence::admit_execution(program, &plan, graph, policy, runtime, evidence)?;
        let observations = Arc::new(Mutex::new(ExecutionObservations::default()));
        let session = OirPhysicalSession::new(&admitted, transport, observations.clone())?;
        let physical_plan = session.plan.clone();
        let operations = session.ops.iter().map(|op| op.plan_node).collect();
        let leases = admitted.executable_leases()?;
        let backends = plan
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                crate::ir::PlanNodeKind::Exec { backend, .. }
                    if backend.execution == crate::ir::ExecutionMode::Shim =>
                {
                    Some(backend.canonical.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let generations = backends
            .into_iter()
            .map(|backend| {
                admitted
                    .backend_launch_generation_sha256(&backend)
                    .map(|generation| (backend, generation))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let coordinator =
            crate::executor::Coordinator::new(admitted)?.with_execution_boundary(session);
        let previous_leases = evaluator.install_executable_leases(Some(leases));
        let previous_generations = evaluator.install_backend_launch_generations(Some(generations));
        let execution = coordinator.run_host(evaluator, scope, None);
        evaluator.install_backend_launch_generations(previous_generations);
        evaluator.install_executable_leases(previous_leases);
        let value = execution?;
        let mut observations = observations.lock().expect("observation lock");
        Ok(OirPhysicalExecutionReportV1 {
            value,
            plan: physical_plan,
            operations,
            tasks: std::mem::take(&mut observations.tasks),
            transfers: std::mem::take(&mut observations.transfers),
        })
    })();
    evaluator.set_policy(previous);
    result
}
