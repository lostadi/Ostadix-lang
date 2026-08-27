#[cfg(test)]
mod authority_tests {
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    use crate::execution_fabric::{
        encode_execution_candidate_v1, encode_execution_capsule_v1, AttemptIdV1,
        CandidateOutcomeV1, CandidateOutputV1, ExecutionCandidateV1, ExecutionCapsuleV1,
        ExecutionIdV1, ExecutionLimitsV1, InputBindingV1, InputManifestV1, LogicalTaskIdV1,
        OutputContractV1, OutputFidelityV1, OutputValueKindV1, RendererPartV1, Sha256DigestV1,
        SourceClosedRendererV1, TrustedInlineRendererV1, MAX_EXECUTION_CANDIDATE_BYTES,
        MAX_EXECUTION_CAPSULE_BYTES,
    };
    use crate::placement_protocol::{GenerationV1, SemanticDigestV1, UnixMillisV1};
    use crate::value::OText;
    use crate::world::{PortableOValue, PortableValueRecord, MAX_OVALUE_RECORD_BYTES};

    use crate::execution_fabric_authority::*;

    const NODE_ID: &str = "fabric-node-a";
    const NODE_GENERATION: u64 = 7;
    const CELL_INCARNATION: u64 = 11;
    const CAPSULE_DEADLINE_UNIX_MS: u64 = 2_000_000_000_000;
    const LEASE_ISSUED_UNIX_MS: u64 = CAPSULE_DEADLINE_UNIX_MS - 30_000;
    const LEASE_EXPIRES_UNIX_MS: u64 = CAPSULE_DEADLINE_UNIX_MS;
    const CANDIDATE_COMPLETED_UNIX_MS: u64 = CAPSULE_DEADLINE_UNIX_MS - 1;

    const REQUEST_HEADER_KAT_V1: (usize, &str) = (
        3243,
        "badd2d7c566b1b92970b73e45c1dfc2c0ec8a6967ae081f3ffa679c61067e4ab",
    );
    const RESPONSE_HEADER_KAT_V1: (usize, &str) = (
        1767,
        "a988ab61deee82f4972b2facf29b5eb060e22b5c235bd432ac322be6144478ed",
    );

    struct Fixture {
        authority_key: FabricSigningKeyV1,
        node_key: FabricSigningKeyV1,
        capsule_bytes: Vec<u8>,
        submission: FabricSubmissionV1,
        candidate_bytes: Vec<u8>,
        terminal: FabricTerminalCandidateV1,
    }

    fn digest(seed: u8) -> Sha256DigestV1 {
        [seed; 32]
    }

    fn semantic_digest(seed: u8) -> SemanticDigestV1 {
        SemanticDigestV1::from_sha256(hex::encode(digest(seed))).unwrap()
    }

    fn fixture_capsule() -> ExecutionCapsuleV1 {
        let execution = ExecutionIdV1::new(digest(1)).unwrap();
        let task = LogicalTaskIdV1::new(execution, digest(2)).unwrap();
        let attempt = AttemptIdV1::new(task, 1).unwrap();
        let input = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "world".to_string(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
        );
        let inputs =
            InputManifestV1::new(vec![InputBindingV1::new("name", &input).unwrap()]).unwrap();
        let region = SourceClosedRendererV1::new(
            TrustedInlineRendererV1::Text,
            vec![
                RendererPartV1::literal("hello "),
                RendererPartV1::input("name"),
            ],
            digest(3),
            digest(4),
            digest(5),
            digest(6),
        )
        .unwrap();
        let output = OutputContractV1::for_renderer(
            "result",
            TrustedInlineRendererV1::Text,
            MAX_OVALUE_RECORD_BYTES,
        )
        .unwrap();
        ExecutionCapsuleV1::new(
            attempt,
            region,
            digest(7),
            inputs,
            output,
            CAPSULE_DEADLINE_UNIX_MS,
            ExecutionLimitsV1::new(30_000, 16 * 1024, MAX_OVALUE_RECORD_BYTES).unwrap(),
        )
        .unwrap()
    }

