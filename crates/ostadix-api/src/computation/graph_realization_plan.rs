//! Multi-operation planning and explicit physical transfer execution.
//!
//! V1 operation closures remain single-operation, authority-free records. This
//! layer composes those closures with a checked value-flow graph and fixed
//! transfer offers. It ranks compatible operation offers without constructing
//! their Cartesian product. Executing a plan requires separately supplied,
//! trusted adapters: record identities never grant execution authority.
//!
//! Correctness is scoped to each adapter's admitted interactions and required
//! observation check. Successful checks are execution observations, not a proof
//! of unrestricted behavioral equivalence between foreign runtimes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{mpsc, Arc};

use anyhow::{anyhow, bail, Context, Result};

use super::realization_plan::*;
use crate::computation_core::{
    ComputationTokenV1, OperationContractV1, OperationInterfaceV1, RealizationDescriptorIdV1,
    RealizationDescriptorV1, SemanticArtifactRefV1,
};
use crate::placement::SemanticDigestV1;
use crate::resource_identity::ArtifactId;

/// Entry `i` supplies the unchanged V1 closure for graph operation `i`.
/// Its private graph and candidate coordinates retain their V1 node ID zero.
#[derive(Clone, Debug)]
pub struct GraphPlanningRequestV1 {
    pub graph: LogicalHGraphV2,
    pub objective: ObjectiveV1,
    pub operations: Vec<OperationPlanningRequestV1>,
    /// Exactly one fixed physical transfer offer per logical edge.
    pub transfers: Vec<TransferPlanV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PhysicalTaskIdV1 {
    Operation(LogicalOperationNodeIdV2),
    Transfer(LogicalEdgeIdV2),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalTaskNodeV1 {
    pub id: PhysicalTaskIdV1,
    pub dependencies: Vec<PhysicalTaskIdV1>,
}

/// Incremental readiness avoids rescanning a large graph on every completion.
struct TaskReadiness {
    ready: BTreeSet<PhysicalTaskIdV1>,
    pending: BTreeMap<PhysicalTaskIdV1, usize>,
    successors: BTreeMap<PhysicalTaskIdV1, Vec<PhysicalTaskIdV1>>,
}

impl TaskReadiness {
    fn new(tasks: &[PhysicalTaskNodeV1]) -> Self {
        let mut state = Self {
            ready: BTreeSet::new(),
            pending: BTreeMap::new(),
            successors: BTreeMap::new(),
        };
        for task in tasks {
            state.pending.insert(task.id, task.dependencies.len());
            if task.dependencies.is_empty() {
                state.ready.insert(task.id);
            }
            for dependency in &task.dependencies {
                state
                    .successors
                    .entry(*dependency)
                    .or_default()
                    .push(task.id);
            }
        }
        state
    }

    fn next(&mut self) -> Option<PhysicalTaskIdV1> {
        let id = self.ready.pop_first()?;
        self.pending.remove(&id);
        Some(id)
    }

    fn complete(&mut self, id: PhysicalTaskIdV1) {
        for successor in self.successors.get(&id).into_iter().flatten() {
            let degree = self
                .pending
                .get_mut(successor)
                .expect("successor waits for every dependency");
            *degree -= 1;
            if *degree == 0 {
                self.ready.insert(*successor);
            }
        }
    }
}

/// Opaque compiled closure. Accessors expose the descriptive deployment and
/// actual operation/transfer DAG; execution rechecks the complete request.
#[derive(Clone, Debug)]
pub struct GraphRealizationPlanV1 {
    request: GraphPlanningRequestV1,
    deployment: DeploymentPlanV2,
    tasks: Vec<PhysicalTaskNodeV1>,
    predicted_total_ns: u64,
}

impl GraphRealizationPlanV1 {
    pub fn deployment(&self) -> &DeploymentPlanV2 {
        &self.deployment
    }

    pub fn tasks(&self) -> &[PhysicalTaskNodeV1] {
        &self.tasks
    }

    pub fn predicted_total_ns(&self) -> u64 {
        self.predicted_total_ns
    }
}

fn offer_matches_edge(
    candidate: &RealizationCandidateTupleV1,
    port: &ComputationTokenV1,
    target: &SemanticDigestV1,
    representation: &PhysicalRepresentationIdV1,
    output: bool,
) -> bool {
    candidate.target == *target
        && (if output {
            &candidate.outputs
        } else {
            &candidate.inputs
        })
        .iter()
        .any(|selection| &selection.port == port && &selection.representation == representation)
}

/// Plan a DAG against exact, fixed transfer coordinates. Transfer alternatives
/// must be planned in separate requests; no unbounded combination search occurs.
pub fn plan_graph_realizations_v1(
    request: &GraphPlanningRequestV1,
) -> Result<GraphRealizationPlanV1> {
    request.graph.validate()?;
    request.objective.validate()?;
    let graph = &request.graph;
    if request.operations.len() != graph.operations.len() {
        bail!("graph closures must exactly cover all operations");
    }
    if request.transfers.len() != graph.edges.len() {
        bail!("physical transfers must exactly cover logical edges");
    }
    let graph_id = graph.id()?;
    let mut transfers = BTreeMap::new();
    for transfer in &request.transfers {
        transfer.validate()?;
        if transfer.logical_hgraph != graph_id
            || transfer.edge.0 >= graph.edges.len() as u64
            || transfers.insert(transfer.edge, transfer).is_some()
        {
            bail!("transfer has a forged graph/edge coordinate or duplicate edge");
        }
    }
    let mut candidate_count = 0usize;
    for (index, closure) in request.operations.iter().enumerate() {
        closure.validate()?;
        let mut local = graph.operations[index].clone();
        local.id = LogicalOperationNodeIdV2(0);
        if closure.graph.operations.as_slice() != [local] || closure.objective != request.objective
        {
            bail!("operation {index} closure does not match graph coordinates and objective");
        }
        candidate_count = candidate_count
            .checked_add(closure.offers.len())
            .ok_or_else(|| anyhow!("graph candidate count overflow"))?;
        if candidate_count > MAX_REALIZATION_PLAN_CANDIDATES_V1 {
            bail!("graph candidate count exceeds the planning limit");
        }
    }
    let mut incident = vec![Vec::new(); graph.operations.len()];
    for edge in &graph.edges {
        incident[edge.producer.operation.0 as usize].push(edge);
        incident[edge.consumer.operation.0 as usize].push(edge);
        let producer = &request.operations[edge.producer.operation.0 as usize].interface;
        let consumer = &request.operations[edge.consumer.operation.0 as usize].interface;
        if !producer
            .outputs
            .iter()
            .any(|port| port.name == edge.producer.port && port.value_type == edge.value_type)
            || !consumer
                .inputs
                .iter()
                .any(|port| port.name == edge.consumer.port && port.value_type == edge.value_type)
        {
            bail!(
                "edge {} has an unknown port or mismatched semantic value type",
                edge.id.0
            );
        }
    }

    let mut operations = Vec::with_capacity(graph.operations.len());
    let mut total = 0u64;
    for (index, closure) in request.operations.iter().enumerate() {
        let id = LogicalOperationNodeIdV2(index as u64);
        let mut compatible = closure.clone();
        compatible.offers = closure
            .offers
            .iter()
            .filter_map(|offer| {
                let candidate = match offer.candidate() {
                    Ok(candidate) => candidate,
                    Err(error) => return Some(Err(error)),
                };
                let matches = incident[index].iter().all(|edge| {
                    let transfer = transfers[&edge.id];
                    (edge.producer.operation != id
                        || offer_matches_edge(
                            &candidate,
                            &edge.producer.port,
                            &transfer.source_target,
                            &transfer.source_representation,
                            true,
                        ))
                        && (edge.consumer.operation != id
                            || offer_matches_edge(
                                &candidate,
                                &edge.consumer.port,
                                &transfer.destination_target,
                                &transfer.destination_representation,
                                false,
                            ))
                });
                matches.then(|| Ok(offer.clone()))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let local = plan_operation_realization_v1(&compatible)?;
        let mut operation = local
            .operations
            .into_iter()
            .next()
            .expect("V1 has one operation");
        let selected = operation.selection.as_mut().ok_or_else(|| {
            anyhow!("operation {index} has no compatible realization for its physical edges")
        })?;
        selected.logical_operation = id;
        operation.logical_operation = id;
        for assessment in &mut operation.candidates {
            assessment.candidate.logical_operation = id;
            if assessment.disposition == CandidateDispositionV1::Selected {
                total = total
                    .checked_add(
                        assessment
                            .predicted_total_ns
                            .expect("V1 checked selected cost"),
                    )
                    .ok_or_else(|| anyhow!("graph operation costs overflow u64"))?;
            }
        }
        operations.push(operation);
    }
    let mut transfer_ids = Vec::with_capacity(request.transfers.len());
    for transfer in transfers.values() {
        total = total
            .checked_add(transfer.estimated_cost_ns)
            .ok_or_else(|| anyhow!("graph transfer costs overflow u64"))?;
        transfer_ids.push(transfer.id()?);
    }
    if request
        .objective
        .maximum_total_ns
        .is_some_and(|maximum| total > maximum)
    {
        bail!("whole graph predicted cost {total} exceeds the objective maximum");
    }
    let mut tasks = Vec::with_capacity(graph.operations.len() + graph.edges.len());
    for operation in &graph.operations {
        tasks.push(PhysicalTaskNodeV1 {
            id: PhysicalTaskIdV1::Operation(operation.id),
            dependencies: incident[operation.id.0 as usize]
                .iter()
                .filter(|edge| edge.consumer.operation == operation.id)
                .map(|edge| PhysicalTaskIdV1::Transfer(edge.id))
                .collect(),
        });
    }
    for edge in &graph.edges {
        tasks.push(PhysicalTaskNodeV1 {
            id: PhysicalTaskIdV1::Transfer(edge.id),
            dependencies: vec![PhysicalTaskIdV1::Operation(edge.producer.operation)],
        });
    }
    let mut readiness = TaskReadiness::new(&tasks);
    let mut schedule = Vec::new();
    while !readiness.pending.is_empty() {
        let next = readiness
            .next()
            .ok_or_else(|| anyhow!("physical task graph contains a cycle"))?;
        readiness.complete(next);
        if let PhysicalTaskIdV1::Operation(id) = next {
            schedule.push(id);
        }
    }
    let deployment = DeploymentPlanV2::new(
        graph_id,
        request.objective.id()?,
        operations,
        schedule,
        transfer_ids,
    )?;
    Ok(GraphRealizationPlanV1 {
        request: request.clone(),
        deployment,
        tasks,
        predicted_total_ns: total,
    })
}

/// The envelope carries no authority. Payloads can be bytes, OValue, or native
/// handle types; their transport and observations belong to explicit adapters.
#[derive(Clone, Debug)]
pub struct PhysicalValueV1<T> {
    pub value_type: SemanticArtifactRefV1,
    pub representation: PhysicalRepresentationIdV1,
    pub residency: ValueResidencyV1,
    pub payload: T,
}

pub type PhysicalPortValuesV1<T> = BTreeMap<ComputationTokenV1, PhysicalValueV1<T>>;
pub type GraphExternalInputsV1<T> = BTreeMap<LogicalEdgeEndpointV2, PhysicalValueV1<T>>;

/// A trusted embedding supplies an adapter at the exact descriptor and target
/// coordinate. `admit` must discharge dynamic/state/actor requirements and bind
/// implementation identity before any tasks start. No default admits work.
pub trait PhysicalOperationAdapterV1<T>: Send + Sync {
    fn admit(
        &self,
        candidate: &RealizationCandidateTupleV1,
        descriptor: &RealizationDescriptorV1,
        contract: &OperationContractV1,
        interface: &OperationInterfaceV1,
    ) -> Result<()>;
    fn execute(
        &self,
        candidate: &RealizationCandidateTupleV1,
        inputs: PhysicalPortValuesV1<T>,
    ) -> Result<PhysicalPortValuesV1<T>>;
}

/// The observation check must establish commutation modulo the adapter's
/// admitted congruence. It is mandatory on every executed transfer, including
/// same-representation edges. Returning a correctly labelled envelope alone
/// never publishes a consumer input.
pub trait PhysicalTransferAdapterV1<T>: Send + Sync {
    fn admit(&self, transfer: &TransferPlanV1, edge: &LogicalEdgeV2) -> Result<()>;
    /// Freeze the admitted observation before transfer. An immutable content
    /// identity prevents a mutable native handle from replacing its own
    /// baseline while the adapter is running.
    fn observe_source(
        &self,
        transfer: &TransferPlanV1,
        source: &PhysicalValueV1<T>,
    ) -> Result<SemanticArtifactRefV1>;
    fn transfer(
        &self,
        transfer: &TransferPlanV1,
        source: &PhysicalValueV1<T>,
    ) -> Result<PhysicalValueV1<T>>;
    fn check_observation(
        &self,
        transfer: &TransferPlanV1,
        source_observation: &SemanticArtifactRefV1,
        destination: &PhysicalValueV1<T>,
    ) -> Result<()>;
}

/// Registration is a host authority boundary, not deserialization of records.
pub struct PhysicalAdapterRegistryV1<T> {
    operations: BTreeMap<
        (RealizationDescriptorIdV1, SemanticDigestV1),
        Arc<dyn PhysicalOperationAdapterV1<T>>,
    >,
    transfers: BTreeMap<ArtifactId, Arc<dyn PhysicalTransferAdapterV1<T>>>,
}

impl<T> Default for PhysicalAdapterRegistryV1<T> {
    fn default() -> Self {
        Self {
            operations: BTreeMap::new(),
            transfers: BTreeMap::new(),
        }
    }
}

impl<T> PhysicalAdapterRegistryV1<T> {
    pub fn register_operation(
        &mut self,
        descriptor: RealizationDescriptorIdV1,
        target: SemanticDigestV1,
        adapter: Arc<dyn PhysicalOperationAdapterV1<T>>,
    ) -> Result<()> {
        let key = (descriptor, target);
        if self.operations.contains_key(&key) {
            bail!("operation adapter coordinate is already registered");
        }
        self.operations.insert(key, adapter);
        Ok(())
    }

    pub fn register_transfer(
        &mut self,
        identity: ArtifactId,
        adapter: Arc<dyn PhysicalTransferAdapterV1<T>>,
    ) -> Result<()> {
        if self.transfers.contains_key(&identity) {
            bail!("transfer adapter identity is already registered");
        }
        self.transfers.insert(identity, adapter);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalTaskTransitionV1 {
    Started,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalTaskObservationV1 {
    pub task: PhysicalTaskIdV1,
    pub transition: PhysicalTaskTransitionV1,
}

#[derive(Debug)]
pub struct GraphExecutionReportV1<T> {
    pub outputs: BTreeMap<LogicalOperationNodeIdV2, PhysicalPortValuesV1<T>>,
    /// Coordinator-observed transitions in actual receive order.
    pub observations: Vec<PhysicalTaskObservationV1>,
}

#[derive(Debug, thiserror::Error)]
#[error("physical graph execution failed: {message}")]
pub struct GraphExecutionFailureV1 {
    pub message: String,
    pub observations: Vec<PhysicalTaskObservationV1>,
}

fn check_value<T>(
    value: &PhysicalValueV1<T>,
    value_type: &SemanticArtifactRefV1,
    selection: &PortRepresentationSelectionV1,
) -> Result<()> {
    if &value.value_type != value_type
        || value.representation != selection.representation
        || value.residency != selection.residency
    {
        bail!(
            "physical value type, representation, or residency does not match selected port {}",
            selection.port
        );
    }
    Ok(())
}

fn check_ports<T>(
    values: &PhysicalPortValuesV1<T>,
    interface: &OperationInterfaceV1,
    candidate: &RealizationCandidateTupleV1,
    outputs: bool,
) -> Result<()> {
    let ports = if outputs {
        &interface.outputs
    } else {
        &interface.inputs
    };
    let selections = if outputs {
        &candidate.outputs
    } else {
        &candidate.inputs
    };
    if values.len() != ports.len() {
        bail!("physical values do not exactly cover interface ports");
    }
    for port in ports {
        let value = values
            .get(&port.name)
            .ok_or_else(|| anyhow!("missing value for port {}", port.name))?;
        let selection = selections
            .iter()
            .find(|selection| selection.port == port.name)
            .ok_or_else(|| anyhow!("missing selected representation for port {}", port.name))?;
        check_value(value, &port.value_type, selection)?;
    }
    Ok(())
}

enum TaskValues<T> {
    Operation(PhysicalPortValuesV1<T>),
    Transfer(PhysicalValueV1<T>),
}

/// Execute the physical DAG, refilling available slots as each task completes.
/// Every adapter and external input is checked before execution. A failed task
/// suppresses new tasks; already-started tasks are drained and joined, retaining
/// their real observations. Independent effects may overlap only as authorized
/// by the explicitly registered adapters' admission contracts.
pub fn execute_graph_realizations_v1<T: Clone + Send + Sync>(
    plan: &GraphRealizationPlanV1,
    registry: &PhysicalAdapterRegistryV1<T>,
    external: &GraphExternalInputsV1<T>,
    maximum_workers: usize,
) -> std::result::Result<GraphExecutionReportV1<T>, GraphExecutionFailureV1> {
    let mut observations = Vec::new();
    let result = execute_graph_inner(plan, registry, external, maximum_workers, &mut observations);
    match result {
        Ok(outputs) => Ok(GraphExecutionReportV1 {
            outputs,
            observations,
        }),
        Err(error) => Err(GraphExecutionFailureV1 {
            message: format!("{error:#}"),
            observations,
        }),
    }
}

fn execute_graph_inner<T: Clone + Send + Sync>(
    plan: &GraphRealizationPlanV1,
    registry: &PhysicalAdapterRegistryV1<T>,
    external: &GraphExternalInputsV1<T>,
    maximum_workers: usize,
    observations: &mut Vec<PhysicalTaskObservationV1>,
) -> Result<BTreeMap<LogicalOperationNodeIdV2, PhysicalPortValuesV1<T>>> {
    if maximum_workers == 0 {
        bail!("physical executor needs at least one worker");
    }
    let checked = plan_graph_realizations_v1(&plan.request)?;
    if checked.deployment != plan.deployment
        || checked.tasks != plan.tasks
        || checked.predicted_total_ns != plan.predicted_total_ns
    {
        bail!("physical plan differs from its recomputed request");
    }
    let request = &plan.request;
    let graph = &request.graph;
    let candidates = plan
        .deployment
        .selected_candidates()
        .map(|candidate| (candidate.logical_operation, candidate))
        .collect::<BTreeMap<_, _>>();
    let transfers = request
        .transfers
        .iter()
        .map(|transfer| (transfer.edge, transfer))
        .collect::<BTreeMap<_, _>>();
    let incoming = graph
        .edges
        .iter()
        .map(|edge| (edge.consumer.clone(), edge.id))
        .collect::<BTreeMap<_, _>>();
    let mut expected_external = BTreeSet::new();
    for operation in &graph.operations {
        let closure = &request.operations[operation.id.0 as usize];
        let candidate = candidates[&operation.id];
        let adapter = registry
            .operations
            .get(&(candidate.descriptor.clone(), candidate.target.clone()))
            .ok_or_else(|| {
                anyhow!(
                    "operation {} has no explicitly registered adapter",
                    operation.id.0
                )
            })?;
        let descriptor = closure
            .descriptors
            .iter()
            .find(|descriptor| descriptor.id().ok().as_ref() == Some(&candidate.descriptor))
            .ok_or_else(|| anyhow!("selected descriptor missing from operation closure"))?;
        adapter
            .admit(candidate, descriptor, &closure.contract, &closure.interface)
            .with_context(|| format!("operation {} admission", operation.id.0))?;
        for port in &closure.interface.inputs {
            let endpoint = LogicalEdgeEndpointV2 {
                operation: operation.id,
                port: port.name.clone(),
            };
            if !incoming.contains_key(&endpoint) {
                let value = external.get(&endpoint).ok_or_else(|| {
                    anyhow!(
                        "missing external input for operation {} port {}",
                        operation.id.0,
                        port.name
                    )
                })?;
                let selection = candidate
                    .inputs
                    .iter()
                    .find(|selection| selection.port == port.name)
                    .expect("ranked interface input");
                check_value(value, &port.value_type, selection)?;
                expected_external.insert(endpoint);
            }
        }
    }
    if external.keys().cloned().collect::<BTreeSet<_>>() != expected_external {
        bail!("external inputs include unknown ports or replace an admitted transfer edge");
    }
    for edge in &graph.edges {
        let transfer = transfers[&edge.id];
        let adapter = registry.transfers.get(&transfer.adapter).ok_or_else(|| {
            anyhow!(
                "edge {} has no explicitly registered transfer adapter",
                edge.id.0
            )
        })?;
        adapter
            .admit(transfer, edge)
            .with_context(|| format!("edge {} admission", edge.id.0))?;
    }

    let mut values = BTreeMap::<PhysicalTaskIdV1, TaskValues<T>>::new();
    let mut readiness = TaskReadiness::new(&plan.tasks);
    std::thread::scope(|scope| -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        let mut active = 0usize;
        let mut failure = None;
        while !readiness.pending.is_empty() || active != 0 {
            while failure.is_none() && active < maximum_workers {
                let Some(id) = readiness.next() else {
                    break;
                };
                let input = match id {
                    PhysicalTaskIdV1::Operation(operation) => {
                        let mut inputs = BTreeMap::new();
                        for port in &request.operations[operation.0 as usize].interface.inputs {
                            let endpoint = LogicalEdgeEndpointV2 {
                                operation,
                                port: port.name.clone(),
                            };
                            let value = if let Some(edge) = incoming.get(&endpoint) {
                                let TaskValues::Transfer(value) =
                                    &values[&PhysicalTaskIdV1::Transfer(*edge)]
                                else {
                                    unreachable!()
                                };
                                value
                            } else {
                                &external[&endpoint]
                            };
                            inputs.insert(port.name.clone(), value.clone());
                        }
                        TaskValues::Operation(inputs)
                    }
                    PhysicalTaskIdV1::Transfer(edge) => {
                        let edge = &graph.edges[edge.0 as usize];
                        let TaskValues::Operation(outputs) =
                            &values[&PhysicalTaskIdV1::Operation(edge.producer.operation)]
                        else {
                            unreachable!()
                        };
                        TaskValues::Transfer(outputs[&edge.producer.port].clone())
                    }
                };
                let sender = sender.clone();
                let candidates = &candidates;
                let transfers = &transfers;
                observations.push(PhysicalTaskObservationV1 {
                    task: id,
                    transition: PhysicalTaskTransitionV1::Started,
                });
                active += 1;
                scope.spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || -> Result<TaskValues<T>> {
                            match (id, input) {
                                (
                                    PhysicalTaskIdV1::Operation(operation),
                                    TaskValues::Operation(inputs),
                                ) => {
                                    let candidate = candidates[&operation];
                                    let interface =
                                        &request.operations[operation.0 as usize].interface;
                                    check_ports(&inputs, interface, candidate, false)?;
                                    let adapter = &registry.operations
                                        [&(candidate.descriptor.clone(), candidate.target.clone())];
                                    let outputs = adapter.execute(candidate, inputs)?;
                                    check_ports(&outputs, interface, candidate, true)?;
                                    Ok(TaskValues::Operation(outputs))
                                }
                                (
                                    PhysicalTaskIdV1::Transfer(edge_id),
                                    TaskValues::Transfer(source),
                                ) => {
                                    let edge = &graph.edges[edge_id.0 as usize];
                                    let transfer = transfers[&edge_id];
                                    let adapter = &registry.transfers[&transfer.adapter];
                                    let observation = adapter.observe_source(transfer, &source)?;
                                    let observation = SemanticArtifactRefV1::new(
                                        observation.schema,
                                        observation.content,
                                    )?;
                                    let destination = adapter.transfer(transfer, &source)?;
                                    let consumer = candidates[&edge.consumer.operation];
                                    let selection = consumer
                                        .inputs
                                        .iter()
                                        .find(|selection| selection.port == edge.consumer.port)
                                        .expect("checked edge input");
                                    check_value(&destination, &edge.value_type, selection)?;
                                    adapter.check_observation(
                                        transfer,
                                        &observation,
                                        &destination,
                                    )?;
                                    Ok(TaskValues::Transfer(destination))
                                }
                                _ => unreachable!("task input kind follows task identity"),
                            }
                        },
                    ))
                    .unwrap_or_else(|_| Err(anyhow!("physical adapter panicked")));
                    let _ = sender.send((id, result));
                });
            }
            if active == 0 {
                if failure.is_some() {
                    break;
                }
                bail!("physical execution stalled with pending tasks");
            }
            let (id, result) = receiver
                .recv()
                .context("physical worker channel disconnected")?;
            active -= 1;
            observations.push(PhysicalTaskObservationV1 {
                task: id,
                transition: if result.is_ok() {
                    PhysicalTaskTransitionV1::Succeeded
                } else {
                    PhysicalTaskTransitionV1::Failed
                },
            });
            match result {
                Ok(value) => {
                    values.insert(id, value);
                    readiness.complete(id);
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error.context(format!("task {id:?}")));
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    })?;
    Ok(values
        .into_iter()
        .filter_map(|(id, value)| match (id, value) {
            (PhysicalTaskIdV1::Operation(operation), TaskValues::Operation(outputs)) => {
                Some((operation, outputs))
            }
            _ => None,
        })
        .collect())
}
