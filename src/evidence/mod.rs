//! Evidence-bound pre-execution admission for the OIR HGraph coordinator.
//!
//! Observations and receipts remain authority-free post-execution artifacts.
//! This module instead derives versioned facts before dispatch, binds them to
//! the exact lowered program/plan/solved graph/runtime snapshot, compiles them
//! into materialized HGraph readiness inputs, and yields the only graph type
//! accepted by [`crate::executor::Coordinator`].

mod admit;
mod analyze;
mod fact;
mod intent;
pub mod profile;

pub(crate) use admit::PreparedAdmissionPartsV2;
pub use admit::{
    admit_execution, admit_execution_v5, admit_execution_v6, AdmittedExecution,
    AdmittedExecutionV5, AdmittedExecutionV6, AdmittedOperationV1, AdmittedOperationV2,
    BlockerReasonV1, ExecutionAdmissionV5, ExecutionAdmissionV6, OperationBlockerV1,
    RetainedSequenceV1, ScheduleExplanationAdmissionV1, ScheduleExplanationAdmissionV2,
    ScheduleExplanationBindingsV1, ScheduleExplanationBindingsV2, ScheduleExplanationV1,
    ScheduleExplanationV2, SchedulePredictionLayerV1, SchedulePredictionV1,
    ScheduleRealizabilityV1, ScheduleWhyDependentV1, ScheduleWhyViewV1, ScheduleWhyViewV2,
    ScheduleWhyWitnessV1, SequenceRetentionReasonV1, EXECUTION_ADMISSION_DIGEST_DOMAIN_V5,
    EXECUTION_ADMISSION_DIGEST_DOMAIN_V6, PLACEMENT_ADMISSION_DIGEST_DOMAIN_V1,
    PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2, SCHEDULE_EXPLANATION_SCHEMA_V1,
    SCHEDULE_EXPLANATION_SCHEMA_V2, SCHEDULE_PREDICTION_SCHEMA_V1,
    SCHEDULE_REALIZABILITY_SCHEMA_V1, SCHEDULE_WHY_SCHEMA_V1, SCHEDULE_WHY_SCHEMA_V2,
};
pub use analyze::{
    analyze_execution, analyze_execution_v5, analyze_execution_v6, evidence_bundle_sha256_v5,
    evidence_bundle_sha256_v6, graph_sha256_v1, graph_sha256_v2,
    runtime_binding_from_adapter_bytes, runtime_binding_from_directory,
    runtime_binding_from_directory_reusing_executables,
    runtime_binding_from_directory_with_current_executable, EVIDENCE_BUNDLE_DIGEST_DOMAIN_V5,
    EVIDENCE_BUNDLE_DIGEST_DOMAIN_V6, SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V1,
    SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2,
};
pub use fact::{
    BackendArtifactStateV1, BackendArtifactV1, CapabilityDispositionV1, CostEstimateV1,
    DispatchAdapterV1, DispatchContractV1, DispatchLaneV1, DispatchSemanticsV1, EffectContractV1,
    EvidenceBindingsV2, EvidenceBundleV5, EvidenceBundleV6, EvidenceProvenance, FailureClassV1,
    FailureContractV1, NodeEvidence, NodeEvidenceV1, NodeEvidenceV2, PlacementContractV1,
    ResourceDemandContractV1, RuntimeBindingV1, RuntimeSnapshotKindV1, TypeContractV1,
    TypeContractV2, ADMISSION_SCHEMA_V5, ADMISSION_SCHEMA_V6, ANALYZER_ID_V5, ANALYZER_ID_V6,
    EVIDENCE_SCHEMA_V5, EVIDENCE_SCHEMA_V6,
};
pub use intent::{source_sha256, ExecutionIntentV1, EXECUTION_INTENT_SCHEMA_V1};