    fn fixture_target() -> FabricTargetBindingV1 {
        FabricTargetBindingV1::new(
            semantic_digest(20),
            NODE_ID,
            GenerationV1::new(NODE_GENERATION).unwrap(),
            ExecutionCellIncarnationV1::new(CELL_INCARNATION).unwrap(),
            semantic_digest(21),
            GenerationV1::new(8).unwrap(),
            GenerationV1::new(9).unwrap(),
            semantic_digest(22),
            semantic_digest(23),
            semantic_digest(24),
            semantic_digest(25),
            semantic_digest(26),
            semantic_digest(27),
            semantic_digest(28),
        )
        .unwrap()
    }

    fn fixture_candidate(capsule: &ExecutionCapsuleV1) -> ExecutionCandidateV1 {
        let output = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "hello world".to_string(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
        );
        ExecutionCandidateV1::new(
            capsule,
            CandidateOutcomeV1::Succeeded {
                output: CandidateOutputV1::new(
                    "result",
                    &output,
                    OutputValueKindV1::Text,
                    OutputFidelityV1::Structural,
                )
                .unwrap(),
            },
            CANDIDATE_COMPLETED_UNIX_MS,
        )
        .unwrap()
    }

    fn fixture() -> Fixture {
        let authority_key = FabricSigningKeyV1::from_secret_bytes([0x11; 32]);
        let node_key = FabricSigningKeyV1::from_secret_bytes([0x22; 32]);
        let capsule = fixture_capsule();
        let capsule_bytes = encode_execution_capsule_v1(&capsule).unwrap();
        let source_closure = FabricSourceClosureV1::new(
            FABRIC_SOURCE_CLOSURE_DIALECT_V1,
            "main = render(name)",
            FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
            "eager",
            digest(10),
            digest(3),
            digest(4),
        )
        .unwrap();
        let lease = PlacementLeaseV3::new(
            authority_key.key_id_digest(),
            semantic_digest(29),
            fixture_target(),
            &source_closure,
            &capsule,
            UnixMillisV1::new(LEASE_ISSUED_UNIX_MS),
            UnixMillisV1::new(LEASE_EXPIRES_UNIX_MS),
        )
        .unwrap();
        let signed_lease = authority_key.sign_execution_lease(lease).unwrap();
        let submission =
            FabricSubmissionV1::new(signed_lease, source_closure, capsule_bytes.clone()).unwrap();
        let candidate_bytes = encode_execution_candidate_v1(&fixture_candidate(&capsule)).unwrap();
        let terminal = node_key
            .sign_terminal_candidate(&submission, candidate_bytes.clone(), 25)
            .unwrap();
        Fixture {
            authority_key,
            node_key,
            capsule_bytes,
            submission,
            candidate_bytes,
            terminal,
        }
    }

