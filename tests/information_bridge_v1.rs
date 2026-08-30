use std::collections::{HashMap, HashSet};
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use num_bigint::BigInt;
use o_lang::evidence::{
    admit_execution_v6, analyze_execution_v6, evidence_bundle_sha256_v6, graph_sha256_v2,
    runtime_binding_from_adapter_bytes,
};
use o_lang::execution_contract::Policy;
use o_lang::hgraph::{AdmissionFactKind, HGraph, HNode, HNodeKind, ValueState};
use o_lang::hosted_remote::v2::{
    HostedNodeSignerV2, JournalEntryV2, JournalEventV2, SignedJournalEntryV2,
    HOSTED_JOURNAL_ENTRY_SCHEMA_V2, HOSTED_SIGNED_ENTRY_SCHEMA_V2,
};
use o_lang::information_bridge::{
    project_evidence_v6, project_hgraph_v1, project_hosted_journal_v2, project_logical_hgraph_v1,
    project_parsed_document_v1, project_public_value_v1, project_registry_profile_v1,
    project_world_receipt_v1, EvidenceInformationV1, HGraphInformationV1,
    HostedJournalInformationV1, ParsedDocumentInformationV1, ProjectGraphInformationV1,
    PublicValueInformationV1, RegistryProfileInformationV1, WorldReceiptInformationV1,
    EVIDENCE_INFORMATION_SCHEMA_V1, HGRAPH_INFORMATION_SCHEMA_V1,
    HOSTED_ENTRY_IDENTITY_DIGEST_DOMAIN_V1, HOSTED_JOURNAL_INFORMATION_SCHEMA_V1,
    INFORMATION_BRIDGE_MEDIA_TYPE_V1, MAX_INFORMATION_BRIDGE_DECODE_DEPTH_V1,
    MAX_INFORMATION_BRIDGE_DECODE_ITEMS_V1, MAX_INFORMATION_BRIDGE_RECORD_BYTES_V1,
    PARSED_DOCUMENT_INFORMATION_SCHEMA_V1, PROJECT_GRAPH_INFORMATION_SCHEMA_V1,
    PUBLIC_VALUE_INFORMATION_SCHEMA_V1, REGISTRY_PROFILE_INFORMATION_SCHEMA_V1,
    WORLD_RECEIPT_INFORMATION_SCHEMA_V1,
};
use o_lang::ir::{OIr, OIrProgram, PlanNodeId};
use o_lang::parser::Parser;
use o_lang::placement::{
    EndiannessV1, GenerationV1, NodeProfileV1, PlatformDescriptorV1, SemanticDigestV1,
    TargetCapabilityModelV1, TargetDescriptorV1,
};
use o_lang::project::{self, build_project_hgraph};
use o_lang::registry::{
    append_profile_publication, create_registry_root, registry_public_key_id,
    verify_registry_store, ProfilePublicationV1, ProfileStalenessPolicyV1, RegistryRootPinV1,
    RegistrySignerV1, RegistryStoreV1, RegistryTrustV1, VerifiedRegistryProfileV1,
};
use o_lang::value::{CapabilityKind, FloatFormat, ONumber, OText, OValue};
use o_lang::world::{
    verify_signed_receipt_v1, Ed25519ReceiptSigner, ReceiptKeyResolver, VerifiedExecutionReceiptV1,
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn canonical_wire_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    let mut framed = Vec::new();
    o_lang::wire::write_frame(&mut framed, value).unwrap();
    let declared = u32::from_be_bytes(framed[..4].try_into().unwrap()) as usize;
    assert_eq!(declared, framed.len() - 4);
    framed.split_off(4)
}

fn parsed_document_wire_value(record: &ParsedDocumentInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "source_sha256": record.source_sha256,
        "source_len": record.source_len,
        "syntax_node_count": record.syntax_node_count,
        "plan_origin_count": record.plan_origin_count,
        "plan_origins_sha256": record.plan_origins_sha256,
    })
}

fn public_value_wire_value(record: &PublicValueInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "value_kind": record.value_kind,
        "canonical_sha256": record.canonical_sha256,
        "canonical_len": record.canonical_len,
        "caller_declared_public": record.caller_declared_public,
    })
}

fn hgraph_wire_value(record: &HGraphInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "metadata_projection_sha256": record.metadata_projection_sha256,
        "node_count": record.node_count,
        "constraint_edge_count": record.constraint_edge_count,
        "execution_operation_count": record.execution_operation_count,
        "root_count": record.root_count,
        "sequence_dependency_count": record.sequence_dependency_count,
        "admission_evidence_input_count": record.admission_evidence_input_count,
    })
}

fn evidence_wire_value(record: &EvidenceInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "evidence_schema": record.evidence_schema,
        "analyzer": record.analyzer,
        "metadata_projection_sha256": record.metadata_projection_sha256,
        "backend_catalog_projection_sha256": record.backend_catalog_projection_sha256,
        "node_count": record.node_count,
    })
}

