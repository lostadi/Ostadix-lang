use std::path::PathBuf;

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
