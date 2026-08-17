//! Curated external entry points for embedding Ostadix.
//!
//! Existing top-level modules remain available for compatibility, but new
//! embedders should start here. The facade intentionally exposes semantic
//! requests, values, admission identities, and runtime handles rather than
//! every implementation module.

pub use crate::backend_morphism::{
    render_rust_scalar_stdout_program_v1, shadow_assess_backend_morphism_v1,
    BackendMorphismAssessmentV1, BackendMorphismErrorV1, BackendMorphismKernelV1,
    BackendMorphismV1, BackendNativeValueV1, BACKEND_MORPHISM_SCHEMA_V1,
};
pub use crate::eval::{
    Evaluator, PlacementFragmentBindingsV1, PlacementFragmentBindingsV2,
    PreparedPlacementFragmentV1, PreparedPlacementFragmentV2,
};
pub use crate::evidence::{
    admit_execution, admit_execution_v5, admit_execution_v6, analyze_execution,
    analyze_execution_v5, analyze_execution_v6, evidence_bundle_sha256_v5,
    evidence_bundle_sha256_v6, graph_sha256_v1, graph_sha256_v2, AdmittedExecution,
    AdmittedExecutionV5, AdmittedExecutionV6, AdmittedOperationV1, AdmittedOperationV2,
    EvidenceBundleV5, EvidenceBundleV6, ExecutionAdmissionV5, ExecutionAdmissionV6,
    ExecutionIntentV1, NodeEvidence, NodeEvidenceV1, NodeEvidenceV2, ScheduleExplanationV1,
    ScheduleExplanationV2, ScheduleWhyViewV1, ScheduleWhyViewV2, TypeContractV1, TypeContractV2,
    ADMISSION_SCHEMA_V5, ADMISSION_SCHEMA_V6, ANALYZER_ID_V5, ANALYZER_ID_V6,
    EVIDENCE_BUNDLE_DIGEST_DOMAIN_V5, EVIDENCE_BUNDLE_DIGEST_DOMAIN_V6, EVIDENCE_SCHEMA_V5,
    EVIDENCE_SCHEMA_V6, EXECUTION_ADMISSION_DIGEST_DOMAIN_V5, EXECUTION_ADMISSION_DIGEST_DOMAIN_V6,
    EXECUTION_INTENT_SCHEMA_V1, PLACEMENT_ADMISSION_DIGEST_DOMAIN_V1,
    PLACEMENT_ADMISSION_DIGEST_DOMAIN_V2, SCHEDULE_EXPLANATION_SCHEMA_V1,
    SCHEDULE_EXPLANATION_SCHEMA_V2, SCHEDULE_WHY_SCHEMA_V1, SCHEDULE_WHY_SCHEMA_V2,
    SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V1, SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2,
};
pub use crate::execution_contract::Policy;
pub use crate::hosted_remote::v2::{
    HostedV2Runtime, HostedV2RuntimeClosedV2, HostedV2RuntimeConfig, HostedV2RuntimeHandle,
    HostedV2RuntimeOwner, HostedV2RuntimeShutdownErrorV2,
};
pub use crate::information_bridge::{
    project_evidence_v6, project_hgraph_v1, project_hosted_journal_v2, project_logical_hgraph_v1,
    project_parsed_document_v1, project_public_value_v1, project_registry_profile_v1,
    project_world_receipt_v1, EvidenceInformationV1, HGraphInformationV1,
    HostedJournalInformationV1, InformationBridgeErrorV1, ParsedDocumentInformationV1,
    ProjectGraphInformationV1, PublicValueInformationV1, RegistryProfileInformationV1,
    WorldReceiptInformationV1, EVIDENCE_INFORMATION_SCHEMA_V1,
    EVIDENCE_METADATA_PROJECTION_DIGEST_DOMAIN_V1, HGRAPH_INFORMATION_SCHEMA_V1,
    HGRAPH_METADATA_PROJECTION_DIGEST_DOMAIN_V1, HOSTED_ENTRY_IDENTITY_DIGEST_DOMAIN_V1,
    HOSTED_JOURNAL_INFORMATION_SCHEMA_V1, HOSTED_SESSION_IDENTITY_DIGEST_DOMAIN_V1,
    INFORMATION_BRIDGE_MEDIA_TYPE_V1, INFORMATION_BRIDGE_SCHEMA_V1,
    MAX_INFORMATION_BRIDGE_DECODE_DEPTH_V1, MAX_INFORMATION_BRIDGE_DECODE_ITEMS_V1,
    MAX_INFORMATION_BRIDGE_RECORD_BYTES_V1, MAX_PUBLIC_VALUE_CANONICAL_BYTES_V1,
    MAX_PUBLIC_VALUE_IDENTIFIER_BYTES_V1, MAX_PUBLIC_VALUE_NUMBER_BYTES_V1,
    MAX_PUBLIC_VALUE_NUMBER_DEPTH_V1, MAX_PUBLIC_VALUE_NUMBER_NODES_V1,
    MAX_PUBLIC_VALUE_TEXT_BYTES_V1, PARSED_DOCUMENT_INFORMATION_SCHEMA_V1,
    PROJECT_GRAPH_INFORMATION_SCHEMA_V1, PUBLIC_VALUE_INFORMATION_SCHEMA_V1,
    REGISTRY_NODE_IDENTITY_DIGEST_DOMAIN_V1, REGISTRY_PROFILE_INFORMATION_SCHEMA_V1,
    WORLD_RECEIPT_INFORMATION_SCHEMA_V1,
};
pub use crate::parser::Parser;
pub use crate::value::{
    BackendAuthority, CapabilityKind, DecimalSpecial, Fidelity, FidelityAssessmentV2,
    FidelityLossSet, FloatFormat, FloatSpecial, GraphNode, GroupMode, NativeBoundary,
    NativeCodecSafety, NativeIdentity, NodeId, OBytes, OKeyword, ONative, ONumber, OSymbol, OText,
    OValue, RehydratePolicy, RequestKind, RuntimeBoundary, SeqKind, SetKind, SnapshotKind,
};
pub use crate::version::{OstadixVersionReportV1, VERSION_REPORT_SCHEMA_V1};
pub use crate::world::PortableOValue;
pub use num_bigint::BigInt;

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
    fn current_aliases_are_v6_and_archival_v5_coordinates_remain_explicit() {
        assert_eq!(super::ADMISSION_SCHEMA_V5, "oexec.admission/v5");
        assert_eq!(super::EVIDENCE_SCHEMA_V5, "oexec.evidence/v5");
        assert_eq!(super::ADMISSION_SCHEMA_V6, "oexec.admission/v6");
        assert_eq!(super::EVIDENCE_SCHEMA_V6, "oexec.evidence/v6");
        assert_eq!(
            super::SCHEDULE_EXPLANATION_SCHEMA_V1,
            "oexec.schedule-explanation/v1"
        );
        assert_eq!(
            super::SCHEDULE_EXPLANATION_SCHEMA_V2,
            "oexec.schedule-explanation/v2"
        );
        assert_eq!(super::SCHEDULE_WHY_SCHEMA_V1, "oexec.admission-why/v1");
        assert_eq!(super::SCHEDULE_WHY_SCHEMA_V2, "oexec.admission-why/v2");
        assert_eq!(
            super::SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V1,
            "ostadix-solved-executable-hgraph/v1"
        );
        assert_eq!(
            super::SOLVED_EXECUTABLE_HGRAPH_DIGEST_DOMAIN_V2,
            "ostadix-solved-executable-hgraph/v2"
        );
        assert_eq!(
            TypeId::of::<super::EvidenceBundleV6>(),
            TypeId::of::<crate::evidence::EvidenceBundleV6>()
        );
        assert_eq!(
            TypeId::of::<super::NodeEvidence>(),
            TypeId::of::<super::NodeEvidenceV2>()
        );
        assert_eq!(
            TypeId::of::<super::AdmittedExecution<'static>>(),
            TypeId::of::<super::AdmittedExecutionV6<'static>>()
        );
        assert_eq!(
            TypeId::of::<super::AdmittedExecutionV6<'static>>(),
            TypeId::of::<crate::evidence::AdmittedExecutionV6<'static>>()
        );
        let _: fn(&crate::hgraph::HGraph) -> String = super::graph_sha256_v2;
    }
}
