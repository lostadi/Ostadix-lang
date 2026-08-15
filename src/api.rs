//! Curated external entry points for embedding Ostadix.
//!
//! Existing top-level modules remain available for 0.2 compatibility, but new
//! embedders should start here. The façade intentionally exposes semantic
//! requests, values, admission identities, and runtime handles rather than
//! every implementation module.

pub use crate::eval::{Evaluator, Policy};
pub use crate::evidence::{
    AdmittedExecution, ExecutionIntentV1, ADMISSION_SCHEMA_V5, EVIDENCE_SCHEMA_V5,
    EXECUTION_INTENT_SCHEMA_V1,
};
pub use crate::hosted_remote::v2::{
    HostedV2Runtime, HostedV2RuntimeClosedV2, HostedV2RuntimeConfig, HostedV2RuntimeShutdownErrorV2,
};
pub use crate::parser::Parser;
pub use crate::value::{Fidelity, FidelityAssessmentV2, FidelityLossSet, ONumber, OValue};
pub use crate::version::{OstadixVersionReportV1, VERSION_REPORT_SCHEMA_V1};
pub use crate::world::PortableOValue;