fn registry_profile_wire_value(record: &RegistryProfileInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "namespace": record.namespace,
        "node_identity_sha256": record.node_identity_sha256,
        "profile_generation": record.profile_generation,
        "event_sha256": record.event_sha256,
        "issued_at_ms": record.issued_at_ms,
        "expires_at_ms": record.expires_at_ms,
        "stale": record.stale,
    })
}

fn world_receipt_wire_value(record: &WorldReceiptInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "receipt_sha256": record.receipt_sha256,
        "semantic_sha256": record.semantic_sha256,
        "signature_validated": record.signature_validated,
    })
}

fn project_graph_wire_value(record: &ProjectGraphInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "logical_graph_sha256": record.logical_graph_sha256,
        "source_bundle_sha256": record.source_bundle_sha256,
        "operation_count": record.operation_count,
        "root_count": record.root_count,
    })
}

fn hosted_journal_wire_value(record: &HostedJournalInformationV1) -> Value {
    json!({
        "schema": record.schema,
        "session_identity_sha256": record.session_identity_sha256,
        "sequence": record.sequence,
        "previous_entry_identity_sha256": record.previous_entry_identity_sha256,
        "entry_identity_sha256": record.entry_identity_sha256,
        "recorded_unix_ms": record.recorded_unix_ms,
        "signature_self_consistent": record.signature_self_consistent,
        "signer_trust_evaluated": record.signer_trust_evaluated,
    })
}

fn projected_identity_digest(domain: &str, identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update((identity.len() as u64).to_be_bytes());
    hasher.update(identity.as_bytes());
    hex::encode(hasher.finalize())
}

fn canonical_map_in_order(entries: &[(&str, Value)]) -> Vec<u8> {
    assert!(entries.len() < 24);
    let mut bytes = vec![0xa0 | entries.len() as u8];
    for (key, value) in entries {
        bytes.extend(canonical_wire_bytes(key));
        bytes.extend(canonical_wire_bytes(value));
    }
    bytes
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("canonical fixture contains expected field encoding")
}

fn registry_signer(seed: u8) -> RegistrySignerV1 {
    RegistrySignerV1::from_secret_bytes([seed; 32])
}

fn registry_node_profile(
    signer: &RegistrySignerV1,
    node_id: &str,
    generation: u64,
) -> NodeProfileV1 {
    let issuer =
        SemanticDigestV1::from_sha256(hex::encode(registry_public_key_id(&signer.public_key())))
            .unwrap();
    let descriptor = TargetDescriptorV1::new(
        node_id,
        "information bridge test node",
        GenerationV1::new(1).unwrap(),
        TargetCapabilityModelV1::DownwardClosedIdeal,
        PlatformDescriptorV1::new("linux", "aarch64", "gnu", EndiannessV1::Little, 64).unwrap(),
        [],
        Vec::<String>::new(),
        [],
    )
    .unwrap();
    NodeProfileV1::new(
        issuer,
        descriptor,
        GenerationV1::new(generation).unwrap(),
        o_lang::placement::UnixMillisV1::new(1_400),
        o_lang::placement::UnixMillisV1::new(2_000),
    )
    .unwrap()
}

fn verified_registry_profile_fixture() -> VerifiedRegistryProfileV1 {
    const NAMESPACE: &str = "capability-research/secret-management";
    const NODE_LOCATOR: &str = "https://node.example/private/path";
    let signer = registry_signer(0x21);
    let mut snapshot = create_registry_root(NAMESPACE, 1_000, 10_000, &signer).unwrap();
    append_profile_publication(
        &mut snapshot,
        ProfilePublicationV1::new(
            NAMESPACE,
            NODE_LOCATOR,
            registry_node_profile(&signer, NODE_LOCATOR, 3),
        )
        .unwrap(),
        1_500,
        &signer,
    )
    .unwrap();
    let trust =
        RegistryTrustV1::new([RegistryRootPinV1::new(NAMESPACE, signer.public_key()).unwrap()])
            .unwrap();
    verify_registry_store(
        &RegistryStoreV1::new(snapshot),
        &trust,
        1_600,
        ProfileStalenessPolicyV1::Reject,
    )
    .unwrap()
    .profiles()
    .values()
    .next()
    .unwrap()
    .clone()
}

struct ExactReceiptResolver {
    key_id: [u8; 32],
    public: [u8; 32],
}

impl ReceiptKeyResolver for ExactReceiptResolver {
    fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]> {
        (key_id == &self.key_id).then_some(self.public)
    }
}

fn verified_world_receipt_fixture() -> VerifiedExecutionReceiptV1 {
    let corpus = hex::decode(include_str!("fixtures/world_receipt_v1.hex").trim()).unwrap();
    let total = u32::from_be_bytes(corpus[12..16].try_into().unwrap()) as usize;
    let signer = Ed25519ReceiptSigner::from_secret_bytes([0x42; 32]);
    let resolver = ExactReceiptResolver {
        key_id: signer.key_id(),
        public: signer.public_key_bytes(),
    };
    verify_signed_receipt_v1(&corpus[..total], &resolver).unwrap()
}

