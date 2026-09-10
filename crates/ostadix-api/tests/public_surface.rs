use std::path::PathBuf;

use ostadix_api::computation::OComputationBuilderV1;
use ostadix_api::computation_core::{
    verify_realization_set_v1, ComputationLineageId, FacetIdV1, FacetRefV1, OComputationErrorV1,
    OComputationManifestV1, OperationContractIdV1, OperationContractV1, OperationIdV1,
    OperationInterfaceIdV1, OperationInterfaceV1, OperationPortV1, OperationShapeParameterV1,
    RealizationDescriptorIdV1, RealizationDescriptorV1, RealizationIdV1,
    RealizationPortRepresentationsV1, RealizationSetIdV1, RealizationSetV1, SemanticArtifactRefV1,
    VerifiedOComputationV1, OCOMPUTATION_MANIFEST_SCHEMA_V1, OPERATION_CONTRACT_SCHEMA_V1,
    OPERATION_INTERFACE_SCHEMA_V1, REALIZATION_DESCRIPTOR_SCHEMA_V1, REALIZATION_SET_SCHEMA_V1,
};
use ostadix_api::execution_fabric::{
    ExecutionCandidateV1, ExecutionCapsuleV1, ExecutionIdV1, EXECUTION_CANDIDATE_SCHEMA_V1,
    EXECUTION_CAPSULE_SCHEMA_V1,
};
use ostadix_api::execution_fabric_authority::{
    FabricRequestV1, FabricResponseV1, FabricSigningKeyV1, FabricSubmissionV1,
    FabricTargetBindingV1, PlacementLeaseV3, FABRIC_PLACEMENT_LEASE_SCHEMA_V3,
    FABRIC_REQUEST_SCHEMA_V1, FABRIC_RESPONSE_SCHEMA_V1, FABRIC_SIGNED_LEASE_SCHEMA_V3,
    FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1, FABRIC_SOURCE_CLOSURE_DIALECT_V1,
    FABRIC_SOURCE_CLOSURE_SCHEMA_V1, FABRIC_SUBMISSION_SCHEMA_V1,
    FABRIC_TERMINAL_RECEIPT_SCHEMA_V1,
};
use ostadix_api::hosted_remote::{
    trusted_inline_fabric_realization_pipeline_sha256_v1, FabricAttemptProviderConfigV1,
    FabricAttemptProviderV1, RemotePureExecutionConfigV1, EXECUTION_FABRIC_TLS_ALPN_V1,
    HOSTED_TLS_ALPN_MESH_V1, HOSTED_TLS_ALPN_V1, HOSTED_TLS_ALPN_V2,
};
use ostadix_api::{
    BackendAuthority, BigInt, CapabilityKind, DecimalSpecial, FloatFormat, FloatSpecial, GraphNode,
    GroupMode, NativeBoundary, NativeCodecSafety, NativeIdentity, NodeId, OBytes, OKeyword,
    ONative, ONumber, OSymbol, OText, OValue, RehydratePolicy, RequestKind, Runtime,
    RuntimeBoundary, RuntimeStage, SeqKind, SetKind, SnapshotKind,
};

fn assert_public<T>() {}

#[test]
fn independent_package_carries_its_license_and_notice() {
    let license = include_bytes!("../LICENSE");
    let notice = include_bytes!("../NOTICE");
    assert!(license
        .windows(b"GNU LESSER GENERAL PUBLIC LICENSE".len())
        .any(|window| window == b"GNU LESSER GENERAL PUBLIC LICENSE"));
    assert!(notice
        .windows(b"Ostadix".len())
        .any(|window| window == b"Ostadix"));
}

#[test]
fn ocomputation_identity_spine_is_nameable_from_the_engine_root() {
    assert_public::<ComputationLineageId>();
    assert_public::<OComputationManifestV1>();
    assert_public::<VerifiedOComputationV1>();
    assert_public::<OComputationBuilderV1>();
    assert_eq!(
        OCOMPUTATION_MANIFEST_SCHEMA_V1,
        "ostadix.ocomputation-manifest/v1"
    );
}

