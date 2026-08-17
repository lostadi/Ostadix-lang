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
pub use crate::eval::Evaluator;
pub use crate::evidence::{
    admit_execution_v6, analyze_execution_v6, evidence_bundle_sha256_v6, graph_sha256_v2,
    AdmittedExecution, AdmittedExecutionV6, AdmittedOperationV2, EvidenceBundleV6,
    ExecutionAdmissionV6, ExecutionIntentV1, NodeEvidenceV2, ScheduleWhyViewV2, TypeContractV2,
    ADMISSION_SCHEMA_V5, ADMISSION_SCHEMA_V6, ANALYZER_ID_V6, EVIDENCE_BUNDLE_DIGEST_DOMAIN_V6,
    EVIDENCE_SCHEMA_V5, EVIDENCE_SCHEMA_V6, EXECUTION_ADMISSION_DIGEST_DOMAIN_V6,
    EXECUTION_INTENT_SCHEMA_V1, PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2, SCHEDULE_WHY_SCHEMA_V2,
    SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2,
};
pub use crate::execution_contract::Policy;
pub use crate::hosted_remote::v2::{
    HostedV2Runtime, HostedV2RuntimeClosedV2, HostedV2RuntimeConfig, HostedV2RuntimeHandle,
    HostedV2RuntimeOwner, HostedV2RuntimeShutdownErrorV2,
};
pub use crate::parser::Parser;
pub use crate::value::{Fidelity, FidelityAssessmentV2, FidelityLossSet, ONumber, OValue};
pub use crate::version::{OstadixVersionReportV1, VERSION_REPORT_SCHEMA_V1};
pub use crate::world::PortableOValue;

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    #[test]
    fn curated_policy_is_the_canonical_execution_contract_type() {
        assert_eq!(
            TypeId::of::<super::Policy>(),
            TypeId::of::<crate::execution_contract::Policy>()
        );
        assert_eq!(
            TypeId::of::<super::Policy>(),
            TypeId::of::<crate::eval::Policy>()
        );

        let canonical: crate::execution_contract::Policy = super::Policy::Lazy;
        let compatibility: crate::eval::Policy = canonical;
        let _: super::Policy = compatibility;
    }

    #[test]
    fn explicit_v6_facade_is_additive_and_current_constants_remain_v5() {
        assert_eq!(super::ADMISSION_SCHEMA_V5, "oexec.admission/v5");
        assert_eq!(super::EVIDENCE_SCHEMA_V5, "oexec.evidence/v5");
        assert_eq!(super::ADMISSION_SCHEMA_V6, "oexec.admission/v6");
        assert_eq!(super::EVIDENCE_SCHEMA_V6, "oexec.evidence/v6");
        assert_eq!(super::SCHEDULE_WHY_SCHEMA_V2, "oexec.admission-why/v2");
        assert_eq!(
            super::SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2,
            "ostadix-solved-executable-hgraph/v2"
        );
        assert_eq!(
            TypeId::of::<super::EvidenceBundleV6>(),
            TypeId::of::<crate::evidence::EvidenceBundleV6>()
        );
        assert_eq!(
            TypeId::of::<super::AdmittedExecutionV6<'static>>(),
            TypeId::of::<crate::evidence::AdmittedExecutionV6<'static>>()
        );
        let _: fn(&crate::hgraph::HGraph) -> String = super::graph_sha256_v2;
    }
}