fn hosted_journal_fixture() -> o_lang::hosted_remote::v2::SignedJournalEntryV2 {
    HostedNodeSignerV2::from_secret_bytes([0x31; 32])
        .issue_journal_entry(JournalEntryV2 {
            schema: HOSTED_JOURNAL_ENTRY_SCHEMA_V2.to_string(),
            session_id: "private-session:lookup-key".to_string(),
            sequence: 1,
            previous_entry_sha256: None,
            recorded_unix_ms: 1_700_000_000_000,
            event: JournalEventV2::JournalTailRepaired {
                journal_id: "authority-journal".to_string(),
                old_bytes: 4096,
                new_bytes: 2048,
                recovered_head_sha256: Some("ab".repeat(32)),
            },
        })
        .unwrap()
}

fn self_sign_unvalidated_hosted_entry(entry: JournalEntryV2) -> SignedJournalEntryV2 {
    const KEY_ID_DOMAIN: &[u8] = b"OSTADIX/HOSTED-NODE-KEY-ID/V2\0";
    const ENTRY_DIGEST_DOMAIN: &[u8] = b"OSTADIX/HOSTED-JOURNAL-ENTRY/V2\0";
    const SIGNING_DOMAIN: &[u8] = b"OSTADIX/HOSTED-JOURNAL/V2\0";
    let signing_key = SigningKey::from_bytes(&[0x35; 32]);
    let public = signing_key.verifying_key().to_bytes();
    let body = o_lang::hosted_remote::canonical_hosted_bytes(&entry).unwrap();

    let mut entry_digest = Sha256::new();
    entry_digest.update(ENTRY_DIGEST_DOMAIN);
    entry_digest.update((body.len() as u64).to_be_bytes());
    entry_digest.update(&body);

    let mut key_id = Sha256::new();
    key_id.update(KEY_ID_DOMAIN);
    key_id.update(public);

    let mut preimage = Vec::new();
    preimage.extend_from_slice(SIGNING_DOMAIN);
    preimage.extend_from_slice(&(body.len() as u64).to_be_bytes());
    preimage.extend_from_slice(&body);
    SignedJournalEntryV2 {
        schema: HOSTED_SIGNED_ENTRY_SCHEMA_V2.to_string(),
        entry,
        signer_public_key: hex::encode(public),
        signer_key_id: hex::encode(key_id.finalize()),
        entry_sha256: hex::encode(entry_digest.finalize()),
        signature: hex::encode(signing_key.sign(&preimage).to_bytes()),
    }
}

fn project_graph_fixture() -> o_lang::project::LogicalHGraphV1 {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_hgraph");
    let bundle = project::assemble(&fixture, "information-bridge", &[]).unwrap();
    build_project_hgraph(&bundle, Some("main"), None)
        .unwrap()
        .logical_v1()
        .unwrap()
}

macro_rules! assert_projection_roundtrip {
    ($record:expr, $ty:ty, $schema:expr) => {{
        let record = $record;
        let bytes = record.canonical_bytes().unwrap();
        assert_eq!(<$ty>::decode_canonical(&bytes).unwrap(), record);
        let reference = record.native_record_ref().unwrap();
        assert_eq!(reference.schema, $schema);
        assert_eq!(reference.media_type, INFORMATION_BRIDGE_MEDIA_TYPE_V1);
        assert_eq!(reference.logical_len, bytes.len() as u64);
        assert_eq!(reference.sha256, hex::encode(Sha256::digest(&bytes)));
        (record, bytes, reference.sha256)
    }};
}

#[test]
fn parsed_document_projection_is_deterministic_and_binds_exact_source() {
    let source = "let answer = python^(40 + 2)_python";
    let backends = HashSet::from(["python".to_string()]);
    let parsed = Parser::new(source, &backends).parse_with_origins().unwrap();

    let first = project_parsed_document_v1(source.as_bytes(), &parsed).unwrap();
    let second = project_parsed_document_v1(source.as_bytes(), &parsed).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.schema, PARSED_DOCUMENT_INFORMATION_SCHEMA_V1);
    assert_eq!(first.source_len, source.len() as u64);
    assert_eq!(first.plan_origin_count, parsed.plan_origins().len() as u64);
    assert_eq!(parsed.clone().into_nodes(), parsed.nodes());

    let bytes = first.canonical_bytes().unwrap();
    assert_eq!(
        ParsedDocumentInformationV1::decode_canonical(&bytes).unwrap(),
        first
    );
    let native = first.native_record_ref().unwrap();
    assert_eq!(native.schema, PARSED_DOCUMENT_INFORMATION_SCHEMA_V1);
    assert_eq!(native.media_type, INFORMATION_BRIDGE_MEDIA_TYPE_V1);
    assert_eq!(native.logical_len, bytes.len() as u64);

    assert!(
        project_parsed_document_v1("let answer = python^(41 + 1)_python".as_bytes(), &parsed)
            .is_err()
    );
}