    fn value_field_mut<'a>(value: &'a mut Value, path: &[&str]) -> &'a mut Value {
        let mut current = value;
        for field in path {
            current = current
                .get_mut(*field)
                .unwrap_or_else(|| panic!("fixture omitted `{field}` in path {path:?}"));
        }
        current
    }

    fn corrupt_digest(value: &mut Value) {
        let bytes = value.as_array_mut().expect("digest is encoded as bytes");
        let first = bytes[0].as_u64().expect("digest byte is an integer");
        bytes[0] = Value::from(if first == 0xff { 0xfe } else { first + 1 });
    }

    fn corrupt_lower_hex(value: &str) -> String {
        let mut bytes = value.as_bytes().to_vec();
        bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
        String::from_utf8(bytes).unwrap()
    }

    fn assert_header_kat(label: &str, bytes: &[u8], expected: (usize, &str)) {
        let actual_sha256 = hex::encode(Sha256::digest(bytes));
        let (expected_length, expected_sha256) = expected;
        assert_eq!(bytes.len(), expected_length, "{label} length KAT drifted");
        assert_eq!(
            actual_sha256, expected_sha256,
            "{label} SHA-256 KAT drifted"
        );
    }

    #[test]
    fn request_and_terminal_response_round_trip_without_changing_m2_payload_kats() {
        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission.clone());
        let encoded_request = encode_fabric_request_v1(&request).unwrap();
        let decoded_request = decode_fabric_request_v1(
            encoded_request.header_bytes(),
            encoded_request.payload_bytes(),
        )
        .unwrap();
        assert_eq!(decoded_request, request);

        let response = FabricResponseV1::TerminalCandidate(fixture.terminal.clone());
        let encoded_response = encode_fabric_response_v1(&response).unwrap();
        let decoded_response = decode_fabric_response_v1(
            encoded_response.header_bytes(),
            encoded_response.payload_bytes(),
        )
        .unwrap();
        assert_eq!(decoded_response, response);

        assert_eq!(fixture.capsule_bytes.len(), 1367);
        assert_eq!(
            hex::encode(Sha256::digest(&fixture.capsule_bytes)),
            "89347e0f8d438e641aab20f3fed04560671559a5045b3c36f277873a3d89a1dd"
        );
        assert_eq!(fixture.candidate_bytes.len(), 759);
        assert_eq!(
            hex::encode(Sha256::digest(&fixture.candidate_bytes)),
            "8393adf9db63b6ba923fb0319fe0fa9475a7bc0572c75ca2682331c537f2088f"
        );
        assert_header_kat(
            "Fabric request header V1",
            encoded_request.header_bytes(),
            REQUEST_HEADER_KAT_V1,
        );
        assert_header_kat(
            "Fabric response header V1",
            encoded_response.header_bytes(),
            RESPONSE_HEADER_KAT_V1,
        );
    }

    #[test]
    fn source_closure_accepts_only_the_frozen_dialect_root_and_policy_vocabulary() {
        let build = |dialect, root_operation, base_policy| {
            FabricSourceClosureV1::new(
                dialect,
                "main = render(name)",
                root_operation,
                base_policy,
                digest(10),
                digest(3),
                digest(4),
            )
        };

        assert!(build(
            FABRIC_SOURCE_CLOSURE_DIALECT_V1,
            FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
            "eager"
        )
        .is_ok());
        assert!(build("o-v1", FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1, "eager").is_err());
        assert!(build(FABRIC_SOURCE_CLOSURE_DIALECT_V1, 1, "eager").is_err());
        assert!(build(
            FABRIC_SOURCE_CLOSURE_DIALECT_V1,
            FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
            "standard-v1"
        )
        .is_err());
    }

    #[test]
    fn pinned_node_signature_cannot_override_the_frozen_m2_region_binding() {
        let fixture = fixture();
        let mut malicious_candidate =
            crate::execution_fabric::decode_execution_candidate_v1(&fixture.candidate_bytes)
                .unwrap();
        malicious_candidate.region_sha256 = digest(0x99);
        let malicious_bytes = encode_execution_candidate_v1(&malicious_candidate).unwrap();

        let mut receipt_value =
            serde_json::to_value(fixture.terminal.signed_receipt().receipt()).unwrap();
        *value_field_mut(&mut receipt_value, &["candidate_payload", "sha256"]) =
            serde_json::to_value(<[u8; 32]>::from(Sha256::digest(&malicious_bytes))).unwrap();
        *value_field_mut(&mut receipt_value, &["candidate_payload", "byte_length"]) =
            Value::from(malicious_bytes.len() as u64);
        let receipt: TerminalCandidateReceiptV1 = serde_json::from_value(receipt_value).unwrap();
        let receipt_bytes = crate::canonical_cbor::encode(&receipt).unwrap();
        let preimage = crate::canonical_cbor::signing_preimage(
            FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1,
            &receipt_bytes,
        )
        .unwrap();
        let signing_key = SigningKey::from_bytes(&[0x22; 32]);
        let signed = SignedTerminalCandidateReceiptV1 {
            schema: FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1.to_string(),
            receipt,
            signer_public_key: hex::encode(signing_key.verifying_key().to_bytes()),
            signer_key_id: fixture.node_key.key_id_hex(),
            signature: hex::encode(signing_key.sign(&preimage).to_bytes()),
        };
        let malicious_terminal =
            FabricTerminalCandidateV1::from_wire(signed, malicious_bytes).unwrap();
        let target = fixture.submission.header().lease().lease().target();
        let pinned = PinnedFabricNodeKeyV1::new(
            target.node_id(),
            target.node_generation(),
            target.execution_cell_incarnation(),
            fixture.node_key.public_key(),
        )
        .unwrap();

        assert!(pinned
            .verify_terminal_candidate(&malicious_terminal, &fixture.submission)
            .unwrap_err()
            .to_string()
            .contains("candidate binding"));
    }

    #[test]
    fn pinned_node_signature_authentication_leaves_semantic_bindings_for_later_gates() {
        let fixture = fixture();
        let mut receipt_value =
            serde_json::to_value(fixture.terminal.signed_receipt().receipt()).unwrap();
        *value_field_mut(&mut receipt_value, &["node_generation"]) =
            Value::from(NODE_GENERATION + 1);
        let receipt: TerminalCandidateReceiptV1 = serde_json::from_value(receipt_value).unwrap();
        let receipt_bytes = crate::canonical_cbor::encode(&receipt).unwrap();
        let preimage = crate::canonical_cbor::signing_preimage(
            FABRIC_TERMINAL_RECEIPT_SIGNING_DOMAIN_V1,
            &receipt_bytes,
        )
        .unwrap();
        let signing_key = SigningKey::from_bytes(&[0x22; 32]);
        let signed = SignedTerminalCandidateReceiptV1 {
            schema: FABRIC_SIGNED_TERMINAL_RECEIPT_SCHEMA_V1.to_string(),
            receipt,
            signer_public_key: hex::encode(signing_key.verifying_key().to_bytes()),
            signer_key_id: fixture.node_key.key_id_hex(),
            signature: hex::encode(signing_key.sign(&preimage).to_bytes()),
        };
        let target = fixture.submission.header().lease().lease().target();
        let pinned = PinnedFabricNodeKeyV1::new(
            target.node_id(),
            target.node_generation(),
            target.execution_cell_incarnation(),
            fixture.node_key.public_key(),
        )
        .unwrap();

        assert_eq!(pinned.node_id(), target.node_id());
        assert_eq!(pinned.node_generation(), target.node_generation());
        assert_eq!(
            pinned.execution_cell_incarnation(),
            target.execution_cell_incarnation()
        );
        assert_eq!(pinned.public_key(), fixture.node_key.public_key());
        assert_eq!(pinned.key_id(), fixture.node_key.key_id_hex());
        pinned
            .authenticate_terminal_receipt(&signed)
            .expect("a valid pinned-node signature is independent of later generation gates");

        let terminal =
            FabricTerminalCandidateV1::from_wire(signed, fixture.candidate_bytes).unwrap();
        assert!(pinned
            .verify_terminal_candidate(&terminal, &fixture.submission)
            .unwrap_err()
            .to_string()
            .contains("node/generation"));
    }

    #[test]
    fn pinned_node_signature_authentication_rejects_wrong_key_and_signature() {
        let fixture = fixture();
        let target = fixture.submission.header().lease().lease().target();
        let wrong_key = FabricSigningKeyV1::from_secret_bytes([0x33; 32]);
        let wrong_pin = PinnedFabricNodeKeyV1::new(
            target.node_id(),
            target.node_generation(),
            target.execution_cell_incarnation(),
            wrong_key.public_key(),
        )
        .unwrap();
        assert!(wrong_pin
            .authenticate_terminal_receipt(fixture.terminal.signed_receipt())
            .unwrap_err()
            .to_string()
            .contains("pinned node key"));

        let pinned = PinnedFabricNodeKeyV1::new(
            target.node_id(),
            target.node_generation(),
            target.execution_cell_incarnation(),
            fixture.node_key.public_key(),
        )
        .unwrap();
        let mut invalid_signature = fixture.terminal.signed_receipt().clone();
        invalid_signature.signature = corrupt_lower_hex(invalid_signature.signature());
        assert!(matches!(
            pinned.authenticate_terminal_receipt(&invalid_signature),
            Err(FabricAuthorityError::Signature(_))
        ));
    }

    #[test]
    fn canonical_decoder_rejects_trailing_nonminimal_duplicate_deep_and_oversized_headers() {
        let fixture = fixture();
        let encoded =
            encode_fabric_request_v1(&FabricRequestV1::SubmitPureAttempt(fixture.submission))
                .unwrap();
        let header = encoded.header_bytes();

        let mut trailing = header.to_vec();
        trailing.push(0);
        assert!(decode_fabric_request_v1(&trailing, encoded.payload_bytes())
            .unwrap_err()
            .to_string()
            .contains("trailing"));

        assert_eq!(header[0], 0xa2, "request envelope remains a two-field map");
        let mut nonminimal = Vec::with_capacity(header.len() + 1);
        nonminimal.extend_from_slice(&[0xb8, 0x02]);
        nonminimal.extend_from_slice(&header[1..]);
        assert!(matches!(
            decode_fabric_request_v1(&nonminimal, encoded.payload_bytes()),
            Err(FabricAuthorityError::NonCanonical {
                kind: "request header"
            })
        ));

        let mut duplicate = header.to_vec();
        duplicate[0] = 0xa3;
        duplicate.extend_from_slice(&crate::canonical_cbor::encode(&"schema").unwrap());
        duplicate
            .extend_from_slice(&crate::canonical_cbor::encode(&FABRIC_REQUEST_SCHEMA_V1).unwrap());
        assert!(matches!(
            decode_fabric_request_v1(&duplicate, encoded.payload_bytes()),
            Err(FabricAuthorityError::NonCanonical {
                kind: "request header"
            })
        ));

        let mut too_deep = vec![0x81; 66];
        too_deep.push(0xf6);
        assert!(decode_fabric_request_v1(&too_deep, None)
            .unwrap_err()
            .to_string()
            .contains("nesting depth"));

        let oversized = vec![0; MAX_FABRIC_HEADER_BYTES + 1];
        assert!(matches!(
            decode_fabric_request_v1(&oversized, None),
            Err(FabricAuthorityError::RecordTooLarge {
                kind: "request header",
                ..
            })
        ));
    }

    #[test]
    fn canonical_decoder_rejects_unknown_tags_fields_and_oversized_payloads() {
        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission.clone());
        let encoded_request = encode_fabric_request_v1(&request).unwrap();

        let mut unknown_field: Value =
            crate::canonical_cbor::decode(encoded_request.header_bytes()).unwrap();
        unknown_field
            .as_object_mut()
            .unwrap()
            .insert("future-field".to_string(), Value::Bool(true));
        let unknown_field = crate::canonical_cbor::encode(&unknown_field).unwrap();
        assert!(decode_fabric_request_v1(&unknown_field, encoded_request.payload_bytes()).is_err());

        let mut unknown_request_tag: Value =
            crate::canonical_cbor::decode(encoded_request.header_bytes()).unwrap();
        *value_field_mut(&mut unknown_request_tag, &["request", "command"]) =
            Value::String("future-command".to_string());
        let unknown_request_tag = crate::canonical_cbor::encode(&unknown_request_tag).unwrap();
        assert!(
            decode_fabric_request_v1(&unknown_request_tag, encoded_request.payload_bytes())
                .is_err()
        );

        let response = FabricResponseV1::TerminalCandidate(fixture.terminal);
        let encoded_response = encode_fabric_response_v1(&response).unwrap();
        let mut unknown_response_tag: Value =
            crate::canonical_cbor::decode(encoded_response.header_bytes()).unwrap();
        *value_field_mut(&mut unknown_response_tag, &["response", "status"]) =
            Value::String("future-status".to_string());
        let unknown_response_tag = crate::canonical_cbor::encode(&unknown_response_tag).unwrap();
        assert!(
            decode_fabric_response_v1(&unknown_response_tag, encoded_response.payload_bytes())
                .is_err()
        );

        let oversized_capsule = vec![0; MAX_EXECUTION_CAPSULE_BYTES + 1];
        assert!(matches!(
            decode_fabric_request_v1(encoded_request.header_bytes(), Some(&oversized_capsule)),
            Err(FabricAuthorityError::RecordTooLarge {
                kind: "capsule",
                ..
            })
        ));
        let oversized_candidate = vec![0; MAX_EXECUTION_CANDIDATE_BYTES + 1];
        assert!(matches!(
            decode_fabric_response_v1(encoded_response.header_bytes(), Some(&oversized_candidate)),
            Err(FabricAuthorityError::RecordTooLarge {
                kind: "candidate",
                ..
            })
        ));
    }

    #[test]
    fn corrupted_capsule_source_owvalue_and_result_digests_fail_closed() {
        let fixture = fixture();
        let request = FabricRequestV1::SubmitPureAttempt(fixture.submission.clone());
        let encoded_request = encode_fabric_request_v1(&request).unwrap();

        let mut corrupted_capsule = fixture.capsule_bytes.clone();
        let last = corrupted_capsule.len() - 1;
        corrupted_capsule[last] ^= 1;
        assert!(
            decode_fabric_request_v1(encoded_request.header_bytes(), Some(&corrupted_capsule))
                .unwrap_err()
                .to_string()
                .contains("payload length/digest mismatch")
        );

        for (field, expected) in [
            ("source_sha256", "source fragment digest mismatch"),
            ("closure_sha256", "source-closure digest mismatch"),
        ] {
            let mut corrupted_source: Value =
                crate::canonical_cbor::decode(encoded_request.header_bytes()).unwrap();
            corrupt_digest(value_field_mut(
                &mut corrupted_source,
                &["request", "body", "source_closure", field],
            ));
            let source_closure: FabricSourceClosureV1 = serde_json::from_value(
                value_field_mut(
                    &mut corrupted_source,
                    &["request", "body", "source_closure"],
                )
                .clone(),
            )
            .unwrap();
            assert!(source_closure
                .validate()
                .unwrap_err()
                .to_string()
                .contains(expected));
            let corrupted_source = crate::canonical_cbor::encode(&corrupted_source).unwrap();
            assert!(
                decode_fabric_request_v1(&corrupted_source, encoded_request.payload_bytes())
                    .unwrap_err()
                    .to_string()
                    .contains("submission binding digest mismatch")
            );
        }

        let mut corrupted_owvalue: Value =
            crate::canonical_cbor::decode(&fixture.capsule_bytes).unwrap();
        let bindings = value_field_mut(&mut corrupted_owvalue, &["inputs", "bindings"])
            .as_array_mut()
            .unwrap();
        corrupt_digest(value_field_mut(
            &mut bindings[0],
            &["value", "content_sha256"],
        ));
        let corrupted_owvalue = crate::canonical_cbor::encode(&corrupted_owvalue).unwrap();
        assert!(FabricSubmissionV1::new(
            fixture.submission.header().lease().clone(),
            fixture.submission.header().source_closure().clone(),
            corrupted_owvalue,
        )
        .unwrap_err()
        .to_string()
        .contains("portable value content digest mismatch"));

        let response = FabricResponseV1::TerminalCandidate(fixture.terminal);
        let encoded_response = encode_fabric_response_v1(&response).unwrap();
        let mut corrupted_result: Value =
            crate::canonical_cbor::decode(encoded_response.header_bytes()).unwrap();
        corrupt_digest(value_field_mut(
            &mut corrupted_result,
            &["response", "body", "receipt", "output_content_sha256"],
        ));
        let corrupted_result = crate::canonical_cbor::encode(&corrupted_result).unwrap();
        let FabricResponseV1::TerminalCandidate(corrupted_terminal) =
            decode_fabric_response_v1(&corrupted_result, encoded_response.payload_bytes()).unwrap()
        else {
            panic!("corrupted response changed its terminal tag");
        };
        let target = fixture.submission.header().lease().lease().target();
        let pinned = PinnedFabricNodeKeyV1::new(
            target.node_id(),
            target.node_generation(),
            target.execution_cell_incarnation(),
            fixture.node_key.public_key(),
        )
        .unwrap();
        assert!(pinned
            .verify_terminal_candidate(&corrupted_terminal, &fixture.submission)
            .unwrap_err()
            .to_string()
            .contains("signature"));
    }

    #[test]
    fn authority_rejects_unknown_issuer_invalid_signature_and_expired_lease() {
        let fixture = fixture();
        let signed = fixture.submission.header().lease();
        let now = UnixMillisV1::new(LEASE_ISSUED_UNIX_MS + 1_000);

        let unknown = TrustedFabricAuthoritiesV1::new();
        assert!(matches!(
            unknown.verify_execution_lease(signed, now),
            Err(FabricAuthorityError::UntrustedSigner(_))
        ));

        let mut trusted = TrustedFabricAuthoritiesV1::new();
        trusted.enroll(fixture.authority_key.public_key());
        trusted.verify_execution_lease(signed, now).unwrap();

        let mut invalid_signature = signed.clone();
        invalid_signature.signature = corrupt_lower_hex(invalid_signature.signature());
        assert!(matches!(
            trusted.verify_execution_lease(&invalid_signature, now),
            Err(FabricAuthorityError::Signature(_))
        ));

        let expired = UnixMillisV1::new(LEASE_EXPIRES_UNIX_MS + FABRIC_CLOCK_SKEW_TOLERANCE_MS + 1);
        assert!(trusted
            .verify_execution_lease(signed, expired)
            .unwrap_err()
            .to_string()
            .contains("expired"));
        trusted
            .authenticate_execution_lease(signed)
            .expect("expiry does not erase a trusted historical signature");
    }

    #[test]
    fn pinned_node_accepts_exact_candidate_and_rejects_wrong_node_or_generation() {
        let fixture = fixture();
        let target = fixture.submission.header().lease().lease().target();
        let pinned = PinnedFabricNodeKeyV1::new(
            NODE_ID,
            target.node_generation(),
            target.execution_cell_incarnation(),
            fixture.node_key.public_key(),
        )
        .unwrap();
        pinned
            .verify_terminal_candidate(&fixture.terminal, &fixture.submission)
            .unwrap();

        let wrong_node = PinnedFabricNodeKeyV1::new(
            "fabric-node-b",
            target.node_generation(),
            target.execution_cell_incarnation(),
            fixture.node_key.public_key(),
        )
        .unwrap();
        assert!(wrong_node
            .verify_terminal_candidate(&fixture.terminal, &fixture.submission)
            .unwrap_err()
            .to_string()
            .contains("node/generation"));

        let wrong_generation = PinnedFabricNodeKeyV1::new(
            NODE_ID,
            GenerationV1::new(target.node_generation().get() + 1).unwrap(),
            target.execution_cell_incarnation(),
            fixture.node_key.public_key(),
        )
        .unwrap();
        assert!(wrong_generation
            .verify_terminal_candidate(&fixture.terminal, &fixture.submission)
            .unwrap_err()
            .to_string()
            .contains("node/generation"));
    }
}
