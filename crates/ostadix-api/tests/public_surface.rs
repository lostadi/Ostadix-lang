use std::path::PathBuf;

use ostadix_api::{
    BackendAuthority, BigInt, CapabilityKind, DecimalSpecial, FloatFormat, FloatSpecial, GraphNode,
    GroupMode, NativeBoundary, NativeCodecSafety, NativeIdentity, NodeId, OBytes, OKeyword,
    ONative, ONumber, OSymbol, OText, OValue, RehydratePolicy, RequestKind, Runtime,
    RuntimeBoundary, RuntimeStage, SeqKind, SetKind, SnapshotKind,
};

fn assert_public<T>() {}

#[test]
fn complete_ovalue_payload_vocabulary_is_nameable_from_the_facade() {
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
fn facade_source_has_no_glob_or_public_evaluator_reexport() {
    let source = include_str!("../src/lib.rs");
    assert!(!source.contains("pub use o_lang::api::*"));
    assert!(!source.lines().any(|line| {
        let line = line.trim();
        line.starts_with("pub use") && line.contains("Evaluator")
    }));
    assert!(!source.lines().any(|line| {
        let line = line.trim();
        line.starts_with("pub use") && line.contains("BackendRegistry")
    }));
    for forbidden in [
        "information_bridge",
        "ParsedDocumentV1",
        "ParsedDocumentInformationV1",
        "PublicValueInformationV1",
        "HGraphInformationV1",
        "EvidenceInformationV1",
        "RegistryProfileInformationV1",
        "WorldReceiptInformationV1",
        "ProjectGraphInformationV1",
        "HostedJournalInformationV1",
        "InformationBridgeErrorV1",
        "NativeRecordRefV1",
        "project_parsed_document_v1",
        "project_public_value_v1",
        "project_hgraph_v1",
        "project_evidence_v6",
        "project_registry_profile_v1",
        "project_world_receipt_v1",
        "project_logical_hgraph_v1",
        "project_hosted_journal_v2",
    ] {
        assert!(
            !source.contains(forbidden),
            "stable facade must not reexport experimental bridge symbol {forbidden}"
        );
    }
}