#[test]
fn public_value_projection_preflights_numeric_and_text_bounds() {
    let exact_text = OValue::Text {
        v: OText {
            utf8: "x".repeat(o_lang::information_bridge::MAX_PUBLIC_VALUE_TEXT_BYTES_V1),
            encoding: None,
        },
    };
    assert!(project_public_value_v1(&exact_text).is_ok());
    let oversized_text = OValue::Text {
        v: OText {
            utf8: "x".repeat(o_lang::information_bridge::MAX_PUBLIC_VALUE_TEXT_BYTES_V1 + 1),
            encoding: None,
        },
    };
    assert!(project_public_value_v1(&oversized_text).is_err());

    let exact_integer = OValue::Number {
        v: ONumber::Int {
            v: BigInt::from(1_u8)
                << (o_lang::information_bridge::MAX_PUBLIC_VALUE_NUMBER_BYTES_V1 * 8 - 1),
        },
    };
    assert!(project_public_value_v1(&exact_integer).is_ok());

    let huge_integer = OValue::Number {
        v: ONumber::Int {
            v: BigInt::from(1_u8)
                << (o_lang::information_bridge::MAX_PUBLIC_VALUE_NUMBER_BYTES_V1 * 8 + 1),
        },
    };
    assert!(project_public_value_v1(&huge_integer).is_err());

    let zero_denominator = OValue::Number {
        v: ONumber::Rational {
            num: BigInt::from(1),
            den: BigInt::from(0),
        },
    };
    assert!(project_public_value_v1(&zero_denominator).is_err());

    let wrong_float_width = OValue::Number {
        v: ONumber::BinaryFloat {
            format: FloatFormat::F64,
            bits: vec![0; 4],
        },
    };
    assert!(project_public_value_v1(&wrong_float_width).is_err());

    let mut deep = ONumber::Int { v: BigInt::from(1) };
    for _ in 0..=o_lang::information_bridge::MAX_PUBLIC_VALUE_NUMBER_DEPTH_V1 {
        deep = ONumber::Complex {
            re: Box::new(deep),
            im: Box::new(ONumber::Int { v: BigInt::from(0) }),
        };
    }
    assert!(project_public_value_v1(&OValue::Number { v: deep }).is_err());
}

#[test]
fn public_value_projection_is_scalar_only_and_never_retains_content() {
    let public = OValue::Text {
        v: OText {
            utf8: "deliberately-public".to_string(),
            encoding: Some("utf-8".to_string()),
        },
    };
    let projected = project_public_value_v1(&public).unwrap();
    assert_eq!(projected.schema, PUBLIC_VALUE_INFORMATION_SCHEMA_V1);
    assert_eq!(projected.value_kind, "text");
    assert!(projected.caller_declared_public);
    let encoded = projected.canonical_bytes().unwrap();
    assert!(!String::from_utf8_lossy(&encoded).contains("deliberately-public"));

    let capability = OValue::Capability {
        kind: CapabilityKind::NetworkEndpoint,
        identity: "secret-bearer".to_string(),
        metadata: HashMap::new(),
    };
    let error = project_public_value_v1(&capability).unwrap_err();
    assert!(error.to_string().contains("capability"));
    assert!(!error.to_string().contains("secret-bearer"));

    assert!(project_public_value_v1(&OValue::StorePath {
        path: "/nix/store/private-handle".to_string(),
    })
    .is_err());
    assert!(project_public_value_v1(&OValue::Scope {
        bindings: HashMap::new(),
    })
    .is_err());
}

#[test]
fn canonical_projection_decoder_rejects_trailing_and_wrong_schema_bytes() {
    let graph = project_hgraph_v1(&HGraph::default()).unwrap();
    assert_eq!(graph.schema, HGRAPH_INFORMATION_SCHEMA_V1);
    assert_eq!(graph.node_count, 0);
    assert_eq!(graph.execution_operation_count, 0);

    let mut bytes = graph.canonical_bytes().unwrap();
    bytes.push(0);
    assert!(o_lang::information_bridge::HGraphInformationV1::decode_canonical(&bytes).is_err());

    let parsed = ParsedDocumentInformationV1 {
        schema: PUBLIC_VALUE_INFORMATION_SCHEMA_V1.to_string(),
        source_sha256: "00".repeat(32),
        source_len: 0,
        syntax_node_count: 0,
        plan_origin_count: 0,
        plan_origins_sha256: "00".repeat(32),
    };
    assert!(parsed.canonical_bytes().is_err());

    let mut admitted_shape = HGraph::default();
    admitted_shape.add_node(HNode::synthetic(
        HNodeKind::AdmissionEvidence {
            plan_node: PlanNodeId(0),
            fact: AdmissionFactKind::Type,
        },
        ValueState::Materialized,
    ));
    assert_eq!(admitted_shape.admission_evidence_input_count(), 0);
    assert!(admitted_shape.validate_execution_graph().is_ok());
    assert!(admitted_shape.contains_admission_evidence_node());
    assert!(project_hgraph_v1(&admitted_shape).is_err());
}

