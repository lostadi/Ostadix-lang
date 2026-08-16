use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::effects::{EffectSummary, ResourceKey};
use crate::eval::Policy;
use crate::hgraph::{AdmissionFactKind, EdgeId, HGraph, HNodeKind, NodeId, ReadySchedule};
use crate::ir::{ExecutionPlan, OIrProgram, PlanEdgeKind, PlanNodeId, PlanNodeKind};
use crate::placement::SemanticDigestV1;
use crate::runtime_exec::{ExecutableLeaseSet, ExecutableManifestV1};

use super::analyze::{
    analyze_execution, digest_fields, evidence_bindings, evidence_bundle_sha256, graph_sha256,
    oir_sha256,
};
use super::fact::{
    BackendArtifactV1, DispatchAdapterV1, DispatchLaneV1, DispatchSemanticsV1, EvidenceBindingsV2,
    EvidenceBundleV5, NodeEvidence, PlacementContractV1, RuntimeBindingV1, RuntimeSnapshotKindV1,
    ADMISSION_SCHEMA_V5, ANALYZER_ID_V5, EVIDENCE_SCHEMA_V5,
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

pub const SCHEDULE_WHY_SCHEMA_V1: &str = "oexec.admission-why/v1";
pub const SCHEDULE_EXPLANATION_SCHEMA_V1: &str = "oexec.schedule-explanation/v1";
pub const SCHEDULE_REALIZABILITY_SCHEMA_V1: &str = "oexec.realizability/v1";
pub const SCHEDULE_PREDICTION_SCHEMA_V1: &str = "oexec.schedule-prediction/v1";
pub const PLACEMENT_ADMISSION_DIGEST_DOMAIN_V1: &str = "ostadix/placement-admission/v1";

/// Digest coordinates identifying the exact admitted computation rendered by
/// a schedule explanation. These are copied from [`ExecutionAdmissionV5`]; the
/// explanation is inspection-only and carries no execution authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleExplanationBindingsV1 {
    pub lowered_oir_sha256: String,
    pub plan_sha256: String,
    pub analyzed_graph_sha256: String,
    pub backend_catalog_projection_sha256: String,
    pub backend_set_sha256: String,
    pub direct_executable_manifest_sha256: String,
    pub launch_context_sha256: String,
    pub environment_sha256: String,
    pub ambient_world_sha256: String,
    pub analyzer_sha256: String,
    pub evidence_sha256: String,
    pub admitted_graph_sha256: String,
    pub placement_admission_sha256: String,
    pub admission_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleExplanationAdmissionV1 {
    pub schema: &'static str,
    pub analyzer: &'static str,
    pub runtime_snapshot_kind: &'static str,
    pub base_policy: &'static str,
    pub bindings: ScheduleExplanationBindingsV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleRealizabilityV1 {
    pub schema: &'static str,
    pub status: &'static str,
    pub execution_realizable: &'static str,
    pub dispatch: &'static str,
    pub scope: &'static str,
    pub worker_count_covers_static_wave: &'static str,
    pub runtime_readiness: &'static str,
    pub placement_lease: &'static str,
    pub observed_overlap: &'static str,
    pub source: &'static str,
    pub available_parallelism: usize,
    pub admitted_static_max_wave_width: usize,
    pub admitted_max_local_worker_wave_width: usize,
    pub selected_workers: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchedulePredictionLayerV1 {
    pub index: usize,
    pub operations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchedulePredictionV1 {
    pub schema: &'static str,
    pub status: &'static str,
    pub provenance: &'static str,
    pub model: &'static str,
    pub admission_sha256: String,
    pub task_count: usize,
    pub predicted_width: usize,
    pub predicted_span: usize,
    pub span_unit: &'static str,
    pub layers: Vec<SchedulePredictionLayerV1>,
}

/// Stable machine projection used by `olangc --explain-schedule --format
/// json`. Human and JSON renderers derive their realizability and prediction
/// values from this same typed view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleExplanationV1 {
    pub schema: &'static str,
    pub admission: ScheduleExplanationAdmissionV1,
    pub realizability: ScheduleRealizabilityV1,
    pub prediction: SchedulePredictionV1,
}

/// One exact HGraph input/producer correspondence behind a blocker.
///
/// Unlike the compact predecessor aggregation in [`OperationBlockerV1`], a
/// witness retains the actual value/control/resource node consumed by the
/// selected operation.  It is a read-only projection of the admitted graph;
/// it does not participate in admission hashing or dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleWhyWitnessV1 {
    pub predecessor: PlanNodeId,
    pub input: NodeId,
    pub producer_edge: EdgeId,
    pub input_kind: HNodeKind,
    pub reasons: Vec<BlockerReasonV1>,
}

/// A later admitted operation whose readiness directly depends on the target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleWhyDependentV1 {
    pub operation: PlanNodeId,
    pub witnesses: Vec<ScheduleWhyWitnessV1>,
}

/// Smallest currently implemented admission projection that explains one
/// canonical plan operation and its immediate scheduling neighborhood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleWhyViewV1 {
    pub schema: &'static str,
    pub bindings: EvidenceBindingsV2,
    pub evidence_sha256: String,
    pub admitted_graph_sha256: String,
    pub admission_sha256: String,
    pub plan_kind: PlanNodeKind,
    pub operation: AdmittedOperationV1,
    pub blocker_witnesses: Vec<ScheduleWhyWitnessV1>,
    pub dependents: Vec<ScheduleWhyDependentV1>,
    pub retained_sequences: Vec<RetainedSequenceV1>,
    pub wave_index: usize,
    pub wave: Vec<PlanNodeId>,
    pub hosted_task_layer: Option<(usize, Vec<PlanNodeId>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionAdmissionV5 {
    schema: &'static str,
    bindings: EvidenceBindingsV2,
    analyzer: &'static str,
    runtime_snapshot_kind: RuntimeSnapshotKindV1,
    backend_artifacts: Vec<BackendArtifactV1>,
    executable_manifest: ExecutableManifestV1,
    evidence_sha256: String,
    admitted_graph_sha256: String,
    placement_admission: SemanticDigestV1,
    admission_sha256: String,
    base_policy: Policy,
    operations: Vec<AdmittedOperationV1>,
    retained_sequences: Vec<RetainedSequenceV1>,
    waves: Vec<Vec<PlanNodeId>>,
    hosted_task_layers: Vec<Vec<PlanNodeId>>,
}

impl ExecutionAdmissionV5 {
    pub fn schema(&self) -> &'static str {
        self.schema
    }

    pub fn bindings(&self) -> &EvidenceBindingsV2 {
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

    pub fn executable_manifest(&self) -> &ExecutableManifestV1 {
        &self.executable_manifest
    }

    pub fn evidence_sha256(&self) -> &str {
        &self.evidence_sha256
    }

    pub fn admitted_graph_sha256(&self) -> &str {
        &self.admitted_graph_sha256
    }

    /// Process-portable semantic admission used by placement authority.
    ///
    /// This digest deliberately excludes the process-local runtime snapshot,
    /// executable manifest, environment, ambient world, and launch context
    /// retained by [`Self::admission_sha256`]. Source bytes, task identity,
    /// backend realization, environment, sandbox, and physical generation
    /// remain separately bound placement coordinates.
    pub fn placement_admission(&self) -> &SemanticDigestV1 {
        &self.placement_admission
    }

    /// Full process-local admission digest used for runtime freshness.
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

    pub fn retained_sequences(&self) -> &[RetainedSequenceV1] {
        &self.retained_sequences
    }

    pub fn admitted_static_max_wave_width(&self) -> usize {
        self.waves.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Widest unit-cost hosted-task layer in the admitted dependency graph.
    /// Coordinator bookkeeping has zero weight; every shim-backed execution
    /// operation has unit weight. This is a topology prediction, not a claim
    /// that the whole layer can be dispatched together on this machine.
    pub fn admitted_hosted_task_max_wave_width(&self) -> usize {
        self.hosted_task_layers
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
    }

    /// Longest admitted dependency path measured in unit-cost shim-backed
    /// hosted operations. Zero-cost coordinator bookkeeping does not inflate
    /// the result, so the unit is hosted-task layers.
    pub fn admitted_hosted_task_wave_count(&self) -> usize {
        self.hosted_task_layers.len()
    }

    pub fn admitted_hosted_task_layers(&self) -> &[Vec<PlanNodeId>] {
        &self.hosted_task_layers
    }

    /// Widest legal static frontier among operations admitted to the local
    /// worker lane. Coordinator-owned operations do not consume worker slots
    /// and therefore do not contribute to this bound.
    pub fn admitted_max_wave_width(&self) -> usize {
        let local_workers = self
            .operations
            .iter()
            .filter_map(|operation| {
                (operation.evidence.dispatch_contract.lane == DispatchLaneV1::LocalWorker)
                    .then_some(operation.plan_node)
            })
            .collect::<BTreeSet<_>>();
        self.waves
            .iter()
            .map(|wave| {
                wave.iter()
                    .filter(|plan_node| local_workers.contains(plan_node))
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    /// Resolve the local-worker count after admission has established the
    /// widest legal worker frontier. A CLI override is authoritative execution
    /// policy; otherwise the machine and admitted graph jointly set the bound.
    pub fn resolved_worker_count(&self, cli_override: Option<usize>) -> usize {
        resolve_worker_count(
            cli_override,
            available_parallelism(),
            self.admitted_max_wave_width(),
        )
    }

    /// Build the stable inspection-only projection shared by human and JSON
    /// schedule renderers.
    pub fn schedule_explanation_with_worker_override(
        &self,
        cli_override: Option<usize>,
    ) -> ScheduleExplanationV1 {
        let available = available_parallelism();
        let admitted_max_local_worker_wave_width = self.admitted_max_wave_width();
        let selected_workers = resolve_worker_count(
            cli_override,
            available,
            admitted_max_local_worker_wave_width,
        );
        let worker_count_covers_static_wave = if admitted_max_local_worker_wave_width == 0 {
            "not-applicable"
        } else if selected_workers >= admitted_max_local_worker_wave_width {
            "yes"
        } else {
            "no"
        };
        ScheduleExplanationV1 {
            schema: SCHEDULE_EXPLANATION_SCHEMA_V1,
            admission: ScheduleExplanationAdmissionV1 {
                schema: self.schema,
                analyzer: self.analyzer,
                runtime_snapshot_kind: self.runtime_snapshot_kind.name(),
                base_policy: policy_name(self.base_policy),
                bindings: ScheduleExplanationBindingsV1 {
                    lowered_oir_sha256: self.bindings.oir_sha256.clone(),
                    plan_sha256: self.bindings.plan_sha256.clone(),
                    analyzed_graph_sha256: self.bindings.analyzed_graph_sha256.clone(),
                    backend_catalog_projection_sha256: self
                        .bindings
                        .backend_catalog_projection_sha256
                        .clone(),
                    backend_set_sha256: self.bindings.backend_set_sha256.clone(),
                    direct_executable_manifest_sha256: self
                        .bindings
                        .executable_manifest_sha256
                        .clone(),
                    launch_context_sha256: self.bindings.launch_context_sha256.clone(),
                    environment_sha256: self.bindings.environment_sha256.clone(),
                    ambient_world_sha256: self.bindings.ambient_world_sha256.clone(),
                    analyzer_sha256: self.bindings.analyzer_sha256.clone(),
                    evidence_sha256: self.evidence_sha256.clone(),
                    admitted_graph_sha256: self.admitted_graph_sha256.clone(),
                    placement_admission_sha256: self.placement_admission.as_sha256().to_string(),
                    admission_sha256: self.admission_sha256.clone(),
                },
            },
            realizability: ScheduleRealizabilityV1 {
                schema: SCHEDULE_REALIZABILITY_SCHEMA_V1,
                status: "inspection-only",
                execution_realizable: "unknown",
                dispatch: "not-run",
                scope: "local-worker-static-wave",
                worker_count_covers_static_wave,
                runtime_readiness: "unknown",
                placement_lease: "none",
                observed_overlap: "not-run",
                source: if cli_override.is_some() {
                    "cli-override"
                } else {
                    "machine-default"
                },
                available_parallelism: available,
                admitted_static_max_wave_width: self.admitted_static_max_wave_width(),
                admitted_max_local_worker_wave_width,
                selected_workers,
            },
            prediction: SchedulePredictionV1 {
                schema: SCHEDULE_PREDICTION_SCHEMA_V1,
                status: "admitted-static",
                provenance: "evidence-bound-admission",
                model: "unit-cost-shim-hosted-tasks",
                admission_sha256: self.admission_sha256.clone(),
                task_count: self.hosted_task_layers.iter().map(Vec::len).sum(),
                predicted_width: self.admitted_hosted_task_max_wave_width(),
                predicted_span: self.admitted_hosted_task_wave_count(),
                span_unit: "hosted-task-layers",
                layers: self
                    .hosted_task_layers
                    .iter()
                    .enumerate()
                    .map(|(index, layer)| SchedulePredictionLayerV1 {
                        index: index + 1,
                        operations: layer.iter().map(|node| format!("P{}", node.0)).collect(),
                    })
                    .collect(),
            },
        }
    }

    pub fn to_explanation_json_with_worker_override(
        &self,
        cli_override: Option<usize>,
    ) -> serde_json::Result<String> {
        serde_json::to_string(&self.schedule_explanation_with_worker_override(cli_override))
    }

    /// Non-executing explanation of the exact admitted scheduling geometry.
    /// The evidence-bound admission text is stable, while the explicitly
    /// advisory realizability marker samples the inspection host's current
    /// parallelism. Waves describe legal readiness, not observed overlap.
    pub fn to_explanation_text(&self) -> String {
        self.to_explanation_text_with_worker_override(None)
    }

    /// Render the admission together with a descriptive worker-capacity
    /// realizability marker. The marker is not part of the admission digest.
    pub fn to_explanation_text_with_worker_override(&self, cli_override: Option<usize>) -> String {
        let explanation = self.schedule_explanation_with_worker_override(cli_override);
        let admission = &explanation.admission;
        let bindings = &admission.bindings;
        let mut out = format!("; ExecutionAdmission {}\n", admission.schema);
        writeln!(
            out,
            "binding lowered-oir-sha256={} plan-sha256={} analyzed-graph-sha256={}",
            bindings.lowered_oir_sha256, bindings.plan_sha256, bindings.analyzed_graph_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(
            out,
            "binding backend-catalog-projection-sha256={} backend-set-sha256={} direct-executable-manifest-sha256={} launch-context-sha256={} environment-sha256={} ambient-world-sha256={}",
            bindings.backend_catalog_projection_sha256,
            bindings.backend_set_sha256,
            bindings.direct_executable_manifest_sha256,
            bindings.launch_context_sha256,
            bindings.environment_sha256,
            bindings.ambient_world_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(
            out,
            "binding analyzer-sha256={} evidence-sha256={} admitted-graph-sha256={} placement-admission-sha256={} admission-sha256={}",
            bindings.analyzer_sha256,
            bindings.evidence_sha256,
            bindings.admitted_graph_sha256,
            bindings.placement_admission_sha256,
            bindings.admission_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(out, "analyzer {}", admission.analyzer).expect("writing to a String cannot fail");
        writeln!(
            out,
            "runtime-snapshot kind={} dispatch-context={}",
            admission.runtime_snapshot_kind,
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
        writeln!(
            out,
            "direct-executable-manifest schema={} scope={} sha256={} guarantee=direct-launcher-only transitive-runtime-closure=not-bound",
            self.executable_manifest.schema,
            self.executable_manifest.scope,
            self.executable_manifest.sha256()
        )
        .expect("writing to a String cannot fail");
        for executable in self.executable_manifest.artifacts() {
            writeln!(
                out,
                "direct-executable backend={} requirement={} selected-alternative={} selection={} command={} role={} state={} invocation-path={} invocation-identity={} canonical-target={} target-identity={} sha256={} guarantee={}",
                executable.canonical_backend,
                executable.requirement_key,
                executable
                    .selected_alternative
                    .map(|index| index.to_string())
                    .unwrap_or_else(|| "not-selected".to_string()),
                executable.selection.name(),
                executable.logical_command,
                executable.role,
                executable.state.name(),
                executable
                    .invocation_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "not-probed".to_string()),
                executable.invocation_identity,
                executable
                    .canonical_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "not-probed".to_string()),
                executable.resolved_identity,
                executable.sha256.as_deref().unwrap_or("not-probed"),
                executable.guarantee.name(),
            )
            .expect("writing to a String cannot fail");
        }
        writeln!(out, "policy {}", admission.base_policy).expect("writing to a String cannot fail");
        let realizability = &explanation.realizability;
        writeln!(out, "; ScheduleRealizability {}", realizability.schema)
            .expect("writing to a String cannot fail");
        writeln!(
            out,
            "realizability status={} execution-realizable={} dispatch={} scope={} worker-count-covers-static-wave={} runtime-readiness={} placement-lease={} observed-overlap={} source={} available-parallelism={} admitted-static-max-wave-width={} admitted-max-local-worker-wave-width={} selected-workers={}",
            realizability.status,
            realizability.execution_realizable,
            realizability.dispatch,
            realizability.scope,
            realizability.worker_count_covers_static_wave,
            realizability.runtime_readiness,
            realizability.placement_lease,
            realizability.observed_overlap,
            realizability.source,
            realizability.available_parallelism,
            realizability.admitted_static_max_wave_width,
            realizability.admitted_max_local_worker_wave_width,
            realizability.selected_workers
        )
        .expect("writing to a String cannot fail");
        let prediction = &explanation.prediction;
        writeln!(out, "; SchedulePrediction {}", prediction.schema)
            .expect("writing to a String cannot fail");
        writeln!(
            out,
            "schedule-prediction schema={} status={} provenance={} model={} admission-sha256={} task-count={} predicted-width={} predicted-span={} span-unit={}",
            prediction.schema,
            prediction.status,
            prediction.provenance,
            prediction.model,
            prediction.admission_sha256,
            prediction.task_count,
            prediction.predicted_width,
            prediction.predicted_span,
            prediction.span_unit
        )
        .expect("writing to a String cannot fail");
        for layer in &prediction.layers {
            writeln!(
                out,
                "schedule-prediction-layer index={} operations=[{}]",
                layer.index,
                layer.operations.join(",")
            )
            .expect("writing to a String cannot fail");
        }

        for operation in &self.operations {
            render_operation(&mut out, operation);
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
            "admission-note hosted-task prediction assigns unit cost to shim-backed execution and zero cost to other admitted operations; it predicts topology, not duration, capacity fit, dispatch, or observed overlap\n",
        );
        out.push_str(
            "admission-note worker-count-covers-static-wave checks local-worker count only; execution realizability remains unknown and no simultaneous dispatch, CPU or memory fit, or overlap is proved\n",
        );
        out.push_str(
            "admission-note admitted maximum local-worker wave width is a static Kahn-wave capacity heuristic, not a bound on the completion-driven dynamic frontier\n",
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
            "admission-note backend-catalog-projection-sha256 binds only canonical specifications referenced by this plan; it is not runtime discovery, health, authorization, capacity, or readiness evidence\n",
        );
        out.push_str(
            "admission-note backend artifact states distinguish hashed, missing, non-regular, and unreadable paths\n",
        );
        out.push_str(
            "admission-note V5 direct-launch leases retain opened hashed canonical targets and dispatch their admitted absolute invocation paths after immediate identity checks; this preserves multicall symlink names and prevents PATH alternative reselection but does not freeze the child environment or eliminate a final same-principal stat-to-exec micro-window\n",
        );
        out.push_str(
            "admission-note direct-launch binding excludes shebang interpreters, compiler-driver subtools, dynamic libraries, hosted descendants, Request/project authorities, and live actor state/generation\n",
        );
        out.push_str(
            "admission-note caller initial scope shape and values are installed after admission and are not digest-bound in V5\n",
        );
        out.push_str(
            "admission-note local placement is descriptive in V5 and does not assert a current lease\n",
        );
        out
    }
}

impl ScheduleWhyViewV1 {
    /// Stable, non-executing text form for the focused admission projection.
    pub fn to_text(&self) -> String {
        let mut out = format!("; ExecutionAdmissionWhy {}\n", self.schema);
        writeln!(
            out,
            "why operation=P{} status=admitted-static inspection-only=yes dispatch=not-run admission-sha256={}",
            self.operation.plan_node.0, self.admission_sha256
        )
        .expect("writing to a String cannot fail");
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
            "binding analyzer-sha256={} evidence-sha256={} admitted-graph-sha256={} admission-sha256={}",
            self.bindings.analyzer_sha256,
            self.evidence_sha256,
            self.admitted_graph_sha256,
            self.admission_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(
            out,
            "binding backend-catalog-projection-sha256={}",
            self.bindings.backend_catalog_projection_sha256
        )
        .expect("writing to a String cannot fail");
        writeln!(
            out,
            "plan-node P{} kind={}",
            self.operation.plan_node.0,
            self.plan_kind.describe()
        )
        .expect("writing to a String cannot fail");
        render_operation(&mut out, &self.operation);

        for witness in &self.blocker_witnesses {
            render_witness(&mut out, "blocker-witness", witness);
        }
        for dependent in &self.dependents {
            writeln!(out, "dependent operation=P{}", dependent.operation.0)
                .expect("writing to a String cannot fail");
            for witness in &dependent.witnesses {
                render_witness(&mut out, "dependent-witness", witness);
            }
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
        writeln!(
            out,
            "wave index={} operations=[{}]",
            self.wave_index,
            plan_nodes_text(&self.wave)
        )
        .expect("writing to a String cannot fail");
        if let Some((index, operations)) = &self.hosted_task_layer {
            writeln!(
                out,
                "hosted-task-layer index={} operations=[{}]",
                index,
                plan_nodes_text(operations)
            )
            .expect("writing to a String cannot fail");
        } else {
            out.push_str("hosted-task-layer none\n");
        }
        out.push_str(
            "why-note this view is derived from the evidence-bound admitted HGraph and does not dispatch the program\n",
        );
        out.push_str(
            "why-note blockers and waves describe admitted static readiness, not observed execution, timing, worker identity, or overlap\n",
        );
        out
    }
}

fn render_operation(out: &mut String, operation: &AdmittedOperationV1) {
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

fn render_witness(out: &mut String, label: &str, witness: &ScheduleWhyWitnessV1) {
    writeln!(
        out,
        "{label} predecessor=P{} input=N{} producer=E{} kind={} reasons=[{}]",
        witness.predecessor.0,
        witness.input.0,
        witness.producer_edge.0,
        hnode_kind_text(&witness.input_kind),
        witness
            .reasons
            .iter()
            .map(BlockerReasonV1::label)
            .collect::<Vec<_>>()
            .join(",")
    )
    .expect("writing to a String cannot fail");
}

fn hnode_kind_text(kind: &HNodeKind) -> String {
    match kind {
        HNodeKind::Value => "value".to_string(),
        HNodeKind::ResourceState { resource, version } => {
            format!("resource-state:{resource}:v{version}")
        }
        HNodeKind::Completion { plan_node } => format!("completion:P{}", plan_node.0),
        HNodeKind::BranchControl { label, version } => {
            format!("branch-control:{label}:v{version}")
        }
        HNodeKind::AdmissionEvidence { plan_node, fact } => {
            format!("admission-evidence:P{}:{}", plan_node.0, fact.name())
        }
    }
}

fn plan_nodes_text(nodes: &[PlanNodeId]) -> String {
    nodes
        .iter()
        .map(|node| format!("P{}", node.0))
        .collect::<Vec<_>>()
        .join(",")
}

fn available_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

fn resolve_worker_count(
    cli_override: Option<usize>,
    available_parallelism: usize,
    admitted_max_wave_width: usize,
) -> usize {
    cli_override
        .unwrap_or_else(|| std::cmp::min(available_parallelism, admitted_max_wave_width).max(1))
}

/// Frozen authority boundary consumed by the coordinator. Its fields and
/// constructor are private; the only route from a solved graph to execution is
/// the digest-checking admission compiler below.
pub struct AdmittedExecution<'a> {
    program: &'a OIrProgram,
    plan: &'a ExecutionPlan,
    graph: HGraph,
    runtime: RuntimeBindingV1,
    admission: ExecutionAdmissionV5,
}

/// Owned half of an admitted execution used when a caller must retain exact
/// admission authority across an external authorization round trip.  The
/// lowered program and plan deliberately live with the caller: storing their
/// references here would make the resulting object self-referential.  The
/// only reconstruction path revalidates those owned sources before yielding
/// the short-lived borrowed [`AdmittedExecution`] consumed by the runtime.
pub(crate) struct PreparedAdmissionPartsV1 {
    graph: HGraph,
    runtime: RuntimeBindingV1,
    admission: ExecutionAdmissionV5,
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

    pub fn admission(&self) -> &ExecutionAdmissionV5 {
        &self.admission
    }

    pub(crate) fn into_prepared_parts(self) -> PreparedAdmissionPartsV1 {
        PreparedAdmissionPartsV1 {
            graph: self.graph,
            runtime: self.runtime,
            admission: self.admission,
        }
    }

    /// Return the process-local executable launch authority retained by this
    /// admission. Inspection admissions intentionally have no such authority.
    pub(crate) fn executable_leases(&self) -> Result<Arc<ExecutableLeaseSet>> {
        self.runtime.executable_leases().context(
            "execution admission carries no executable leases (inspection-only runtime snapshot)",
        )
    }

    /// Conservative generation identity for a backend process launch. It
    /// binds that backend's selected direct executable set, consumed legacy
    /// shim artifact rows, and the complete admitted environment snapshot.
    /// Persistent actors use this digest to prevent reuse across any of those
    /// launch-context changes.
    pub(crate) fn backend_launch_generation_sha256(&self, backend: &str) -> Result<String> {
        let leases = self.executable_leases()?;
        let executable_set = leases.backend_executable_set_sha256(backend)?;
        let mut fields = vec![executable_set, self.runtime.launch_context_sha256()];
        for artifact in self
            .runtime
            .backend_artifacts()
            .iter()
            .filter(|artifact| artifact.canonical_backend == backend)
        {
            fields.push(artifact.resolved_identity.as_str());
            fields.push(artifact.state.name());
            fields.push(artifact.state.sha256().unwrap_or("none"));
        }
        Ok(digest_fields(
            "ostadix-backend-launch-generation/v1",
            &fields,
        ))
    }

    /// Recheck mutable legacy-shim/environment context without re-resolving or
    /// re-hashing direct executables. The retained lease set owns direct-launch
    /// freshness and is verified separately at each backend spawn.
    pub(crate) fn verify_runtime_context(
        &self,
        shim_dir: &Path,
        context: &[(&str, &str)],
    ) -> Result<()> {
        let current = super::analyze::runtime_binding_from_directory_reusing_executables(
            self.plan,
            shim_dir,
            context,
            self.runtime.executable_manifest.clone(),
        );
        self.verify_runtime(&current)
    }

    /// Project one canonical plan operation and its immediate admitted
    /// dependency neighborhood without dispatching any work.
    pub fn schedule_why(&self, target: PlanNodeId) -> Result<ScheduleWhyViewV1> {
        let Some(plan_node) = self
            .plan
            .nodes
            .get(target.0)
            .filter(|node| node.id == target)
        else {
            if self.plan.nodes.is_empty() {
                bail!("cannot explain P{}: the ExecutionPlan is empty", target.0);
            }
            bail!(
                "cannot explain P{}: the ExecutionPlan contains {} nodes (valid range P0..P{})",
                target.0,
                self.plan.nodes.len(),
                self.plan.nodes.len() - 1
            );
        };
        let operation = self
            .admission
            .operations
            .iter()
            .find(|operation| operation.plan_node == target)
            .cloned()
            .with_context(|| {
                format!(
                    "P{} exists in the ExecutionPlan as `{}` but is not an admitted executable operation",
                    target.0,
                    plan_node.kind.describe()
                )
            })?;

        let producer_by_edge = self
            .graph
            .op_map
            .values()
            .map(|operation| (operation.edge, operation.plan_node))
            .collect::<BTreeMap<_, _>>();
        let sequences = self
            .graph
            .sequence_dependencies
            .iter()
            .map(|dependency| (dependency.predecessor, dependency.successor))
            .collect::<BTreeSet<_>>();
        let blocker_witnesses =
            schedule_why_witnesses(&self.graph, target, &producer_by_edge, &sequences)?;

        let mut dependents = Vec::new();
        for candidate in self.graph.exec_ops_ordered() {
            if candidate.plan_node == target {
                continue;
            }
            let witnesses = schedule_why_witnesses(
                &self.graph,
                candidate.plan_node,
                &producer_by_edge,
                &sequences,
            )?
            .into_iter()
            .filter(|witness| witness.predecessor == target)
            .collect::<Vec<_>>();
            if !witnesses.is_empty() {
                dependents.push(ScheduleWhyDependentV1 {
                    operation: candidate.plan_node,
                    witnesses,
                });
            }
        }

        let retained_sequences = self
            .admission
            .retained_sequences
            .iter()
            .filter(|sequence| sequence.predecessor == target || sequence.successor == target)
            .cloned()
            .collect::<Vec<_>>();
        let (wave_index, wave) = self
            .admission
            .waves
            .iter()
            .enumerate()
            .find(|(_, wave)| wave.contains(&target))
            .map(|(index, wave)| (index, wave.clone()))
            .with_context(|| format!("admission static waves omit operation P{}", target.0))?;
        let hosted_task_layer = self
            .admission
            .hosted_task_layers
            .iter()
            .enumerate()
            .find(|(_, layer)| layer.contains(&target))
            .map(|(index, layer)| (index + 1, layer.clone()));

        Ok(ScheduleWhyViewV1 {
            schema: SCHEDULE_WHY_SCHEMA_V1,
            bindings: self.admission.bindings.clone(),
            evidence_sha256: self.admission.evidence_sha256.clone(),
            admitted_graph_sha256: self.admission.admitted_graph_sha256.clone(),
            admission_sha256: self.admission.admission_sha256.clone(),
            plan_kind: plan_node.kind.clone(),
            operation,
            blocker_witnesses,
            dependents,
            retained_sequences,
            wave_index,
            wave,
            hosted_task_layer,
        })
    }

    pub(crate) fn verify_runtime(&self, current: &RuntimeBindingV1) -> Result<()> {
        let mut changed = Vec::new();
        let snapshot_kind_changed = self.runtime.snapshot_kind != current.snapshot_kind;
        let backend_artifacts_changed = self.runtime.backend_artifacts != current.backend_artifacts;
        let executable_manifest_changed =
            self.runtime.executable_manifest != current.executable_manifest;
        let backend_set_changed = self.runtime.backend_set_sha256 != current.backend_set_sha256;
        let launch_context_changed =
            self.runtime.launch_context_sha256 != current.launch_context_sha256;
        let environment_changed = self.runtime.environment_sha256 != current.environment_sha256;

        if snapshot_kind_changed {
            changed.push("snapshot kind");
        }
        if backend_artifacts_changed {
            changed.push("backend artifacts");
        }
        if executable_manifest_changed {
            changed.push("direct executable manifest");
        }
        if self.runtime.backend_catalog_projection_sha256
            != current.backend_catalog_projection_sha256
        {
            changed.push("backend catalog projection digest");
        }
        // Report the earliest changed source in each deterministic digest
        // chain. A changed artifact necessarily changes backend-set,
        // environment, and ambient-World digests; repeating every downstream
        // consequence obscures the actionable freshness failure.
        if !backend_artifacts_changed && backend_set_changed {
            changed.push("backend-set digest");
        }
        if launch_context_changed {
            changed.push("backend launch context digest");
        }
        if !snapshot_kind_changed
            && !backend_set_changed
            && !launch_context_changed
            && environment_changed
        {
            changed.push("environment digest");
        }
        if !environment_changed && self.runtime.ambient_world_sha256 != current.ambient_world_sha256
        {
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

impl PreparedAdmissionPartsV1 {
    /// Reconstruct the runtime authority view without re-analysis or
    /// re-admission.  The inputs are borrowed only for the lifetime of the
    /// returned value and must reproduce the exact canonical bindings sealed
    /// at preparation time.
    pub(crate) fn bind<'a>(
        self,
        program: &'a OIrProgram,
        plan: &'a ExecutionPlan,
    ) -> Result<AdmittedExecution<'a>> {
        if plan != &program.plan() {
            bail!(
                "prepared execution requires the canonical ExecutionPlan derived from its exact lowered OIR"
            );
        }
        self.graph
            .validate_execution_source(program, plan)
            .map_err(anyhow::Error::msg)
            .context("prepared execution rejected OIR/plan/HGraph provenance")?;
        let plan_sha256 = hex::encode(Sha256::digest(plan.to_text().as_bytes()));
        if self.admission.bindings.oir_sha256 != oir_sha256(program)
            || self.admission.bindings.plan_sha256 != plan_sha256
            || self.admission.admitted_graph_sha256 != graph_sha256(&self.graph)
            || self.admission.bindings.backend_catalog_projection_sha256
                != self.runtime.backend_catalog_projection_sha256()
            || self.admission.bindings.backend_set_sha256 != self.runtime.backend_set_sha256()
            || self.admission.bindings.executable_manifest_sha256
                != self.runtime.executable_manifest().sha256()
            || self.admission.bindings.launch_context_sha256 != self.runtime.launch_context_sha256()
            || self.admission.bindings.environment_sha256 != self.runtime.environment_sha256()
            || self.admission.bindings.ambient_world_sha256 != self.runtime.ambient_world_sha256()
        {
            bail!(
                "prepared execution binding mismatch: lowered OIR, plan, graph, catalog, runtime, or environment changed"
            );
        }
        if self.admission.runtime_snapshot_kind != self.runtime.snapshot_kind()
            || self.admission.backend_artifacts != self.runtime.backend_artifacts()
            || self.admission.executable_manifest != *self.runtime.executable_manifest()
        {
            bail!("prepared execution runtime authority no longer matches its admission");
        }
        Ok(AdmittedExecution {
            program,
            plan,
            graph: self.graph,
            runtime: self.runtime,
            admission: self.admission,
        })
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
    evidence: EvidenceBundleV5,
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
    if evidence.schema != EVIDENCE_SCHEMA_V5 || evidence.analyzer != ANALYZER_ID_V5 {
        bail!("unsupported or untrusted evidence bundle schema/analyzer");
    }
    if evidence.runtime != runtime {
        bail!("evidence runtime binding is stale or belongs to another execution context");
    }
    let expected_bindings = evidence_bindings(program, plan, &graph, &runtime);
    if evidence.bindings != expected_bindings {
        bail!(
            "evidence digest binding mismatch: lowered OIR, plan, graph, backend catalog projection, backend artifacts, environment, or ambient World changed"
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
    let hosted_task_layers = explain_hosted_task_layers(plan, &schedule, &waves)?;
    let operations = explain_operations(&graph, &schedule, &by_plan)?;
    let retained_sequences = explain_sequences(plan, &graph);
    let placement_admission =
        placement_admission_digest(&expected_bindings, &admitted_graph_sha256, base_policy);
    let admission_sha256 = digest_fields(
        "ostadix-execution-admission/v5",
        &[
            &evidence_sha256,
            &admitted_graph_sha256,
            policy_name(base_policy),
        ],
    );

    let admission = ExecutionAdmissionV5 {
        schema: ADMISSION_SCHEMA_V5,
        bindings: expected_bindings,
        analyzer: ANALYZER_ID_V5,
        runtime_snapshot_kind: runtime.snapshot_kind(),
        backend_artifacts: runtime.backend_artifacts().to_vec(),
        executable_manifest: runtime.executable_manifest().clone(),
        evidence_sha256,
        admitted_graph_sha256,
        placement_admission,
        admission_sha256,
        base_policy,
        operations,
        retained_sequences,
        waves,
        hosted_task_layers,
    };
    Ok(AdmittedExecution {
        program,
        plan,
        graph,
        runtime,
        admission,
    })
}

fn placement_admission_digest(
    bindings: &EvidenceBindingsV2,
    admitted_graph_sha256: &str,
    base_policy: Policy,
) -> SemanticDigestV1 {
    let digest = digest_fields(
        PLACEMENT_ADMISSION_DIGEST_DOMAIN_V1,
        &[
            ADMISSION_SCHEMA_V5,
            ANALYZER_ID_V5,
            &bindings.oir_sha256,
            &bindings.plan_sha256,
            &bindings.analyzed_graph_sha256,
            admitted_graph_sha256,
            &bindings.backend_catalog_projection_sha256,
            policy_name(base_policy),
        ],
    );
    SemanticDigestV1::from_sha256(digest)
        .expect("canonical placement admission hashing always yields lowercase SHA-256")
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
                && crate::dispatch_model::autonomous_ephemeral_group(plan, plan_node, oir)
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

/// Project the admitted dependency DAG onto unit-cost shim-backed hosted
/// operations. Every other operation has zero weight, so structural stores,
/// groups, schedule controls, and scope loads preserve causality without
/// inflating the hosted-task span.
fn explain_hosted_task_layers(
    plan: &ExecutionPlan,
    schedule: &ReadySchedule,
    waves: &[Vec<PlanNodeId>],
) -> Result<Vec<Vec<PlanNodeId>>> {
    let hosted_tasks = plan
        .nodes
        .iter()
        .filter_map(|node| match &node.kind {
            PlanNodeKind::Exec { backend, .. }
                if backend.execution == crate::ir::ExecutionMode::Shim =>
            {
                Some(node.id)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let op_indices = schedule
        .ops
        .iter()
        .enumerate()
        .map(|(index, operation)| (operation.plan_node, index))
        .collect::<BTreeMap<_, _>>();
    let mut weighted_depth = vec![0usize; schedule.ops.len()];
    let mut layers = BTreeMap::<usize, Vec<PlanNodeId>>::new();
    let mut observed = BTreeSet::new();

    // `waves` is a validated topological order over every potential blocker.
    // Within a wave there are no dependency edges, so predecessor depths are
    // complete before the wave is visited.
    for wave in waves {
        for plan_node in wave {
            let operation_index = *op_indices.get(plan_node).with_context(|| {
                format!(
                    "admitted static wave references unknown operation {}",
                    plan_node.0
                )
            })?;
            let operation = &schedule.ops[operation_index];
            let predecessor_depth = operation
                .blocked_by
                .iter()
                .map(|predecessor| weighted_depth[*predecessor])
                .max()
                .unwrap_or(0);
            let hosted = hosted_tasks.contains(plan_node);
            let depth = predecessor_depth + usize::from(hosted);
            weighted_depth[operation_index] = depth;
            if hosted {
                observed.insert(*plan_node);
                layers.entry(depth).or_default().push(*plan_node);
            }
        }
    }

    if observed != hosted_tasks {
        bail!("hosted-task schedule projection differs from the shim-backed execution inventory");
    }
    let max_depth = layers.keys().next_back().copied().unwrap_or(0);
    let mut result = Vec::with_capacity(max_depth);
    for depth in 1..=max_depth {
        let layer = layers
            .remove(&depth)
            .with_context(|| format!("hosted-task schedule projection omitted layer {depth}"))?;
        result.push(layer);
    }
    Ok(result)
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
            let Some(reasons) =
                blocker_reasons(graph, &sequences, predecessor, op.plan_node, &node.kind)?
            else {
                continue;
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

fn schedule_why_witnesses(
    graph: &HGraph,
    consumer: PlanNodeId,
    producer_by_edge: &BTreeMap<EdgeId, PlanNodeId>,
    sequences: &BTreeSet<(PlanNodeId, PlanNodeId)>,
) -> Result<Vec<ScheduleWhyWitnessV1>> {
    let operation = graph
        .op_for(consumer)
        .with_context(|| format!("admitted graph omits operation P{}", consumer.0))?;
    let mut witnesses = Vec::new();
    for input in &operation.inputs {
        let node = graph.node(*input).with_context(|| {
            format!(
                "operation P{} references missing input N{}",
                consumer.0, input.0
            )
        })?;
        let Some(producer_edge) = node.producer else {
            continue;
        };
        let Some(predecessor) = producer_by_edge.get(&producer_edge).copied() else {
            continue;
        };
        let Some(reasons) = blocker_reasons(graph, sequences, predecessor, consumer, &node.kind)?
        else {
            continue;
        };
        witnesses.push(ScheduleWhyWitnessV1 {
            predecessor,
            input: *input,
            producer_edge,
            input_kind: node.kind.clone(),
            reasons,
        });
    }
    Ok(witnesses)
}

fn blocker_reasons(
    graph: &HGraph,
    sequences: &BTreeSet<(PlanNodeId, PlanNodeId)>,
    predecessor: PlanNodeId,
    successor: PlanNodeId,
    input_kind: &HNodeKind,
) -> Result<Option<Vec<BlockerReasonV1>>> {
    let reasons = match input_kind {
        HNodeKind::Value => vec![BlockerReasonV1::ValueDependency],
        HNodeKind::Completion { .. } => {
            let mut reasons = Vec::new();
            if sequences.contains(&(predecessor, successor)) {
                reasons.push(BlockerReasonV1::SourceCompletion);
            }
            let (predecessor_reads, _) = graph
                .effect_summary(predecessor)
                .with_context(|| {
                    format!("blocker producer P{} has no effect summary", predecessor.0)
                })?
                .scheduling_accesses();
            let (_, successor_writes) = graph
                .effect_summary(successor)
                .with_context(|| {
                    format!("blocker consumer P{} has no effect summary", successor.0)
                })?
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
        HNodeKind::AdmissionEvidence { .. } => return Ok(None),
    };
    Ok(Some(reasons))
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
        analyze_execution, runtime_binding_from_adapter_bytes, runtime_binding_from_directory,
        runtime_binding_from_directory_reusing_executables, BackendArtifactStateV1, CostEstimateV1,
        EvidenceProvenance,
    };
    use crate::hgraph::from_oir::build_program;
    use crate::hgraph::solve::solve_types;
    use crate::hgraph::{DomainFlags, ValueState};
    use crate::ir::{BackendRegistry, InvokeMode, OIr, OIrProgram, PlanNodeKind};
    use crate::parser::Parser;
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

    fn compile_admission(
        program: &OIrProgram,
        graph: HGraph,
        policy: Policy,
        runtime: RuntimeBindingV1,
    ) -> ExecutionAdmissionV5 {
        let plan = program.plan();
        let evidence = analyze_execution(program, &plan, &graph, runtime.clone())
            .expect("fixture evidence must analyze");
        admit_execution(program, &plan, graph, policy, runtime, evidence)
            .expect("fixture evidence must admit")
            .admission()
            .clone()
    }

    type LegalProjection = (
        Vec<Vec<PlanNodeId>>,
        Vec<(PlanNodeId, Vec<OperationBlockerV1>)>,
    );

    fn legal_projection(admission: &ExecutionAdmissionV5) -> LegalProjection {
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
        assert_eq!(evidence_a.schema(), EVIDENCE_SCHEMA_V5);
        assert_eq!(evidence_a.analyzer(), ANALYZER_ID_V5);
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
        assert!(explanation.starts_with("; ExecutionAdmission oexec.admission/v5\n"));
        assert!(explanation.contains("binding lowered-oir-sha256="));
        assert!(explanation.contains("binding backend-catalog-projection-sha256="));
        assert_eq!(
            admitted_a
                .admission()
                .bindings()
                .backend_catalog_projection_sha256
                .len(),
            64
        );
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
        assert!(explanation.contains(
            "it is not runtime discovery, health, authorization, capacity, or readiness evidence"
        ));
        assert!(explanation.contains("fixed-size per-run pool with per-completion wakeups"));
        assert!(explanation
            .contains("verified-pure infallible local-worker outputs may provisionally unlock"));
        assert_eq!(admitted_a.admission().admitted_max_wave_width(), 2);
        assert!(explanation.contains(
            "runtime-readiness=unknown placement-lease=none observed-overlap=not-run source=machine-default"
        ));
    }

    #[test]
    fn placement_admission_excludes_process_context_but_binds_semantic_coordinates() {
        let program = reader_writer_program("portable-admission");
        let plan = program.plan();

        let runtime_a = inspection_runtime(&plan, "process-context-a");
        let runtime_b = inspection_runtime(&plan, "process-context-b");
        assert_ne!(
            runtime_a.launch_context_sha256(),
            runtime_b.launch_context_sha256(),
            "fixture must perturb process-local launch context"
        );
        assert_eq!(
            runtime_a.backend_catalog_projection_sha256(),
            runtime_b.backend_catalog_projection_sha256(),
            "process perturbation must retain the semantic catalog projection"
        );

        let admission_a =
            compile_admission(&program, solved_graph(&program), Policy::Eager, runtime_a);
        let admission_b =
            compile_admission(&program, solved_graph(&program), Policy::Eager, runtime_b);
        assert_ne!(
            admission_a.admission_sha256(),
            admission_b.admission_sha256(),
            "full admission must retain process-local freshness"
        );
        assert_eq!(
            admission_a.placement_admission(),
            admission_b.placement_admission(),
            "placement admission must be portable across process context"
        );

        let lazy = compile_admission(
            &program,
            solved_graph(&program),
            Policy::Lazy,
            inspection_runtime(&plan, "process-context-a"),
        );
        assert_ne!(
            admission_a.placement_admission(),
            lazy.placement_admission(),
            "base policy is a semantic placement coordinate"
        );

        let mut changed_catalog_bindings = admission_a.bindings().clone();
        changed_catalog_bindings.backend_catalog_projection_sha256 = "ab".repeat(32);
        let changed_catalog = placement_admission_digest(
            &changed_catalog_bindings,
            admission_a.admitted_graph_sha256(),
            Policy::Eager,
        );
        assert_ne!(
            admission_a.placement_admission(),
            &changed_catalog,
            "current backend-catalog projection is a semantic placement coordinate"
        );

        let mut changed_graph_bindings = admission_a.bindings().clone();
        changed_graph_bindings.analyzed_graph_sha256 = "cd".repeat(32);
        let changed_analyzed_graph = placement_admission_digest(
            &changed_graph_bindings,
            admission_a.admitted_graph_sha256(),
            Policy::Eager,
        );
        assert_ne!(
            admission_a.placement_admission(),
            &changed_analyzed_graph,
            "analyzed HGraph semantics are a placement coordinate"
        );

        let changed_admitted_graph =
            placement_admission_digest(admission_a.bindings(), &"ef".repeat(32), Policy::Eager);
        assert_ne!(
            admission_a.placement_admission(),
            &changed_admitted_graph,
            "admitted HGraph semantics are a placement coordinate"
        );
    }

    #[test]
    fn default_worker_count_is_bounded_by_machine_and_admitted_width() {
        assert_eq!(resolve_worker_count(None, 12, 3), 3);
        assert_eq!(resolve_worker_count(None, 2, 7), 2);
        assert_eq!(resolve_worker_count(None, 12, 0), 1);
        assert_eq!(resolve_worker_count(Some(9), 2, 3), 9);
    }

    #[test]
    fn coordinator_only_admission_has_no_local_worker_wave() {
        let program = OIrProgram {
            nodes: vec![OIr::Text("coordinator-only".into())],
        };
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "coordinator-only-width");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

        assert_eq!(admitted.admission().admitted_max_wave_width(), 0);
        let explanation = admitted.admission().to_explanation_text();
        assert!(explanation.contains(
            "worker-count-covers-static-wave=not-applicable runtime-readiness=unknown placement-lease=none observed-overlap=not-run"
        ));
        assert!(explanation.contains(
            "task-count=0 predicted-width=0 predicted-span=0 span-unit=hosted-task-layers"
        ));
        assert!(!explanation
            .lines()
            .any(|line| line.starts_with("schedule-prediction-layer ")));
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
            format!("{error:#}").contains("exact canonical solved HGraph"),
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
            format!("{error:#}").contains("canonical ExecutionPlan"),
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
        assert_eq!(
            diagnostic,
            "execution admission runtime binding is stale; changed components: backend launch context digest"
        );
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
                    args: vec![python(1), python(2), python(3), python(4)],
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
        assert_eq!(hosted.len(), 4);
        assert!(hosted.iter().all(|node| {
            node.dispatch_contract.semantics == DispatchSemanticsV1::ExplicitAutonomousUnordered
                && node.failure_contract.class
                    == crate::evidence::FailureClassV1::MayFailUnorderedExternalEffects
                && !node.effect_contract.footprint_closed
        }));

        let admitted = admit_execution(
            &program,
            &plan,
            graph,
            Policy::Eager,
            runtime.clone(),
            evidence.clone(),
        )
        .unwrap();
        assert_eq!(admitted.admission().admitted_max_wave_width(), 4);
        let explanation = admitted
            .admission()
            .to_explanation_text_with_worker_override(Some(2));
        assert!(explanation.contains("adapter=autonomous-ephemeral-shim/v1"));
        assert!(explanation.contains("semantics=explicit-autonomous-unordered"));
        assert!(explanation.contains(
            "worker-count-covers-static-wave=no runtime-readiness=unknown placement-lease=none observed-overlap=not-run source=cli-override"
        ));
        assert!(explanation.contains("admitted-max-local-worker-wave-width=4 selected-workers=2"));
        assert_eq!(
            admitted.admission().admitted_hosted_task_max_wave_width(),
            4
        );
        assert_eq!(admitted.admission().admitted_hosted_task_wave_count(), 1);
        assert!(explanation.contains(
            "schedule-prediction schema=oexec.schedule-prediction/v1 status=admitted-static"
        ));
        assert!(explanation.contains(
            "task-count=4 predicted-width=4 predicted-span=1 span-unit=hosted-task-layers"
        ));
        let explanation_json = admitted
            .admission()
            .to_explanation_json_with_worker_override(Some(2))
            .unwrap();
        let explanation_value: serde_json::Value = serde_json::from_str(&explanation_json).unwrap();
        assert_eq!(explanation_value["schema"], SCHEDULE_EXPLANATION_SCHEMA_V1);
        assert_eq!(
            explanation_value["admission"]["bindings"]["admission_sha256"],
            admitted.admission().admission_sha256()
        );
        assert_eq!(
            explanation_value["prediction"]["admission_sha256"],
            admitted.admission().admission_sha256()
        );
        assert_eq!(explanation_value["prediction"]["task_count"], 4);
        assert_eq!(explanation_value["prediction"]["predicted_width"], 4);
        assert_eq!(explanation_value["prediction"]["predicted_span"], 1);
        assert_eq!(explanation_value["realizability"]["selected_workers"], 2);
        assert_eq!(
            explanation_value["realizability"]["worker_count_covers_static_wave"],
            "no"
        );

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
    fn hosted_task_prediction_collapses_zero_cost_bookkeeping() {
        let source = r#"
let seed = python^(
__oval_result__ = 10
)_python
let branches = autonomous(batch(
python^(__oval_result__ = seed + 1)_python,
python^(__oval_result__ = seed + 2)_python,
python^(__oval_result__ = seed + 3)_python,
python^(__oval_result__ = seed + 4)_python
))
python^(__oval_result__ = sum(branches))_python
"#;
        let backends = BackendRegistry::global().registered_backend_tags();
        let nodes = Parser::new(source, &backends).parse().unwrap();
        let program = OIrProgram::lower(&nodes);
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "hosted-task-prediction");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

        assert_eq!(
            admitted.admission().admitted_hosted_task_max_wave_width(),
            4
        );
        assert_eq!(admitted.admission().admitted_hosted_task_wave_count(), 3);
        assert_eq!(
            admitted
                .admission()
                .admitted_hosted_task_layers()
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![1, 4, 1]
        );
        let explanation = admitted.admission().to_explanation_text();
        assert!(explanation.contains(
            "task-count=6 predicted-width=4 predicted-span=3 span-unit=hosted-task-layers"
        ));
        assert_eq!(
            explanation
                .lines()
                .filter(|line| line.starts_with("schedule-prediction-layer "))
                .count(),
            3
        );
    }

    #[test]
    fn hosted_benchmark_fixtures_match_their_reviewed_topologies() {
        let fixtures = [
            (
                "heterogeneous",
                include_str!("../../benchmarks/hgraph_hosted/heterogeneous.O"),
                3,
                3,
                1,
            ),
            (
                "chained",
                include_str!("../../benchmarks/hgraph_hosted/chained.O"),
                4,
                1,
                4,
            ),
            (
                "mixed_width",
                include_str!("../../benchmarks/hgraph_hosted/mixed_width.O"),
                6,
                4,
                3,
            ),
            (
                "realistic",
                include_str!("../../benchmarks/hgraph_hosted/realistic.O"),
                4,
                2,
                3,
            ),
        ];

        for (name, source, task_count, width, span) in fixtures {
            let source = source
                .replace("__SLEEP_SECONDS__", "0")
                .replace("__SLEEP_MILLISECONDS__", "0");
            let backends = BackendRegistry::global().registered_backend_tags();
            let nodes = Parser::new(&source, &backends)
                .parse()
                .unwrap_or_else(|error| panic!("parse {name}: {error:#}"));
            let program = OIrProgram::lower(&nodes);
            let plan = program.plan();
            let graph = solved_graph(&program);
            let runtime = inspection_runtime(&plan, name);
            let evidence = analyze_execution(&program, &plan, &graph, runtime.clone())
                .unwrap_or_else(|error| panic!("analyze {name}: {error:#}"));
            let admitted =
                admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence)
                    .unwrap_or_else(|error| panic!("admit {name}: {error:#}"));

            assert_eq!(
                admitted
                    .admission()
                    .admitted_hosted_task_layers()
                    .iter()
                    .map(Vec::len)
                    .sum::<usize>(),
                task_count,
                "{name} hosted task count"
            );
            assert_eq!(
                admitted.admission().admitted_hosted_task_max_wave_width(),
                width,
                "{name} predicted width"
            );
            assert_eq!(
                admitted.admission().admitted_hosted_task_wave_count(),
                span,
                "{name} predicted span"
            );
        }
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

        let mut changed_runtime = admitted.runtime.clone();
        changed_runtime.backend_catalog_projection_sha256 = "changed-catalog".to_string();
        let error = admitted
            .verify_runtime(&changed_runtime)
            .expect_err("the changed catalog projection must invalidate admission");
        assert_eq!(
            error.to_string(),
            "execution admission runtime binding is stale; changed components: backend catalog projection digest"
        );
    }

    #[test]
    fn runtime_recheck_rejects_replaced_shim_artifact_before_execution() {
        let shim_dir = tempfile::tempdir().expect("create isolated shim directory");
        let shim_path = shim_dir.path().join("python_shim.py");
        std::fs::write(&shim_path, b"# admitted shim bytes\n")
            .expect("write the initially admitted shim artifact");

        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "python".to_string(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for("python"),
                body: vec![OIr::Text("artifact freshness".to_string())],
            }],
        };
        let plan = program.plan();
        let graph = solved_graph(&program);
        let context = &[("artifact-drift-test", "v1")];
        let runtime = runtime_binding_from_directory(&plan, shim_dir.path(), context).unwrap();
        assert!(
            runtime.backend_artifacts().iter().any(|artifact| {
                artifact.canonical_backend == "python"
                    && matches!(artifact.state, BackendArtifactStateV1::Hashed { .. })
            }),
            "expected a hashed Python shim binding, got {:#?}",
            runtime.backend_artifacts()
        );
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone())
            .expect("analyze against the initial shim bytes");
        let admitted = admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence)
            .expect("admit without dispatching the hosted operation");

        std::fs::write(&shim_path, b"# adversarially replaced shim bytes\n")
            .expect("replace the shim after admission");
        let current = runtime_binding_from_directory_reusing_executables(
            &plan,
            shim_dir.path(),
            context,
            admitted.runtime.executable_manifest.clone(),
        );
        let error = admitted
            .verify_runtime(&current)
            .expect_err("artifact replacement must stale the admission before dispatch");
        assert_eq!(
            error.to_string(),
            "execution admission runtime binding is stale; changed components: backend artifacts"
        );
    }

    #[test]
    fn unrelated_backend_does_not_change_python_launch_generation() {
        let shim_dir = tempfile::tempdir().expect("create isolated shim directory");
        std::fs::write(
            shim_dir.path().join("python_shim.py"),
            b"# stable Python shim generation\n",
        )
        .unwrap();
        let program = |include_bash: bool| {
            let mut nodes = vec![OIr::Exec {
                lang: "python".to_string(),
                env_id: 0,
                attr: None,
                backend: BackendRegistry::global().interface_for("python"),
                body: vec![OIr::Text("__oval_result__ = 1".to_string())],
            }];
            if include_bash {
                nodes.push(OIr::Exec {
                    lang: "bash".to_string(),
                    env_id: u32::MAX,
                    attr: None,
                    backend: BackendRegistry::global().interface_for("bash"),
                    body: vec![OIr::Text("printf unrelated".to_string())],
                });
            }
            OIrProgram { nodes }
        };
        let context = &[("launch-generation-test", "stable")];
        let admit = |program: &OIrProgram| {
            let plan = program.plan();
            let graph = solved_graph(program);
            let runtime = runtime_binding_from_directory(&plan, shim_dir.path(), context).unwrap();
            let evidence = analyze_execution(program, &plan, &graph, runtime.clone()).unwrap();
            // Keep the owned plan alive for the returned digest only; the
            // admission itself is consumed inside this closure.
            admit_execution(program, &plan, graph, Policy::Eager, runtime, evidence)
                .unwrap()
                .backend_launch_generation_sha256("python")
                .unwrap()
        };

        assert_eq!(admit(&program(false)), admit(&program(true)));
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
    fn schedule_why_preserves_exact_blocker_nodes_and_reverse_dependents() {
        let program = reader_writer_program("initial");
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "focused-why");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

        let loads = plan
            .nodes
            .iter()
            .filter_map(|node| matches!(node.kind, PlanNodeKind::Load { .. }).then_some(node.id))
            .collect::<Vec<_>>();
        let writer = plan
            .nodes
            .iter()
            .filter_map(|node| matches!(node.kind, PlanNodeKind::Store { .. }).then_some(node.id))
            .nth(1)
            .expect("fixture must contain a second writer");

        let why = admitted.schedule_why(writer).unwrap();
        assert_eq!(why.schema, SCHEDULE_WHY_SCHEMA_V1);
        assert_eq!(why.operation.plan_node, writer);
        for reader in &loads {
            let witness = why
                .blocker_witnesses
                .iter()
                .find(|witness| witness.predecessor == *reader)
                .unwrap_or_else(|| panic!("focused why omitted reader P{}", reader.0));
            assert!(matches!(
                witness.input_kind,
                HNodeKind::Completion { plan_node } if plan_node == *reader
            ));
            assert_eq!(
                admitted.graph().node(witness.input).unwrap().producer,
                Some(witness.producer_edge)
            );
            assert!(witness.reasons.contains(&BlockerReasonV1::ReaderDrain(
                ResourceKey::ScopeBinding("shared".into())
            )));
        }

        let reader_why = admitted.schedule_why(loads[0]).unwrap();
        assert!(reader_why.dependents.iter().any(|dependent| {
            dependent.operation == writer
                && dependent
                    .witnesses
                    .iter()
                    .any(|witness| witness.predecessor == loads[0])
        }));

        let text = why.to_text();
        assert!(text.contains("; ExecutionAdmissionWhy oexec.admission-why/v1"));
        assert!(text.contains("inspection-only=yes dispatch=not-run"));
        assert!(text.contains("blocker-witness predecessor=P"));
        assert!(text.contains("producer=E"));
        assert!(text.contains("kind=completion:P"));
        assert!(text.contains("why-note blockers and waves describe admitted static readiness"));
        assert!(!text.contains("; OIrProgram"));
        assert!(!text.contains("; HGraph"));
    }

    #[test]
    fn schedule_why_rejects_plan_nodes_outside_the_exact_admission() {
        let program = reader_writer_program("initial");
        let plan = program.plan();
        let graph = solved_graph(&program);
        let runtime = inspection_runtime(&plan, "focused-why-range");
        let evidence = analyze_execution(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

        let target = PlanNodeId(plan.nodes.len());
        let error = admitted.schedule_why(target).unwrap_err();
        assert!(
            error.to_string().contains(&format!(
                "ExecutionPlan contains {} nodes (valid range P0..P{})",
                plan.nodes.len(),
                plan.nodes.len() - 1
            )),
            "{error:#}"
        );

        let text_program = OIrProgram {
            nodes: vec![OIr::Text("materialized literal".into())],
        };
        let text_plan = text_program.plan();
        let text_graph = solved_graph(&text_program);
        let text_runtime = inspection_runtime(&text_plan, "focused-why-non-executable");
        let text_evidence =
            analyze_execution(&text_program, &text_plan, &text_graph, text_runtime.clone())
                .unwrap();
        let text_admitted = admit_execution(
            &text_program,
            &text_plan,
            text_graph,
            Policy::Eager,
            text_runtime,
            text_evidence,
        )
        .unwrap();
        let error = text_admitted.schedule_why(PlanNodeId(0)).unwrap_err();
        assert!(
            error.to_string().contains(
                "P0 exists in the ExecutionPlan as `text` but is not an admitted executable operation"
            ),
            "{error:#}"
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