#[test]
fn operation_realization_schema_is_nameable_from_the_engine_root() {
    macro_rules! assert_semantic_record_api {
        ($record:ty, $id:ty) => {{
            let _verify: fn($record) -> Result<$record, OComputationErrorV1> = <$record>::verify;
            let _decode_canonical: fn(&[u8]) -> Result<$record, OComputationErrorV1> =
                <$record>::decode_canonical;
            let _decode_json: fn(&[u8]) -> Result<$record, OComputationErrorV1> =
                <$record>::decode_json;
            let _canonical_bytes: fn(&$record) -> Result<Vec<u8>, OComputationErrorV1> =
                <$record>::canonical_bytes;
            let _canonical_json: fn(&$record) -> Result<Vec<u8>, OComputationErrorV1> =
                <$record>::canonical_json;
            let _canonical_json_pretty: fn(&$record) -> Result<Vec<u8>, OComputationErrorV1> =
                <$record>::canonical_json_pretty;
            let _id: fn(&$record) -> Result<$id, OComputationErrorV1> = <$record>::id;
            let _facet_ref: fn(&$record, FacetIdV1) -> Result<FacetRefV1, OComputationErrorV1> =
                <$record>::facet_ref;
        }};
    }

    assert_public::<OperationIdV1>();
    assert_public::<RealizationIdV1>();
    assert_public::<OperationContractIdV1>();
    assert_public::<OperationInterfaceIdV1>();
    assert_public::<RealizationDescriptorIdV1>();
    assert_public::<RealizationSetIdV1>();
    assert_public::<SemanticArtifactRefV1>();
    assert_public::<OperationPortV1>();
    assert_public::<OperationShapeParameterV1>();
    assert_public::<RealizationPortRepresentationsV1>();
    assert_public::<OperationContractV1>();
    assert_public::<OperationInterfaceV1>();
    assert_public::<RealizationDescriptorV1>();
    assert_public::<RealizationSetV1>();
    assert_semantic_record_api!(OperationContractV1, OperationContractIdV1);
    assert_semantic_record_api!(OperationInterfaceV1, OperationInterfaceIdV1);
    assert_semantic_record_api!(RealizationDescriptorV1, RealizationDescriptorIdV1);
    assert_semantic_record_api!(RealizationSetV1, RealizationSetIdV1);
    let _verify: fn(
        &OperationContractV1,
        &OperationInterfaceV1,
        &[RealizationDescriptorV1],
        &RealizationSetV1,
    ) -> Result<(), OComputationErrorV1> = verify_realization_set_v1;
    assert_eq!(
        OPERATION_CONTRACT_SCHEMA_V1,
        "ostadix.operation-contract/v1"
    );
    assert_eq!(
        OPERATION_INTERFACE_SCHEMA_V1,
        "ostadix.operation-interface/v1"
    );
    assert_eq!(
        REALIZATION_DESCRIPTOR_SCHEMA_V1,
        "ostadix.realization-descriptor/v1"
    );
    assert_eq!(REALIZATION_SET_SCHEMA_V1, "ostadix.realization-set/v1");
}