#[test]
fn production_admitted_execution_graph_is_outside_the_bridge() {
    let backends = HashSet::from(["python".to_string()]);
    let parsed = Parser::new(
        "python^(print('admitted-graph-must-not-project'))_python",
        &backends,
    )
    .parse()
    .unwrap();
    let program = OIrProgram::lower(&parsed);
    let plan = program.plan();
    let mut graph = program.hgraph_for_plan(&plan).unwrap();
    o_lang::hgraph::solve::solve_types(&mut graph).unwrap();
    let runtime =
        runtime_binding_from_adapter_bytes(&plan, &[], &[("bridge-admission-negative", "v1")]);
    let evidence = analyze_execution_v6(&program, &plan, &graph, runtime.clone()).unwrap();
    let admitted =
        admit_execution_v6(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();

    assert!(admitted.graph().admission_evidence_input_count() > 0);
    assert!(project_hgraph_v1(admitted.graph()).is_err());
}

#[test]
fn all_eight_native_projections_roundtrip_and_pin_t2_goldens() {
    let source = "let answer = python^(40 + 2)_python";
    let backends = HashSet::from(["python".to_string()]);
    let parsed = Parser::new(source, &backends).parse_with_origins().unwrap();
    let parsed = project_parsed_document_v1(source.as_bytes(), &parsed).unwrap();

    let public = project_public_value_v1(&OValue::Text {
        v: OText {
            utf8: "caller-public-fixture".to_string(),
            encoding: Some("utf-8".to_string()),
        },
    })
    .unwrap();

    let program = OIrProgram {
        nodes: vec![OIr::Text("upstream-value-not-exported".to_string())],
    };
    let plan = program.plan();
    let mut graph = program.hgraph_for_plan(&plan).unwrap();
    o_lang::hgraph::solve::solve_types(&mut graph).unwrap();
    let hgraph = project_hgraph_v1(&graph).unwrap();
    let runtime = runtime_binding_from_adapter_bytes(
        &plan,
        &[],
        &[("information-bridge-fixture", "runtime-not-exported")],
    );
    let evidence =
        project_evidence_v6(&analyze_execution_v6(&program, &plan, &graph, runtime).unwrap())
            .unwrap();

    let registry = project_registry_profile_v1(&verified_registry_profile_fixture());
    let world = project_world_receipt_v1(&verified_world_receipt_fixture()).unwrap();
    let project = project_logical_hgraph_v1(&project_graph_fixture()).unwrap();
    let hosted = project_hosted_journal_v2(&hosted_journal_fixture()).unwrap();

    let (_, parsed_bytes, parsed_sha) = assert_projection_roundtrip!(
        parsed,
        ParsedDocumentInformationV1,
        PARSED_DOCUMENT_INFORMATION_SCHEMA_V1
    );
    let (_, public_bytes, public_sha) = assert_projection_roundtrip!(
        public,
        PublicValueInformationV1,
        PUBLIC_VALUE_INFORMATION_SCHEMA_V1
    );
    let (hgraph, hgraph_bytes, hgraph_sha) =
        assert_projection_roundtrip!(hgraph, HGraphInformationV1, HGRAPH_INFORMATION_SCHEMA_V1);
    let (evidence, evidence_bytes, evidence_sha) = assert_projection_roundtrip!(
        evidence,
        EvidenceInformationV1,
        EVIDENCE_INFORMATION_SCHEMA_V1
    );
    let (registry, registry_bytes, registry_sha) = assert_projection_roundtrip!(
        registry,
        RegistryProfileInformationV1,
        REGISTRY_PROFILE_INFORMATION_SCHEMA_V1
    );
    let (world, world_bytes, world_sha) = assert_projection_roundtrip!(
        world,
        WorldReceiptInformationV1,
        WORLD_RECEIPT_INFORMATION_SCHEMA_V1
    );
    let (_, project_bytes, project_sha) = assert_projection_roundtrip!(
        project,
        ProjectGraphInformationV1,
        PROJECT_GRAPH_INFORMATION_SCHEMA_V1
    );
    let (hosted, hosted_bytes, hosted_sha) = assert_projection_roundtrip!(
        hosted,
        HostedJournalInformationV1,
        HOSTED_JOURNAL_INFORMATION_SCHEMA_V1
    );

    assert_eq!(
        vec![
            parsed_sha,
            public_sha,
            hgraph_sha,
            evidence_sha,
            registry_sha,
            world_sha,
            project_sha,
            hosted_sha,
        ],
        vec![
            "178c99cd772707d6cdaa83dc0af59223018720e652df1da895445f6774212fff",
            "44a1dec26c1f425305d777b52ae0df902a46f0ad22e1cd4db5f850eba074963c",
            "4898d60361492a7508b5befa5d17d92def15291b803336eeba636629feff4965",
            "aab562e52f0bef68e5e8f855d4a01bb1ee4b8ef29c5567434536860f53d26aba",
            "c19dd8e66807b2d927fc89a85a2c62cfdb187bd41dd3ac4f6a38015f896f8e55",
            "e2df6e5aee888f9d8d44737393bd9a1ccca221658e54f1fe683c9ff99fff2cf1",
            "b5b734e009431c1e1ad63db087a1ba0834aca2cc5af16215b84a73f45967c6fc",
            "32afbe8d94ab044fffbe38e5d2bb121b2b8ff9f0cebe8f34b908c80f3501c829",
        ],
        "canonical/T2 digest changes require a schema review and updated vectors"
    );

    assert!(!String::from_utf8_lossy(&parsed_bytes).contains(source));
    assert!(!String::from_utf8_lossy(&hgraph_bytes).contains("upstream-value-not-exported"));
    assert!(!String::from_utf8_lossy(&evidence_bytes).contains("runtime-not-exported"));
    assert_eq!(registry.namespace, "capability-research/secret-management");
    assert!(!String::from_utf8_lossy(&registry_bytes).contains("https://node.example/private/path"));
    assert!(!String::from_utf8_lossy(&hosted_bytes).contains("private-session:lookup-key"));
    assert!(!String::from_utf8_lossy(&hosted_bytes).contains("authority-journal"));
    assert!(!String::from_utf8_lossy(&hosted_bytes).contains(&"ab".repeat(32)));
    for sentinel in [
        "input.txt",
        "PLAN_VARIANT",
        "PR7_NONEXEC_MARKER",
        "SHOULD_NOT_EXIST",
    ] {
        assert!(!String::from_utf8_lossy(&project_bytes).contains(sentinel));
    }
    for sentinel in ["capability-a", "node-a", "world-a", "resource-busy"] {
        assert!(!String::from_utf8_lossy(&world_bytes).contains(sentinel));
    }
    assert!(world.signature_validated);
    assert!(hosted.signature_self_consistent);
    assert!(!hosted.signer_trust_evaluated);
    assert_eq!(hgraph.admission_evidence_input_count, 0);
    assert_eq!(
        evidence.evidence_schema,
        o_lang::evidence::EVIDENCE_SCHEMA_V6
    );

    assert!(PublicValueInformationV1::decode_canonical(&parsed_bytes).is_err());
    assert!(HGraphInformationV1::decode_canonical(&public_bytes).is_err());
    assert!(EvidenceInformationV1::decode_canonical(&hgraph_bytes).is_err());
    assert!(RegistryProfileInformationV1::decode_canonical(&evidence_bytes).is_err());
    assert!(WorldReceiptInformationV1::decode_canonical(&registry_bytes).is_err());
    assert!(ProjectGraphInformationV1::decode_canonical(&world_bytes).is_err());
    assert!(HostedJournalInformationV1::decode_canonical(&project_bytes).is_err());
    assert!(ParsedDocumentInformationV1::decode_canonical(&hosted_bytes).is_err());
}

#[test]
fn metadata_projection_digests_intentionally_ignore_native_value_source_and_runtime_identity() {
    let build = |value: &str, runtime_marker: &'static str| {
        let program = OIrProgram {
            nodes: vec![OIr::Text(value.to_string())],
        };
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).unwrap();
        o_lang::hgraph::solve::solve_types(&mut graph).unwrap();
        let runtime = runtime_binding_from_adapter_bytes(
            &plan,
            &[],
            &[("information-bridge-omitted-runtime", runtime_marker)],
        );
        let evidence = analyze_execution_v6(&program, &plan, &graph, runtime).unwrap();
        (graph, evidence)
    };

    let (left_graph, left_evidence) = build("private-upstream-alpha", "runtime-alpha");
    let (right_graph, right_evidence) = build("private-upstream-bravo", "runtime-bravo");
    assert_ne!(graph_sha256_v2(&left_graph), graph_sha256_v2(&right_graph));
    assert_ne!(
        evidence_bundle_sha256_v6(&left_evidence),
        evidence_bundle_sha256_v6(&right_evidence)
    );

    let left_graph_projection = project_hgraph_v1(&left_graph).unwrap();
    let right_graph_projection = project_hgraph_v1(&right_graph).unwrap();
    assert_eq!(left_graph_projection, right_graph_projection);
    let left_evidence_projection = project_evidence_v6(&left_evidence).unwrap();
    let right_evidence_projection = project_evidence_v6(&right_evidence).unwrap();
    assert_eq!(left_evidence_projection, right_evidence_projection);

    for sentinel in [
        "private-upstream-alpha",
        "private-upstream-bravo",
        "runtime-alpha",
        "runtime-bravo",
    ] {
        assert!(
            !String::from_utf8_lossy(&left_graph_projection.canonical_bytes().unwrap())
                .contains(sentinel)
        );
        assert!(
            !String::from_utf8_lossy(&left_evidence_projection.canonical_bytes().unwrap())
                .contains(sentinel)
        );
    }
}

