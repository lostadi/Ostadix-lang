use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{bail, Context, Result};

use crate::effects::{EffectSummary, ResourceKey};
use crate::eval::Policy;
use crate::hgraph::{AdmissionFactKind, HGraph, HNodeKind, NodeId, ReadySchedule};
use crate::ir::{ExecutionPlan, OIrProgram, PlanEdgeKind, PlanNodeId, PlanNodeKind};

use super::analyze::{
    analyze_execution, digest_fields, evidence_bindings, evidence_bundle_sha256, graph_sha256,
};
use super::fact::{
    BackendArtifactV1, DispatchAdapterV1, DispatchLaneV1, DispatchSemanticsV1, EvidenceBindingsV1,
    EvidenceBundleV3, NodeEvidence, PlacementContractV1, RuntimeBindingV1, RuntimeSnapshotKindV1,
    ADMISSION_SCHEMA_V3, ANALYZER_ID_V3, EVIDENCE_SCHEMA_V3,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlockerReasonV1 {
    ValueDependency,
    SourceCompletion,
    ReaderDrain(ResourceKey),
    ResourceVersion(ResourceKey),
    ActorVersion(String),
    BranchControl(String),
}

impl BlockerReasonV1 {
    fn label(&self) -> String {
        match self {
            Self::ValueDependency => "value".to_string(),
            Self::SourceCompletion => "source-completion".to_string(),
            Self::ReaderDrain(resource) => format!("reader-drain:{resource}"),
            Self::ResourceVersion(resource) => format!("resource:{resource}"),
            Self::ActorVersion(actor) => format!("actor:{actor}"),
            Self::BranchControl(label) => format!("control:{label}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationBlockerV1 {
    pub predecessor: PlanNodeId,
    pub reasons: Vec<BlockerReasonV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceRetentionReasonV1 {
    LeftToRightRegion,
    ActorIdentity,
    ResourceConflict,
    StrictFailStopUnproven,
    ConservativeSourceSequence,
}

impl SequenceRetentionReasonV1 {
    fn name(self) -> &'static str {
        match self {
            Self::LeftToRightRegion => "left-to-right-region",
            Self::ActorIdentity => "actor-identity",
            Self::ResourceConflict => "resource-conflict",
            Self::StrictFailStopUnproven => "strict-fail-stop-unproven",
            Self::ConservativeSourceSequence => "conservative-source-sequence",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedSequenceV1 {
    pub predecessor: PlanNodeId,
    pub successor: PlanNodeId,
    pub completion: NodeId,
    pub reason: SequenceRetentionReasonV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedOperationV1 {
    pub plan_node: PlanNodeId,
    pub ordinal: u64,
    pub evidence: NodeEvidence,
    pub blockers: Vec<OperationBlockerV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionAdmissionV3 {
    schema: &'static str,
    bindings: EvidenceBindingsV1,
    analyzer: &'static str,
    runtime_snapshot_kind: RuntimeSnapshotKindV1,
    backend_artifacts: Vec<BackendArtifactV1>,
    evidence_sha256: String,
    admitted_graph_sha256: String,
    admission_sha256: String,
    base_policy: Policy,
    operations: Vec<AdmittedOperationV1>,
    retained_sequences: Vec<RetainedSequenceV1>,
    waves: Vec<Vec<PlanNodeId>>,
}

impl ExecutionAdmissionV3 {
    pub fn schema(&self) -> &'static str {
        self.schema
    }

    pub fn bindings(&self) -> &EvidenceBindingsV1 {
        &self.bindings
    }

    pub fn analyzer(&self) -> &'static str {
        self.analyzer
    }

    pub fn runtime_snapshot_kind(&self) -> RuntimeSnapshotKindV1 {
        self.runtime_snapshot_kind
    }

    pub fn backend_artifacts(&self) -> &[BackendArtifactV1] {
        &self.backend_artifacts
    }

    pub fn evidence_sha256(&self) -> &str {
        &self.evidence_sha256
    }

    pub fn admitted_graph_sha256(&self) -> &str {
        &self.admitted_graph_sha256
    }

    pub fn admission_sha256(&self) -> &str {
        &self.admission_sha256
    }

    pub fn base_policy(&self) -> Policy {
        self.base_policy
    }

    pub fn operations(&self) -> &[AdmittedOperationV1] {
        &self.operations
    }

    pub fn waves(&self) -> &[Vec<PlanNodeId>] {
        &self.waves
    }

    /// Stable, non-executing explanation of the exact admitted scheduling
    /// geometry. Unknown cost/capacity facts remain explicit; waves describe
    /// legal readiness, not observed worker overlap.
    pub fn to_explanation_text(&self) -> String {
        let mut out = format!("; ExecutionAdmission {}\n", self.schema);
        writeln!(
            out,
            "binding lowered-oir-sha256={} plan-sha256={} analyzed-graph-sha256={}",
            self.bindings.oir_sha256,
            self.bindings.plan_sha256,
            self.bindings.analyzed_graph_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(
            out,
            "binding backend-set-sha256={} environment-sha256={} ambient-world-sha256={}",
            self.bindings.backend_set_sha256,
            self.bindings.environment_sha256,
            self.bindings.ambient_world_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(
            out,
            "binding analyzer-sha256={} evidence-sha256={} admitted-graph-sha256={} admission-sha256={}",
            self.bindings.analyzer_sha256,
            self.evidence_sha256,
            self.admitted_graph_sha256,
            self.admission_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(out, "analyzer {}", self.analyzer).expect("writing to a String cannot fail");
        writeln!(
            out,
            "runtime-snapshot kind={} dispatch-context={}",
            self.runtime_snapshot_kind.name(),
            match self.runtime_snapshot_kind {
                RuntimeSnapshotKindV1::Execution => "execution",
                RuntimeSnapshotKindV1::Inspection => "inspection-only",
            }
        )
        .expect("writing to a String cannot fail");
        for artifact in &self.backend_artifacts {
            writeln!(
                out,
                "backend-artifact canonical={} identity={} state={} sha256={}{}",
                artifact.canonical_backend,
                artifact.resolved_identity,
                artifact.state.name(),
                artifact.state.sha256().unwrap_or("none"),
                match &artifact.state {
                    crate::evidence::BackendArtifactStateV1::Unreadable { error_kind } => {
                        format!(" error-kind={error_kind}")
                    }
                    _ => String::new(),
                }
            )
            .expect("writing to a String cannot fail");
        }
        writeln!(out, "policy {}", policy_name(self.base_policy))
            .expect("writing to a String cannot fail");

        for operation in &self.operations {
            let evidence = &operation.evidence;
            writeln!(
                out,
                "operation P{} admitted=yes ordinal={}",
                operation.plan_node.0, operation.ordinal
            )
            .expect("writing to a String cannot fail");
            writeln!(
                out,
                "  type constraints-solved={} provenance={} domain-bounds=0x{:04x} representation-bounds=0x{:04x} fidelity-bound={}",
                yes_no(evidence.type_contract.constraints_solved),
                evidence.type_contract.provenance.name(),
                evidence.type_contract.output_domain_bits,
                evidence.type_contract.output_representation_bits,
                evidence.type_contract.output_fidelity
            )
            .expect("writing to a String cannot fail");
            writeln!(
                out,
                "  dispatch lane={} adapter={} semantics={} preparation={}",
                evidence.dispatch_contract.lane.name(),
                evidence.dispatch_contract.adapter.name(),
                evidence.dispatch_contract.semantics.name(),
                if evidence.dispatch_contract.send_only_preparation {
                    "deferred-materialized-input-check"
                } else {
                    "coordinator-owned"
                }
            )
            .expect("writing to a String cannot fail");
            writeln!(
                out,
                "  effects provenance={} closed={} failure={} reads=[{}] writes=[{}]",
                evidence.effect_contract.provenance.name(),
                yes_no(evidence.effect_contract.footprint_closed),
                evidence.failure_contract.class.name(),
                resources_text(&evidence.effect_contract.reads),
                resources_text(&evidence.effect_contract.writes)
            )
            .expect("writing to a String cannot fail");
            writeln!(
                out,
                "  capability={} provenance={} placement={} provenance={}",
                evidence.capability_disposition.name(),
                evidence.capability_provenance.name(),
                evidence.placement.name(),
                evidence.placement_provenance.name()
            )
            .expect("writing to a String cannot fail");
            writeln!(
                out,
                "  demand cpu={} memory={} file-descriptors={} process-slots={} provenance={} cost={} cost-provenance={}",
                optional_number(evidence.resource_demand.cpu_units),
                optional_number(evidence.resource_demand.hard_memory_bytes),
                optional_number(evidence.resource_demand.file_descriptors),
                optional_number(evidence.resource_demand.process_slots),
                evidence.resource_demand.provenance.name(),
                optional_number(evidence.cost_estimate.expected_duration_micros),
                evidence.cost_estimate.provenance.name()
            )
            .expect("writing to a String cannot fail");
            let facts = AdmissionFactKind::ALL
                .iter()
                .map(|fact| format!("{}:{}", fact.name(), evidence.provenance_for(*fact).name()))
                .collect::<Vec<_>>()
                .join(",");
            writeln!(out, "  evidence-inputs=[{facts}]").expect("writing to a String cannot fail");
            let blockers = operation
                .blockers
                .iter()
                .map(|blocker| {
                    format!(
                        "P{}:{}",
                        blocker.predecessor.0,
                        blocker
                            .reasons
                            .iter()
                            .map(BlockerReasonV1::label)
                            .collect::<Vec<_>>()
                            .join("+")
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            writeln!(out, "  blockers=[{blockers}]").expect("writing to a String cannot fail");
        }

        for sequence in &self.retained_sequences {
            writeln!(
                out,
                "sequence P{} -> P{} retained reason={} token=N{}",
                sequence.predecessor.0,
                sequence.successor.0,
                sequence.reason.name(),
                sequence.completion.0
            )
            .expect("writing to a String cannot fail");
        }
        for (index, wave) in self.waves.iter().enumerate() {
            let nodes = wave
                .iter()
                .map(|node| format!("P{}", node.0))
                .collect::<Vec<_>>()
                .join(",");
            writeln!(out, "wave {index} [{nodes}]").expect("writing to a String cannot fail");
        }
        out.push_str(
            "admission-note waves describe the legal static frontier, not observed dispatch\n",
        );
        out.push_str(
            "admission-note dispatch adapter IDs are evidence-bound; runtime preparation may validate but cannot reclassify an operation\n",
        );
        out.push_str(
            "admission-note explicit-autonomous-unordered dispatch is a source-level semantic opt-in; deterministic result settlement does not roll back external effects from already-started hosted tasks\n",
        );
        out.push_str(
            "admission-note local-worker runtime uses a fixed-size per-run pool with per-completion wakeups; static waves are not pool batches or capacity promises\n",
        );
        out.push_str(
            "admission-note fallible local-worker outcomes may complete out of order but remain provisional and settle at the contiguous serial-topological prefix\n",
        );
        out.push_str(
            "admission-note verified-pure infallible local-worker outputs may provisionally unlock only equally safe worker dependents; dependent NodeStarted may precede producer NodeFinished, durable settlement remains serial-topological, and any earlier failure revokes provisionally published outputs\n",
        );
        out.push_str(
            "admission-note ambient-world-sha256 is descriptive HostWorld context, not governed authority\n",
        );
        out.push_str(
            "admission-note backend artifact states distinguish hashed, missing, non-regular, and unreadable paths\n",
        );
        out.push_str(
            "admission-note adapter/environment rechecks are best-effort snapshots; v3 does not pin an opened artifact or frozen child environment and cannot prove bytes/environment observed at spawn\n",
        );
        out.push_str(
            "admission-note backend binding does not cover live actor state/generation or external toolchain closure\n",
        );
        out.push_str(
            "admission-note caller initial scope shape and values are installed after admission and are not digest-bound in v3\n",
        );
        out.push_str(
            "admission-note local placement is descriptive in v3 and does not assert a current lease\n",
        );
        out
    }
}

/// Frozen authority boundary consumed by the coordinator. Its fields and
/// constructor are private; the only route from a solved graph to execution is
/// the digest-checking admission compiler below.
pub struct AdmittedExecution<'a> {
    program: &'a OIrProgram,
    plan: &'a ExecutionPlan,
    graph: HGraph,
    runtime: RuntimeBindingV1,
    admission: ExecutionAdmissionV3,
}

impl<'a> AdmittedExecution<'a> {
    pub fn program(&self) -> &'a OIrProgram {
        self.program
    }

    pub fn plan(&self) -> &'a ExecutionPlan {
        self.plan
    }

    pub fn graph(&self) -> &HGraph {
        &self.graph
    }

    pub fn admission(&self) -> &ExecutionAdmissionV3 {
        &self.admission
    }

    pub(crate) fn verify_runtime(&self, current: &RuntimeBindingV1) -> Result<()> {
        let mut changed = Vec::new();
        if self.runtime.snapshot_kind != current.snapshot_kind {
            changed.push("snapshot kind");
        }
        if self.runtime.backend_artifacts != current.backend_artifacts {
            changed.push("backend artifacts");
        }
        if self.runtime.backend_set_sha256 != current.backend_set_sha256 {
            changed.push("backend-set digest");
        }
        if self.runtime.environment_sha256 != current.environment_sha256 {
            changed.push("environment digest");
        }
        if self.runtime.ambient_world_sha256 != current.ambient_world_sha256 {
            changed.push("ambient World digest");
        }
        if !changed.is_empty() {
            bail!(
                "execution admission runtime binding is stale; changed components: {}",
                changed.join(", ")
            );
        }
        Ok(())
    }
}

/// Compile a solved graph and its exact evidence bundle into the only graph
/// type accepted by the runtime coordinator.
pub fn admit_execution<'a>(
    program: &'a OIrProgram,
    plan: &'a ExecutionPlan,
    mut graph: HGraph,
    base_policy: Policy,
    runtime: RuntimeBindingV1,
    evidence: EvidenceBundleV3,
) -> Result<AdmittedExecution<'a>> {
    if plan != &program.plan() {
        bail!(
            "execution admission requires the canonical ExecutionPlan derived from the exact lowered OIR"
        );
    }
    graph
        .validate_execution_source(program, plan)
        .map_err(anyhow::Error::msg)
        .context("admission rejected OIR/plan/HGraph provenance")?;
    if evidence.schema != EVIDENCE_SCHEMA_V3 || evidence.analyzer != ANALYZER_ID_V3 {
        bail!("unsupported or untrusted evidence bundle schema/analyzer");
    }
    if evidence.runtime != runtime {
        bail!("evidence runtime binding is stale or belongs to another execution context");
    }
    let expected_bindings = evidence_bindings(program, plan, &graph, &runtime);
    if evidence.bindings != expected_bindings {
        bail!(
            "evidence digest binding mismatch: lowered OIR, plan, graph, backend, environment, or ambient World changed"
        );
    }
    let baseline = analyze_execution(program, plan, &graph, runtime.clone())
        .context("admission could not reproduce the trusted analyzer result")?;
    let baseline_by_plan = baseline
        .nodes
        .iter()
        .map(|node| (node.plan_node, node))
        .collect::<BTreeMap<_, _>>();

    let mut by_plan = BTreeMap::new();
    for node in &evidence.nodes {
        if by_plan.insert(node.plan_node, node).is_some() {
            bail!("evidence repeats operation {}", node.plan_node.0);
        }
    }
    let expected_operations = graph.op_map.keys().copied().collect::<BTreeSet<_>>();
    if by_plan.keys().copied().collect::<BTreeSet<_>>() != expected_operations {
        bail!("evidence operation inventory does not match the executable graph");
    }
    let flat = program.flatten_for_plan();
    for (plan_node, node) in &by_plan {
        let expected = baseline_by_plan
            .get(plan_node)
            .with_context(|| format!("trusted analyzer omitted operation {}", plan_node.0))?;
        validate_node_evidence(&graph, plan, flat[plan_node.0], *plan_node, node, expected)?;
    }

    let evidence_sha256 = evidence_bundle_sha256(&evidence);
    for plan_node in expected_operations {
        for fact in AdmissionFactKind::ALL {
            graph
                .add_admission_evidence_input(plan_node, fact)
                .map_err(anyhow::Error::msg)?;
        }
    }
    graph
        .validate_admitted_execution_graph()
        .map_err(anyhow::Error::msg)
        .context("admission compiler produced an invalid admitted graph")?;
    let admitted_graph_sha256 = graph_sha256(&graph);
    let schedule = ReadySchedule::derive(&graph)
        .map_err(anyhow::Error::msg)
        .context("admitted graph has no executable ready schedule")?;
    let waves = schedule.waves().map_err(anyhow::Error::msg)?;
    let operations = explain_operations(&graph, &schedule, &by_plan)?;
    let retained_sequences = explain_sequences(plan, &graph);
    let admission_sha256 = digest_fields(
        "ostadix-execution-admission/v3",
        &[
            &evidence_sha256,
            &admitted_graph_sha256,
            policy_name(base_policy),
        ],
    );

    let admission = ExecutionAdmissionV3 {
        schema: ADMISSION_SCHEMA_V3,
        bindings: expected_bindings,
        analyzer: ANALYZER_ID_V3,
        runtime_snapshot_kind: runtime.snapshot_kind(),
        backend_artifacts: runtime.backend_artifacts().to_vec(),
        evidence_sha256,
        admitted_graph_sha256,
        admission_sha256,
        base_policy,
        operations,
        retained_sequences,
        waves,
    };
    Ok(AdmittedExecution {
        program,
        plan,
        graph,
        runtime,
        admission,
    })
}

fn validate_node_evidence(
    graph: &HGraph,
    plan: &ExecutionPlan,
    oir: &crate::ir::OIr,
    plan_node: PlanNodeId,
    evidence: &NodeEvidence,
    expected: &NodeEvidence,
) -> Result<()> {
    if evidence.type_contract != expected.type_contract
        || evidence.effect_contract != expected.effect_contract
        || evidence.dispatch_contract != expected.dispatch_contract
        || evidence.capability_disposition != expected.capability_disposition
        || evidence.capability_provenance != expected.capability_provenance
        || evidence.placement != expected.placement
        || evidence.placement_provenance != expected.placement_provenance
        || evidence.failure_contract != expected.failure_contract
        || evidence.resource_demand != expected.resource_demand
    {
        bail!(
            "operation {} hard evidence differs from the trusted analyzer result",
            plan_node.0
        );
    }
    if !evidence.type_contract.constraints_solved {
        bail!("operation {} lacks solved type constraints", plan_node.0);
    }
    let output = graph
        .node(
            graph
                .op_for(plan_node)
                .expect("inventory checked")
                .value_output,
        )
        .expect("validated graph output exists");
    if evidence.type_contract.output_domain_bits != output.domain.bits()
        || evidence.type_contract.output_representation_bits != output.rep.bits()
    {
        bail!("operation {} type evidence is stale", plan_node.0);
    }
    let summary = graph
        .effect_summary(plan_node)
        .expect("validated graph effect summary exists");
    let (reads, writes) = summary.scheduling_accesses();
    if evidence
        .effect_contract
        .reads
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != reads
        || evidence
            .effect_contract
            .writes
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != writes
    {
        bail!("operation {} effect evidence is stale", plan_node.0);
    }
    if summary.unknown && evidence.effect_contract.footprint_closed {
        bail!(
            "operation {} cannot close an unknown effect footprint",
            plan_node.0
        );
    }
    if evidence.effect_contract.footprint_closed
        && !evidence
            .effect_contract
            .provenance
            .may_close_unknown_effect()
    {
        bail!(
            "operation {} effect provenance cannot close a footprint",
            plan_node.0
        );
    }
    match evidence.dispatch_contract.lane {
        DispatchLaneV1::LocalWorker => {
            let explicitly_autonomous_shim = evidence.dispatch_contract.adapter
                == DispatchAdapterV1::AutonomousEphemeralShimV1
                && crate::hgraph::from_oir::autonomous_ephemeral_group(plan, plan_node, oir)
                    .is_some()
                && summary.unknown
                && !evidence.effect_contract.footprint_closed;
            let strict_semantics =
                evidence.dispatch_contract.semantics == DispatchSemanticsV1::StrictEquivalent;
            let autonomous_semantics = evidence.dispatch_contract.semantics
                == DispatchSemanticsV1::ExplicitAutonomousUnordered;
            if !evidence.dispatch_contract.adapter.is_local_worker()
                || !evidence.dispatch_contract.send_only_preparation
                || evidence.placement != PlacementContractV1::LocalWorker
                || !((strict_semantics
                    && (summary.is_verified_pure_infallible()
                        || evidence.failure_contract.class
                            == crate::evidence::FailureClassV1::MayFailNoExternalEffects))
                    || (explicitly_autonomous_shim
                        && autonomous_semantics
                        && evidence.failure_contract.class
                            == crate::evidence::FailureClassV1::MayFailUnorderedExternalEffects))
            {
                bail!(
                    "operation {} has an unsafe worker dispatch claim",
                    plan_node.0
                );
            }
        }
        DispatchLaneV1::Coordinator => {
            if evidence.dispatch_contract.adapter != DispatchAdapterV1::CoordinatorV1
                || evidence.dispatch_contract.semantics != DispatchSemanticsV1::StrictEquivalent
                || evidence.dispatch_contract.send_only_preparation
                || evidence.placement != PlacementContractV1::LocalCoordinator
            {
                bail!(
                    "operation {} has an incoherent coordinator dispatch claim",
                    plan_node.0
                );
            }
        }
        _ => bail!(
            "operation {} selects an execution lane unsupported by this admission compiler",
            plan_node.0
        ),
    }
    Ok(())
}

fn explain_operations(
    graph: &HGraph,
    schedule: &ReadySchedule,
    evidence: &BTreeMap<PlanNodeId, &NodeEvidence>,
) -> Result<Vec<AdmittedOperationV1>> {
    let edge_to_index = schedule
        .ops
        .iter()
        .enumerate()
        .map(|(index, op)| (op.edge, index))
        .collect::<BTreeMap<_, _>>();
    let sequences = graph
        .sequence_dependencies
        .iter()
        .map(|dependency| (dependency.predecessor, dependency.successor))
        .collect::<BTreeSet<_>>();
    let mut operations = Vec::with_capacity(schedule.ops.len());
    for op in &schedule.ops {
        let mut blockers: BTreeMap<PlanNodeId, BTreeSet<BlockerReasonV1>> = BTreeMap::new();
        for input in &op.inputs {
            let Some(node) = graph.node(*input) else {
                continue;
            };
            let Some(producer_edge) = node.producer else {
                continue;
            };
            let Some(producer_index) = edge_to_index.get(&producer_edge).copied() else {
                continue;
            };
            let predecessor = schedule.ops[producer_index].plan_node;
            let reasons = match &node.kind {
                HNodeKind::Value => vec![BlockerReasonV1::ValueDependency],
                HNodeKind::Completion { .. } => {
                    let mut reasons = Vec::new();
                    if sequences.contains(&(predecessor, op.plan_node)) {
                        reasons.push(BlockerReasonV1::SourceCompletion);
                    }
                    let (predecessor_reads, _) = graph
                        .effect_summary(predecessor)
                        .expect("validated blocker producer has effects")
                        .scheduling_accesses();
                    let (_, successor_writes) = graph
                        .effect_summary(op.plan_node)
                        .expect("validated blocker consumer has effects")
                        .scheduling_accesses();
                    reasons.extend(
                        predecessor_reads
                            .intersection(&successor_writes)
                            .cloned()
                            .map(BlockerReasonV1::ReaderDrain),
                    );
                    reasons
                }
                HNodeKind::ResourceState {
                    resource: ResourceKey::ActorState(actor),
                    ..
                } => vec![BlockerReasonV1::ActorVersion(actor.to_string())],
                HNodeKind::ResourceState { resource, .. } => {
                    vec![BlockerReasonV1::ResourceVersion(resource.clone())]
                }
                HNodeKind::BranchControl { label, .. } => {
                    vec![BlockerReasonV1::BranchControl(label.clone())]
                }
                HNodeKind::AdmissionEvidence { .. } => continue,
            };
            blockers.entry(predecessor).or_default().extend(reasons);
        }
        operations.push(AdmittedOperationV1 {
            plan_node: op.plan_node,
            ordinal: op.ordinal,
            evidence: (*evidence
                .get(&op.plan_node)
                .with_context(|| format!("missing evidence for operation {}", op.plan_node.0))?)
            .clone(),
            blockers: blockers
                .into_iter()
                .map(|(predecessor, reasons)| OperationBlockerV1 {
                    predecessor,
                    reasons: reasons.into_iter().collect(),
                })
                .collect(),
        });
    }
    Ok(operations)
}

fn explain_sequences(plan: &ExecutionPlan, graph: &HGraph) -> Vec<RetainedSequenceV1> {
    let mut sequences = graph
        .sequence_dependencies
        .iter()
        .map(|dependency| {
            let left = graph
                .effect_summary(dependency.predecessor)
                .expect("validated sequence predecessor has effects");
            let right = graph
                .effect_summary(dependency.successor)
                .expect("validated sequence successor has effects");
            RetainedSequenceV1 {
                predecessor: dependency.predecessor,
                successor: dependency.successor,
                completion: dependency.completion,
                reason: sequence_reason(
                    plan,
                    dependency.predecessor,
                    dependency.successor,
                    left,
                    right,
                ),
            }
        })
        .collect::<Vec<_>>();
    sequences.sort_by_key(|sequence| (sequence.predecessor.0, sequence.successor.0));
    sequences
}

fn sequence_reason(
    plan: &ExecutionPlan,
    predecessor: PlanNodeId,
    successor: PlanNodeId,
    left: &EffectSummary,
    right: &EffectSummary,
) -> SequenceRetentionReasonV1 {
    if inside_left_to_right_region(plan, predecessor)
        || inside_left_to_right_region(plan, successor)
    {
        return SequenceRetentionReasonV1::LeftToRightRegion;
    }
    if left.actor_state.is_some() && left.actor_state == right.actor_state {
        return SequenceRetentionReasonV1::ActorIdentity;
    }
    if left.conflicts_with(right) {
        return SequenceRetentionReasonV1::ResourceConflict;
    }
    if left.unknown
        || right.unknown
        || !left.writes.is_empty()
        || !right.writes.is_empty()
        || !matches!(left.fallibility, crate::effects::Fallibility::Infallible)
        || !matches!(right.fallibility, crate::effects::Fallibility::Infallible)
    {
        return SequenceRetentionReasonV1::StrictFailStopUnproven;
    }
    SequenceRetentionReasonV1::ConservativeSourceSequence
}

fn inside_left_to_right_region(plan: &ExecutionPlan, node: PlanNodeId) -> bool {
    plan.edges
        .iter()
        .filter(|edge| edge.kind == PlanEdgeKind::Structural && edge.from == node)
        .map(|edge| &plan.nodes[edge.to.0].kind)
        .any(|kind| {
            matches!(
                kind,
                PlanNodeKind::Exec { backend, .. }
                    if backend.execution == crate::ir::ExecutionMode::InlineAst
                        && backend.canonical == "O"
            )
        })
}

fn policy_name(policy: Policy) -> &'static str {
    match policy {
        Policy::Eager => "eager",
        Policy::Lazy => "lazy",
        Policy::Autonomous => "autonomous",
    }
}

fn resources_text(resources: &[ResourceKey]) -> String {
    resources
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn optional_number<T: ToString>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{
        analyze_execution, runtime_binding_from_adapter_bytes, CostEstimateV1, EvidenceProvenance,
    };
    use crate::hgraph::from_oir::build_program;
    use crate::hgraph::solve::solve_types;
    use crate::hgraph::{DomainFlags, ValueState};
    use crate::ir::{InvokeMode, OIr, OIrProgram, PlanNodeKind};
    use crate::value::GroupMode;

    fn reader_writer_program(initial: &str) -> OIrProgram {
        OIrProgram {
            nodes: vec![
                OIr::Store {
                    name: "shared".into(),
                    expr: Box::new(OIr::Text(initial.into())),
                },
                OIr::Load("shared".into()),
                OIr::Load("shared".into()),
                OIr::Store {
                    name: "shared".into(),
                    expr: Box::new(OIr::Text("updated".into())),
                },
            ],
        }
    }

    fn solved_graph(program: &OIrProgram) -> HGraph {
        let mut graph = build_program(program);
        solve_types(&mut graph).expect("fixture HGraph must solve");
        graph
    }

    fn inspection_runtime(plan: &ExecutionPlan, label: &str) -> RuntimeBindingV1 {
        runtime_binding_from_adapter_bytes(plan, &[], &[("evidence-test", label)])
    }

    type LegalProjection = (
        Vec<Vec<PlanNodeId>>,
        Vec<(PlanNodeId, Vec<OperationBlockerV1>)>,
    );

    fn legal_projection(admission: &ExecutionAdmissionV3) -> LegalProjection {
        (
            admission.waves().to_vec(),
            admission
                .operations()
                .iter()
                .map(|operation| (operation.plan_node, operation.blockers.clone()))
                .collect(),
        )
    }

    #[test]
    fn identical_inputs_produce_deterministic_bindings_digests_and_explanations() {
        let program = reader_writer_program("initial");
        let plan = program.plan();
        let graph_a = solved_graph(&program);
        let graph_b = solved_graph(&program);
        let runtime_a = inspection_runtime(&plan, "deterministic");
        let runtime_b = inspection_runtime(&plan, "deterministic");
        assert_eq!(runtime_a, runtime_b);

        let evidence_a = analyze_execution(&program, &plan, &graph_a, runtime_a.clone()).unwrap();
        let evidence_b = analyze_execution(&program, &plan, &graph_b, runtime_b.clone()).unwrap();
        assert_eq!(evidence_a.bindings(), evidence_b.bindings());
        assert_eq!(
            evidence_bundle_sha256(&evidence_a),
            evidence_bundle_sha256(&evidence_b)
        );

        let admitted_a = admit_execution(
            &program,
            &plan,
            graph_a,
            Policy::Eager,
            runtime_a,
            evidence_a,
        )
        .unwrap();
        let admitted_b = admit_execution(
            &program,
            &plan,
            graph_b,
            Policy::Eager,
            runtime_b,
            evidence_b,
        )
        .unwrap();

        assert_eq!(
            admitted_a.admission().admission_sha256(),
            admitted_b.admission().admission_sha256()
        );
        assert_eq!(
            admitted_a.admission().to_explanation_text(),
            admitted_b.admission().to_explanation_text()
        );
        let explanation = admitted_a.admission().to_explanation_text();
        assert!(explanation.contains("binding lowered-oir-sha256="));
        assert!(explanation
            .contains("runtime-snapshot kind=inspection dispatch-context=inspection-only"));
        assert!(explanation.contains("effects provenance="));
        assert!(explanation.contains("adapter="));
        assert!(explanation.contains("semantics=strict-equivalent"));
        assert!(explanation.contains("evidence-inputs=["));
        assert!(explanation.contains("blockers=["));
        assert!(explanation.contains("wave 0 ["));
        assert!(explanation.contains("legal static frontier"));
        assert!(explanation.contains("dispatch adapter IDs are evidence-bound"));
        assert!(explanation.contains("fixed-size per-run pool with per-completion wakeups"));
        assert!(explanation
            .contains("verified-pure infallible local-worker outputs may provisionally unlock"));
    }

    #[test]
    fn analyzer_rejects_an_unsolved_caller_graph() {
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "html".into(),
                env_id: u32::MAX,
                attr: None,
                backend: crate::ir::BackendRegistry::global().interface_for("html"),
                body: vec![OIr::Text("<strong>fixed point</strong>".into())],
            }],
        };
        let plan = program.plan();
        let graph = build_program(&program);
        let canonical_solved = solved_graph(&program);
        assert_ne!(
            graph_sha256(&graph),
            graph_sha256(&canonical_solved),
            "fixture must actually distinguish an unsolved graph from its canonical fixed point"
        );
        let runtime = inspection_runtime(&plan, "unsolved");

        let error = analyze_execution(&program, &plan, &graph, runtime)
            .expect_err("analysis must own the canonical type-solve boundary");
        assert!(
            error.to_string().contains("exact canonical solved HGraph"),
            "{error:#}"
        );
    }

    #[test]
    fn analyzer_rejects_forged_backend_metadata_before_issuing_evidence() {
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "python".into(),
                env_id: u32::MAX,
                attr: None,
                backend: crate::ir::BackendRegistry::global().interface_for("text"),
                body: vec![OIr::Text("not python".into())],
            }],
        };
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "forged-backend-metadata");

        let error = analyze_execution(&program, &plan, &graph, runtime)
            .expect_err("analysis must reject an interface forged for another language");

        assert!(
            format!("{error:#}")
                .contains("does not match the registered execution and authority policy"),
            "{error:#}"
        );
    }

    #[test]
    fn analyzer_rejects_forged_invocation_metadata_before_issuing_evidence() {
        for (label, node) in [
            (
                "unknown-group-name",
                OIr::Invoke {
                    fn_name: "not-a-group".into(),
                    mode: InvokeMode::Group(GroupMode::Batch),
                    args: vec![OIr::Text("member".into())],
                },
            ),
            (
                "wrong-group-mode",
                OIr::Invoke {
                    fn_name: "batch".into(),
                    mode: InvokeMode::Group(GroupMode::Race),
                    args: vec![OIr::Text("member".into())],
                },
            ),
            (
                "reserved-name-marked-eager",
                OIr::Invoke {
                    fn_name: "lazy".into(),
                    mode: InvokeMode::Eager,
                    args: vec![OIr::Text("member".into())],
                },
            ),
        ] {
            let program = OIrProgram { nodes: vec![node] };
            let plan = program.plan();
            let graph = solved_graph(&program);
            let runtime = inspection_runtime(&plan, label);

            let error = analyze_execution(&program, &plan, &graph, runtime)
                .expect_err("analysis must reject a forged invocation name/mode pair");
            assert!(
                format!("{error:#}").contains("canonical lowering requires"),
                "{label}: {error:#}"
            );
        }
    }

    #[test]
    fn analyzer_rejects_a_noncanonical_dependency_plan() {
        let program = reader_writer_program("initial");
        let mut altered_plan = program.plan();
        altered_plan
            .edges
            .pop()
            .expect("fixture has a dependency edge to remove");
        altered_plan
            .validate(program.nodes.len())
            .expect("the forged plan remains locally well formed");
        let mut graph = program
            .hgraph_for_plan(&altered_plan)
            .expect("analysis-only projection accepts the alternate plan");
        solve_types(&mut graph).unwrap();
        let runtime = inspection_runtime(&altered_plan, "altered-plan");

        let error = analyze_execution(&program, &altered_plan, &graph, runtime)
            .expect_err("admission analysis must reject noncanonical dependencies");
        assert!(
            error.to_string().contains("canonical ExecutionPlan"),
            "{error:#}"
        );
    }

    #[test]
    fn admission_rejects_stale_oir_solved_graph_and_runtime_bindings() {
        let program = reader_writer_program("initial");
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "original-runtime");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();

        let stale_program = reader_writer_program("different-source-bytes");
        let stale_plan = stale_program.plan();
        assert_eq!(plan, stale_plan, "fixture must retain the same plan shape");
        let error = admit_execution(
            &stale_program,
            &stale_plan,
            solved_graph(&program),
            Policy::Eager,
            runtime.clone(),
            evidence.clone(),
        )
        .err()
        .expect("changed lowered OIR must invalidate admission");
        assert!(
            format!("{error:#}").contains("does not match HGraph source provenance"),
            "{error:#}"
        );

        let mut stale_graph = solved_graph(&program);
        let output = stale_graph
            .exec_ops_ordered()
            .first()
            .expect("fixture has an executable operation")
            .value_output;
        let original_domain = stale_graph.node(output).unwrap().domain;
        let stale_domain = if original_domain == DomainFlags::BOOL {
            DomainFlags::STRING
        } else {
            DomainFlags::BOOL
        };
        assert_ne!(original_domain, stale_domain);
        stale_graph.node_mut(output).unwrap().domain = stale_domain;
        let error = admit_execution(
            &program,
            &plan,
            stale_graph,
            Policy::Eager,
            runtime.clone(),
            evidence.clone(),
        )
        .err()
        .expect("changed solved facts must invalidate admission");
        assert!(
            format!("{error:#}").contains("evidence digest binding mismatch"),
            "{error:#}"
        );

        let stale_runtime = inspection_runtime(&plan, "different-runtime");
        let error = admit_execution(
            &program,
            &plan,
            solved_graph(&program),
            Policy::Eager,
            stale_runtime,
            evidence,
        )
        .err()
        .expect("changed runtime snapshot must invalidate admission");
        assert!(
            error
                .to_string()
                .contains("evidence runtime binding is stale"),
            "{error:#}"
        );

        let admitted_graph = solved_graph(&program);
        let admitted_runtime = inspection_runtime(&plan, "post-admission-runtime");
        let admitted_evidence =
            analyze_execution(&program, &plan, &admitted_graph, admitted_runtime.clone()).unwrap();
        let admitted = admit_execution(
            &program,
            &plan,
            admitted_graph,
            Policy::Eager,
            admitted_runtime,
            admitted_evidence,
        )
        .unwrap();
        let changed_runtime = inspection_runtime(&plan, "post-admission-runtime-changed");
        let error = admitted
            .verify_runtime(&changed_runtime)
            .expect_err("a frozen admission must reject runtime drift before execution");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("runtime binding is stale"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("environment digest"), "{diagnostic}");
        assert!(diagnostic.contains("ambient World digest"), "{diagnostic}");
    }

    #[test]
    fn admission_rejects_dispatch_adapter_substitution() {
        let program = reader_writer_program("adapter-substitution");
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "adapter-substitution");
        let mut evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let load = evidence
            .nodes
            .iter_mut()
            .find(|node| node.dispatch_contract.adapter == DispatchAdapterV1::OScopeLoadV1)
            .expect("fixture must contain one admitted scope-load adapter");
        load.dispatch_contract.adapter = DispatchAdapterV1::TrustedInlineRendererV1;

        let error = admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence)
            .err()
            .expect("changing a hard preparation adapter must invalidate admission");

        assert!(
            error
                .to_string()
                .contains("hard evidence differs from the trusted analyzer result"),
            "{error:#}"
        );
    }

    #[test]
    fn autonomous_hosted_dispatch_is_explicitly_non_strict_and_evidence_bound() {
        let python = |value| OIr::Exec {
            lang: "python".into(),
            env_id: u32::MAX,
            attr: None,
            backend: crate::ir::BackendRegistry::global().interface_for("python"),
            body: vec![OIr::Text(format!("__oval_result__ = {value}"))],
        };
        let program = OIrProgram {
            nodes: vec![OIr::Invoke {
                fn_name: "autonomous".into(),
                mode: InvokeMode::Autonomous,
                args: vec![OIr::Invoke {
                    fn_name: "batch".into(),
                    mode: InvokeMode::Group(GroupMode::Batch),
                    args: vec![python(1), python(2)],
                }],
            }],
        };
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "autonomous-hosted-semantics");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let hosted = evidence
            .nodes()
            .iter()
            .filter(|node| {
                node.dispatch_contract.adapter == DispatchAdapterV1::AutonomousEphemeralShimV1
            })
            .collect::<Vec<_>>();
        assert_eq!(hosted.len(), 2);
        assert!(hosted.iter().all(|node| {
            node.dispatch_contract.semantics == DispatchSemanticsV1::ExplicitAutonomousUnordered
                && node.failure_contract.class
                    == crate::evidence::FailureClassV1::MayFailUnorderedExternalEffects
                && !node.effect_contract.footprint_closed
        }));

        let explanation = admit_execution(
            &program,
            &plan,
            graph,
            Policy::Eager,
            runtime.clone(),
            evidence.clone(),
        )
        .unwrap()
        .admission()
        .to_explanation_text();
        assert!(explanation.contains("adapter=autonomous-ephemeral-shim/v1"));
        assert!(explanation.contains("semantics=explicit-autonomous-unordered"));

        let mut forged = evidence;
        forged
            .nodes
            .iter_mut()
            .find(|node| {
                node.dispatch_contract.adapter == DispatchAdapterV1::AutonomousEphemeralShimV1
            })
            .unwrap()
            .dispatch_contract
            .semantics = DispatchSemanticsV1::StrictEquivalent;
        let error = admit_execution(
            &program,
            &plan,
            solved_graph(&program),
            Policy::Eager,
            runtime,
            forged,
        )
        .err()
        .expect("dispatch semantics are hard evidence and cannot be substituted");
        assert!(
            error
                .to_string()
                .contains("hard evidence differs from the trusted analyzer result"),
            "{error:#}"
        );
    }

    #[test]
    fn runtime_staleness_diagnostic_names_the_exact_changed_component() {
        let program = reader_writer_program("runtime-diagnostic");
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "runtime-diagnostic");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

        let mut changed_runtime = admitted.runtime.clone();
        changed_runtime.backend_set_sha256 = "changed-backend-set".to_string();
        let error = admitted
            .verify_runtime(&changed_runtime)
            .expect_err("the changed backend-set digest must invalidate admission");

        assert_eq!(
            error.to_string(),
            "execution admission runtime binding is stale; changed components: backend-set digest"
        );
    }

    #[test]
    fn admitted_graph_has_every_materialized_evidence_input_per_operation() {
        let program = reader_writer_program("initial");
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "evidence-inputs");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

        admitted
            .graph()
            .validate_admitted_execution_graph()
            .expect("admission compiler must produce a valid authority graph");
        for operation in admitted.graph().exec_ops_ordered() {
            let fact_inputs = operation
                .inputs
                .iter()
                .filter_map(
                    |input| match admitted.graph().node(*input).map(|node| &node.kind) {
                        Some(HNodeKind::AdmissionEvidence { plan_node, fact }) => {
                            assert_eq!(*plan_node, operation.plan_node);
                            let node = admitted.graph().node(*input).unwrap();
                            assert_eq!(node.state, ValueState::Materialized);
                            assert!(node.producer.is_none());
                            Some(*fact)
                        }
                        _ => None,
                    },
                )
                .collect::<Vec<_>>();
            assert_eq!(
                fact_inputs.len(),
                AdmissionFactKind::ALL.len(),
                "operation {} must consume exactly seven evidence nodes",
                operation.plan_node.0
            );
            let facts = fact_inputs.into_iter().collect::<BTreeSet<_>>();
            assert_eq!(
                facts,
                AdmissionFactKind::ALL.into_iter().collect(),
                "operation {} must consume exactly the seven admission facts",
                operation.plan_node.0
            );
        }
    }

    #[test]
    fn writer_explanation_keeps_both_reader_drains_and_overlapping_source_completion() {
        let program = reader_writer_program("initial");
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "reader-drain-explanation");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

        let loads = plan
            .nodes
            .iter()
            .filter_map(|node| matches!(node.kind, PlanNodeKind::Load { .. }).then_some(node.id))
            .collect::<Vec<_>>();
        let stores = plan
            .nodes
            .iter()
            .filter_map(|node| matches!(node.kind, PlanNodeKind::Store { .. }).then_some(node.id))
            .collect::<Vec<_>>();
        assert_eq!((loads.len(), stores.len()), (2, 2));
        let writer = admitted
            .admission()
            .operations()
            .iter()
            .find(|operation| operation.plan_node == stores[1])
            .expect("second store must be an admitted writer");
        let resource = ResourceKey::ScopeBinding("shared".into());

        for reader in &loads {
            let blocker = writer
                .blockers
                .iter()
                .find(|blocker| blocker.predecessor == *reader)
                .unwrap_or_else(|| panic!("writer omitted reader P{} blocker", reader.0));
            assert!(
                blocker
                    .reasons
                    .contains(&BlockerReasonV1::ReaderDrain(resource.clone())),
                "writer must explain P{} as a drain of {resource}: {:?}",
                reader.0,
                blocker.reasons
            );
        }

        let immediate_reader = writer
            .blockers
            .iter()
            .find(|blocker| blocker.predecessor == loads[1])
            .expect("immediately preceding reader must block the writer");
        assert!(
            immediate_reader
                .reasons
                .contains(&BlockerReasonV1::SourceCompletion),
            "one completion token serves both source-order and reader-drain authority"
        );
        let explanation = admitted.admission().to_explanation_text();
        assert!(
            explanation.contains(&format!(
                "P{}:source-completion+reader-drain:scope:shared",
                loads[1].0
            )),
            "{explanation}"
        );
    }

    #[test]
    fn soft_cost_estimates_cannot_change_legal_blockers_or_waves() {
        let program = reader_writer_program("initial");
        let plan = program.plan();
        let baseline_graph = solved_graph(&program);
        let measured_graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "soft-cost");
        let evidence =
            analyze_execution(&program, &plan, &baseline_graph, runtime.clone()).unwrap();
        let mut measured = evidence.clone();
        for (index, node) in measured.nodes.iter_mut().enumerate() {
            node.cost_estimate = CostEstimateV1 {
                expected_duration_micros: Some((index as u64 + 1) * 10_000),
                confidence_parts_per_million: Some(900_000),
                provenance: EvidenceProvenance::HistoricalObservation,
            };
        }

        let baseline = admit_execution(
            &program,
            &plan,
            baseline_graph,
            Policy::Eager,
            runtime.clone(),
            evidence,
        )
        .unwrap();
        let cost_ranked = admit_execution(
            &program,
            &plan,
            measured_graph,
            Policy::Eager,
            runtime,
            measured,
        )
        .unwrap();

        assert_eq!(
            legal_projection(baseline.admission()),
            legal_projection(cost_ranked.admission()),
            "soft measurements may describe or rank legal work, never alter legality"
        );
        assert_ne!(
            baseline.admission().evidence_sha256(),
            cost_ranked.admission().evidence_sha256(),
            "the changed measurement remains auditable even though topology is fixed"
        );

        let load_nodes = plan
            .nodes
            .iter()
            .filter_map(|node| matches!(node.kind, PlanNodeKind::Load { .. }).then_some(node.id))
            .collect::<Vec<_>>();
        assert_eq!(load_nodes.len(), 2);
        assert!(baseline
            .admission()
            .waves()
            .iter()
            .any(|wave| load_nodes.iter().all(|load| wave.contains(load))));
    }
}
