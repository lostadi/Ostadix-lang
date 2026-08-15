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

pub(crate) use admit::PreparedAdmissionPartsV1;
pub use admit::{
    admit_execution, AdmittedExecution, AdmittedOperationV1, BlockerReasonV1, ExecutionAdmissionV5,
    OperationBlockerV1, RetainedSequenceV1, ScheduleWhyDependentV1, ScheduleWhyViewV1,
    ScheduleWhyWitnessV1, SequenceRetentionReasonV1, PLACEMENT_ADMISSION_DIGEST_DOMAIN_V1,
    SCHEDULE_WHY_SCHEMA_V1,
};
pub use analyze::{
    analyze_execution, runtime_binding_from_adapter_bytes, runtime_binding_from_directory,
    runtime_binding_from_directory_reusing_executables,
    runtime_binding_from_directory_with_current_executable,
};
pub use fact::{
    BackendArtifactStateV1, BackendArtifactV1, CapabilityDispositionV1, CostEstimateV1,
    DispatchAdapterV1, DispatchContractV1, DispatchLaneV1, DispatchSemanticsV1, EffectContractV1,
    EvidenceBindingsV2, EvidenceBundleV5, EvidenceProvenance, FailureClassV1, FailureContractV1,
    NodeEvidence, PlacementContractV1, ResourceDemandContractV1, RuntimeBindingV1,
    RuntimeSnapshotKindV1, TypeContractV1, ADMISSION_SCHEMA_V5, ANALYZER_ID_V5, EVIDENCE_SCHEMA_V5,
};
pub use intent::{source_sha256, ExecutionIntentV1, EXECUTION_INTENT_SCHEMA_V1};