#[test]
fn manual_records_cannot_bypass_projection_semantic_validation() {
    macro_rules! assert_invalid {
        ($record:expr, $ty:ty, $wire_value:ident) => {{
            let record = $record;
            assert!(record.canonical_bytes().is_err());
            let wire_value = $wire_value(&record);
            assert!(<$ty>::decode_canonical(&canonical_wire_bytes(&wire_value)).is_err());
        }};
    }

    assert_invalid!(
        ParsedDocumentInformationV1 {
            schema: PARSED_DOCUMENT_INFORMATION_SCHEMA_V1.to_string(),
            source_sha256: "A0".repeat(32),
            source_len: 0,
            syntax_node_count: 0,
            plan_origin_count: 0,
            plan_origins_sha256: "00".repeat(32),
        },
        ParsedDocumentInformationV1,
        parsed_document_wire_value
    );
    assert_invalid!(
        PublicValueInformationV1 {
            schema: PUBLIC_VALUE_INFORMATION_SCHEMA_V1.to_string(),
            value_kind: "capability".to_string(),
            canonical_sha256: "00".repeat(32),
            canonical_len: 0,
            caller_declared_public: false,
        },
        PublicValueInformationV1,
        public_value_wire_value
    );

    let mut invalid_hgraph = project_hgraph_v1(&HGraph::default()).unwrap();
    invalid_hgraph.admission_evidence_input_count = 1;
    assert_invalid!(invalid_hgraph, HGraphInformationV1, hgraph_wire_value);

    let program = OIrProgram {
        nodes: vec![OIr::Text("evidence-invalid-fixture".to_string())],
    };
    let plan = program.plan();
    let mut graph = program.hgraph_for_plan(&plan).unwrap();
    o_lang::hgraph::solve::solve_types(&mut graph).unwrap();
    let runtime = runtime_binding_from_adapter_bytes(&plan, &[], &[("bridge-invalid", "v1")]);
    let mut invalid_evidence =
        project_evidence_v6(&analyze_execution_v6(&program, &plan, &graph, runtime).unwrap())
            .unwrap();
    invalid_evidence.analyzer = "caller-claimed-analyzer".to_string();
    assert_invalid!(invalid_evidence, EvidenceInformationV1, evidence_wire_value);

    let valid_registry = project_registry_profile_v1(&verified_registry_profile_fixture());
    for invalid_namespace in [
        "https://registry.example/path",
        "control\u{0007}namespace",
        "a//b",
        "a/../b",
    ] {
        let mut invalid_registry = valid_registry.clone();
        invalid_registry.namespace = invalid_namespace.to_string();
        assert_invalid!(
            invalid_registry,
            RegistryProfileInformationV1,
            registry_profile_wire_value
        );
    }
    let mut zero_validity = valid_registry;
    zero_validity.expires_at_ms = zero_validity.issued_at_ms;
    assert_invalid!(
        zero_validity,
        RegistryProfileInformationV1,
        registry_profile_wire_value
    );

    let mut invalid_world = project_world_receipt_v1(&verified_world_receipt_fixture()).unwrap();
    invalid_world.signature_validated = false;
    assert_invalid!(
        invalid_world,
        WorldReceiptInformationV1,
        world_receipt_wire_value
    );

    let mut invalid_project = project_logical_hgraph_v1(&project_graph_fixture()).unwrap();
    invalid_project.root_count = invalid_project.operation_count + 1;
    assert_invalid!(
        invalid_project,
        ProjectGraphInformationV1,
        project_graph_wire_value
    );

    let valid_hosted = project_hosted_journal_v2(&hosted_journal_fixture()).unwrap();
    let mut zero_sequence = valid_hosted.clone();
    zero_sequence.sequence = 0;
    assert_invalid!(
        zero_sequence,
        HostedJournalInformationV1,
        hosted_journal_wire_value
    );
    let mut claimed_trust = valid_hosted;
    claimed_trust.signer_trust_evaluated = true;
    assert_invalid!(
        claimed_trust,
        HostedJournalInformationV1,
        hosted_journal_wire_value
    );
}