#[test]
fn authenticated_execution_fabric_is_nameable_from_the_engine_root() {
    assert_public::<ExecutionCapsuleV1>();
    assert_public::<ExecutionCandidateV1>();
    assert_public::<ExecutionIdV1>();
    assert_public::<FabricRequestV1>();
    assert_public::<FabricResponseV1>();
    assert_public::<FabricSubmissionV1>();
    assert_public::<FabricTargetBindingV1>();
    assert_public::<PlacementLeaseV3>();
    assert_public::<FabricSigningKeyV1>();
    assert_public::<FabricAttemptProviderConfigV1>();
    assert_public::<FabricAttemptProviderV1>();
    assert_public::<RemotePureExecutionConfigV1>();

    assert_eq!(
        EXECUTION_CAPSULE_SCHEMA_V1,
        "ostadix.oir-execution-capsule/v1"
    );
    assert_eq!(
        EXECUTION_CANDIDATE_SCHEMA_V1,
        "ostadix.oir-execution-candidate/v1"
    );
    assert_eq!(
        FABRIC_REQUEST_SCHEMA_V1,
        "ostadix.execution-fabric-request/v1"
    );
    assert_eq!(
        FABRIC_RESPONSE_SCHEMA_V1,
        "ostadix.execution-fabric-response/v1"
    );
    assert_eq!(
        FABRIC_SUBMISSION_SCHEMA_V1,
        "ostadix.execution-fabric-submission/v1"
    );
    assert_eq!(
        FABRIC_SOURCE_CLOSURE_SCHEMA_V1,
        "ostadix.execution-source-closure/v1"
    );
    assert_eq!(
        FABRIC_SOURCE_CLOSURE_DIALECT_V1,
        "ostadix-source-closure/v1"
    );
    assert_eq!(
        FABRIC_PLACEMENT_LEASE_SCHEMA_V3,
        "ostadix.execution-placement-lease/v3"
    );
    assert_eq!(
        FABRIC_SIGNED_LEASE_SCHEMA_V3,
        "ostadix.signed-execution-lease/v3"
    );
    assert_eq!(
        FABRIC_TERMINAL_RECEIPT_SCHEMA_V1,
        "ostadix.execution-fabric-terminal-receipt/v1"
    );
    assert_eq!(
        FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1,
        "ostadix.signed-execution-fabric-terminal-receipt/v1"
    );
    assert_eq!(HOSTED_TLS_ALPN_V1, &b"ostadix-hosted/1"[..]);
    assert_eq!(HOSTED_TLS_ALPN_V2, &b"ostadix-hosted/2"[..]);
    assert_eq!(HOSTED_TLS_ALPN_MESH_V1, &b"ostadix-mesh/1"[..]);
    assert_eq!(
        EXECUTION_FABRIC_TLS_ALPN_V1,
        &b"ostadix-execution-fabric/1"[..]
    );

    let pipeline = trusted_inline_fabric_realization_pipeline_sha256_v1("text")
        .expect("trusted text renderer must expose its Fabric pipeline identity");
    assert_eq!(pipeline.as_sha256().len(), 64);
}

#[test]
fn complete_ovalue_payload_vocabulary_is_nameable_from_the_engine_root() {
    assert_public::<BackendAuthority>();
    assert_public::<BigInt>();
    assert_public::<CapabilityKind>();
    assert_public::<DecimalSpecial>();
    assert_public::<FloatFormat>();
    assert_public::<FloatSpecial>();
    assert_public::<GraphNode>();
    assert_public::<GroupMode>();
    assert_public::<NativeBoundary>();
    assert_public::<NativeCodecSafety>();
    assert_public::<NativeIdentity>();
    assert_public::<NodeId>();
    assert_public::<OBytes>();
    assert_public::<OKeyword>();
    assert_public::<ONative>();
    assert_public::<ONumber>();
    assert_public::<OSymbol>();
    assert_public::<OText>();
    assert_public::<OValue>();
    assert_public::<RehydratePolicy>();
    assert_public::<RequestKind>();
    assert_public::<RuntimeBoundary>();
    assert_public::<SeqKind>();
    assert_public::<SetKind>();
    assert_public::<SnapshotKind>();

    let value = OValue::Number {
        v: ONumber::Int { v: BigInt::from(3) },
    };
    assert!(matches!(value, OValue::Number { .. }));
}

