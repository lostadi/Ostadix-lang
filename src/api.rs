//! Curated external entry points for embedding Ostadix.
//!
//! Existing top-level modules remain available for 0.2 compatibility, but new
//! embedders should start here. The façade intentionally exposes semantic
//! requests, values, admission identities, and runtime handles rather than
//! every implementation module.

pub use crate::backend_morphism::{
    render_rust_scalar_stdout_program_v1, shadow_assess_backend_morphism_v1,
    BackendMorphismAssessmentV1, BackendMorphismErrorV1, BackendMorphismKernelV1,
    BackendMorphismV1, BackendNativeValueV1, BACKEND_MORPHISM_SCHEMA_V1,
};
pub use crate::eval::{Evaluator, Policy};
pub use crate::evidence::{
    AdmittedExecution, ExecutionIntentV1, ADMISSION_SCHEMA_V5, EVIDENCE_SCHEMA_V5,
    EXECUTION_INTENT_SCHEMA_V1,
};
pub use crate::hosted_remote::v2::{
    HostedV2Runtime, HostedV2RuntimeClosedV2, HostedV2RuntimeConfig, HostedV2RuntimeHandle,
    HostedV2RuntimeOwner, HostedV2RuntimeShutdownErrorV2,
};
pub use crate::parser::Parser;
pub use crate::value::{Fidelity, FidelityAssessmentV2, FidelityLossSet, ONumber, OValue};
pub use crate::version::{OstadixVersionReportV1, VERSION_REPORT_SCHEMA_V1};
pub use crate::world::PortableOValue;