#[test]
fn hosted_projection_rejects_self_signed_but_semantically_invalid_entries() {
    let mut zero_sequence = hosted_journal_fixture().entry;
    zero_sequence.sequence = 0;
    let zero_sequence = self_sign_unvalidated_hosted_entry(zero_sequence);
    zero_sequence
        .verify()
        .expect("native self-signature verification intentionally proves no sequence policy");
    assert!(project_hosted_journal_v2(&zero_sequence).is_err());

    let mut malformed_previous = hosted_journal_fixture().entry;
    malformed_previous.previous_entry_sha256 = Some("not-a-digest".to_string());
    let malformed_previous = self_sign_unvalidated_hosted_entry(malformed_previous);
    malformed_previous
        .verify()
        .expect("native self-signature verification intentionally proves no continuity policy");
    assert!(project_hosted_journal_v2(&malformed_previous).is_err());

    let attacker_token = "de".repeat(32);
    let mut attacker_chosen_previous = hosted_journal_fixture().entry;
    attacker_chosen_previous.previous_entry_sha256 = Some(attacker_token.clone());
    let attacker_chosen_previous = self_sign_unvalidated_hosted_entry(attacker_chosen_previous);
    attacker_chosen_previous.verify().unwrap();
    let projected = project_hosted_journal_v2(&attacker_chosen_previous).unwrap();
    let expected_previous =
        projected_identity_digest(HOSTED_ENTRY_IDENTITY_DIGEST_DOMAIN_V1, &attacker_token);
    assert_eq!(
        projected.previous_entry_identity_sha256.as_deref(),
        Some(expected_previous.as_str())
    );
    let projected_bytes = projected.canonical_bytes().unwrap();
    assert!(!String::from_utf8_lossy(&projected_bytes).contains(&attacker_token));
    assert!(
        !String::from_utf8_lossy(&projected_bytes).contains(&attacker_chosen_previous.entry_sha256)
    );
}