#[test]
fn runtime_owns_success_parse_failure_and_evaluate_failure_stages() {
    let mut runtime = Runtime::new(PathBuf::new());
    assert_eq!(runtime.evaluate("").unwrap(), OValue::Null);

    let parse = runtime.evaluate("python^(unterminated").unwrap_err();
    assert_eq!(parse.stage(), RuntimeStage::Parse);
    assert!(!parse.message().is_empty());

    let evaluate = runtime.evaluate("now()").unwrap_err();
    assert_eq!(evaluate.stage(), RuntimeStage::Evaluate);
    assert!(!evaluate.message().is_empty());
}

#[test]
fn runtime_matches_cli_shebang_and_repeated_document_boundaries() {
    let mut runtime = Runtime::new(PathBuf::new());
    assert_eq!(
        runtime.evaluate("#!/usr/bin/env O\n").unwrap(),
        OValue::Null
    );
    assert_eq!(runtime.evaluate("").unwrap(), OValue::Null);
}

#[test]
fn engine_owns_the_full_runtime_without_a_compatibility_dependency() {
    let source = include_str!("../src/lib.rs");
    let manifest = include_str!("../Cargo.toml");

    for owned_module in [
        "pub mod eval;",
        "pub mod evidence;",
        "pub mod execution_fabric;",
        "pub mod execution_fabric_authority;",
        "pub mod executor;",
        "pub mod hgraph;",
        "pub mod information_bridge;",
        "pub mod information_provenance;",
        "pub mod parser;",
        "pub mod project;",
        "pub mod runtime_exec;",
        "pub mod value;",
        "pub mod world;",
    ] {
        assert!(source.contains(owned_module), "missing {owned_module}");
    }
    assert!(!source.contains("pub use o_lang"));
    assert!(!manifest
        .lines()
        .any(|line| line.trim_start().starts_with("o-lang")));

    assert_public::<ostadix_api::eval::Evaluator>();
    assert_public::<ostadix_api::evidence::ExecutionAdmissionV6>();
    assert_public::<ostadix_api::hgraph::HGraph>();
    assert_public::<ostadix_api::information::InformationProvenanceV2>();
    assert_public::<ostadix_api::information_provenance::InformationProvenanceAnalyzerV2>();
    assert_public::<ostadix_api::parser::Parser<'static>>();
}

#[test]
fn engine_source_keeps_parser_and_information_bridge_boundaries_closed() {
    let parser_source = include_str!("../src/parser.rs");
    assert!(!parser_source.contains("pub nodes: Vec<ONode>"));

    let bridge_source = include_str!("../src/information_bridge/mod.rs");
    assert!(!bridge_source.contains("graph_sha256_v2"));
    assert!(!bridge_source.contains("evidence_bundle_sha256_v6"));

    for record in [
        "ParsedDocumentInformationV1",
        "PublicValueInformationV1",
        "HGraphInformationV1",
        "EvidenceInformationV1",
        "RegistryProfileInformationV1",
        "WorldReceiptInformationV1",
        "ProjectGraphInformationV1",
        "HostedJournalInformationV1",
    ] {
        let declaration = format!("pub struct {record}");
        let declaration_at = bridge_source
            .find(&declaration)
            .unwrap_or_else(|| panic!("missing public bridge record {record}"));
        let derive_at = bridge_source[..declaration_at]
            .rfind("#[derive(")
            .unwrap_or_else(|| panic!("missing derive boundary for {record}"));
        let public_prelude = &bridge_source[derive_at..declaration_at];
        assert!(!public_prelude.contains("Serialize"), "{record}");
        assert!(!public_prelude.contains("Deserialize"), "{record}");
        assert!(!public_prelude.contains("#[serde"), "{record}");
        assert!(!bridge_source.contains(&format!("impl Serialize for {record}")));
        assert!(!bridge_source.contains(&format!("impl Deserialize for {record}")));
        assert!(!bridge_source.contains(&format!("impl serde::Serialize for {record}")));
        assert!(!bridge_source.contains(&format!("impl serde::Deserialize for {record}")));
    }
}
