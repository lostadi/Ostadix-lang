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
pub mod profile;

pub use admit::{
    admit_execution, AdmittedExecution, AdmittedOperationV1, BlockerReasonV1, ExecutionAdmissionV1,
    OperationBlockerV1, RetainedSequenceV1, SequenceRetentionReasonV1,
};
pub use analyze::{
    analyze_execution, runtime_binding_from_adapter_bytes, runtime_binding_from_directory,
};
pub use fact::{
    BackendArtifactStateV1, BackendArtifactV1, CapabilityDispositionV1, CostEstimateV1,
    DispatchContractV1, DispatchLaneV1, EffectContractV1, EvidenceBindingsV1, EvidenceBundleV1,
    EvidenceProvenance, FailureClassV1, FailureContractV1, NodeEvidence, PlacementContractV1,
    ResourceDemandContractV1, RuntimeBindingV1, RuntimeSnapshotKindV1, TypeContractV1,
    ADMISSION_SCHEMA_V1, ANALYZER_ID_V1, EVIDENCE_SCHEMA_V1,
};