#[test]
fn decoders_enforce_byte_item_and_depth_limits_before_record_lifting() {
    let too_large = vec![0_u8; MAX_INFORMATION_BRIDGE_RECORD_BYTES_V1 + 1];
    assert!(ParsedDocumentInformationV1::decode_canonical(&too_large).is_err());

    let too_many_items = Value::Array(vec![Value::Null; MAX_INFORMATION_BRIDGE_DECODE_ITEMS_V1]);
    assert!(
        ParsedDocumentInformationV1::decode_canonical(&canonical_wire_bytes(&too_many_items))
            .is_err()
    );

    let mut too_deep = Value::Null;
    for _ in 0..=MAX_INFORMATION_BRIDGE_DECODE_DEPTH_V1 {
        too_deep = Value::Array(vec![too_deep]);
    }
    assert!(
        ParsedDocumentInformationV1::decode_canonical(&canonical_wire_bytes(&too_deep)).is_err()
    );
}

#[test]
fn decoder_rejects_noncanonical_and_malformed_cbor_forms() {
    let world = WorldReceiptInformationV1 {
        schema: WORLD_RECEIPT_INFORMATION_SCHEMA_V1.to_string(),
        receipt_sha256: "11".repeat(32),
        semantic_sha256: "22".repeat(32),
        signature_validated: true,
    };

    let mut unknown = world_receipt_wire_value(&world);
    unknown
        .as_object_mut()
        .unwrap()
        .insert("authority".to_string(), Value::Bool(true));
    assert!(WorldReceiptInformationV1::decode_canonical(&canonical_wire_bytes(&unknown)).is_err());

    let mut duplicate = world.canonical_bytes().unwrap();
    assert_eq!(duplicate[0], 0xa4);
    duplicate[0] = 0xa5;
    duplicate.extend(canonical_wire_bytes(&"schema"));
    duplicate.extend(canonical_wire_bytes(&world.schema));
    assert!(WorldReceiptInformationV1::decode_canonical(&duplicate).is_err());

    let reversed_map = canonical_map_in_order(&[
        ("signature_validated", Value::Bool(true)),
        ("semantic_sha256", Value::String("22".repeat(32))),
        ("receipt_sha256", Value::String("11".repeat(32))),
        (
            "schema",
            Value::String(WORLD_RECEIPT_INFORMATION_SCHEMA_V1.to_string()),
        ),
    ]);
    assert!(WorldReceiptInformationV1::decode_canonical(&reversed_map).is_err());

    let hosted = HostedJournalInformationV1 {
        schema: HOSTED_JOURNAL_INFORMATION_SCHEMA_V1.to_string(),
        session_identity_sha256: "33".repeat(32),
        sequence: 1,
        previous_entry_identity_sha256: None,
        entry_identity_sha256: "44".repeat(32),
        recorded_unix_ms: 1,
        signature_self_consistent: true,
        signer_trust_evaluated: false,
    };
    let mut nonminimal_integer = hosted.canonical_bytes().unwrap();
    let mut sequence_one = canonical_wire_bytes(&"sequence");
    sequence_one.push(1);
    let at = find_subslice(&nonminimal_integer, &sequence_one) + sequence_one.len() - 1;
    nonminimal_integer.splice(at..=at, [0x18, 0x01]);
    assert!(HostedJournalInformationV1::decode_canonical(&nonminimal_integer).is_err());

    let mut nonminimal_length = hosted.canonical_bytes().unwrap();
    let schema_value = canonical_wire_bytes(&hosted.schema);
    assert_eq!(schema_value[0], 0x78);
    let at = find_subslice(&nonminimal_length, &schema_value);
    nonminimal_length.splice(at..at + 2, [0x79, 0x00, schema_value[1]]);
    assert!(HostedJournalInformationV1::decode_canonical(&nonminimal_length).is_err());

    assert!(WorldReceiptInformationV1::decode_canonical(&[0xbf, 0xff]).is_err());
    assert!(WorldReceiptInformationV1::decode_canonical(&[0xfa, 0x3f, 0x80, 0, 0]).is_err());
    assert!(WorldReceiptInformationV1::decode_canonical(&[0x61, 0xff]).is_err());
}

#[test]
fn decoded_world_and_hosted_flags_remain_untrusted_descriptive_metadata() {
    let supplier_world = WorldReceiptInformationV1 {
        schema: WORLD_RECEIPT_INFORMATION_SCHEMA_V1.to_string(),
        receipt_sha256: "11".repeat(32),
        semantic_sha256: "22".repeat(32),
        signature_validated: true,
    };
    assert_eq!(
        WorldReceiptInformationV1::decode_canonical(&supplier_world.canonical_bytes().unwrap())
            .unwrap(),
        supplier_world
    );

    let supplier_hosted = HostedJournalInformationV1 {
        schema: HOSTED_JOURNAL_INFORMATION_SCHEMA_V1.to_string(),
        session_identity_sha256: "33".repeat(32),
        sequence: 1,
        previous_entry_identity_sha256: None,
        entry_identity_sha256: "44".repeat(32),
        recorded_unix_ms: 1,
        signature_self_consistent: true,
        signer_trust_evaluated: false,
    };
    assert_eq!(
        HostedJournalInformationV1::decode_canonical(&supplier_hosted.canonical_bytes().unwrap())
            .unwrap(),
        supplier_hosted
    );
}
