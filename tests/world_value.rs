use std::collections::{BTreeMap, HashMap};

use num_bigint::BigInt;
use sha2::{Digest, Sha256};

use o_lang::value::{CapabilityKind, OBytes, ONumber, OText, OValue};
use o_lang::world::{
    identity_v1_conformance_records, negotiate_schema, world_value_v1_conformance_bytes,
    world_value_v1_conformance_records, world_value_v1_conformance_sha256, HostedValueError,
    IdentityWireRecord, NegotiatedSchema, PortableOValue, PortableValueError, PortableValueRecord,
    SchemaNegotiation, SchemaOffer, MAX_OVALUE_BYTES_BYTES, MAX_OVALUE_DEPTH,
    MAX_OVALUE_INTEGER_BYTES, MAX_OVALUE_LIST_ITEMS, MAX_OVALUE_MAP_ENTRIES, MAX_OVALUE_NODES,
    MAX_OVALUE_RECORD_BYTES, OVALUE_WIRE_MAGIC, OVALUE_WIRE_SCHEMA_V1,
};

fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right)
        .position(|(left, right)| left != right)
        .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())))
}

fn set_u16(record: &mut [u8], offset: usize, value: u16) {
    record[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn set_u32(record: &mut [u8], offset: usize, value: u32) {
    record[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn negotiated(max_record_bytes: u32) -> NegotiatedSchema {
    match negotiate_schema(
        SchemaOffer::v1(max_record_bytes).unwrap(),
        SchemaOffer::v1(max_record_bytes).unwrap(),
    ) {
        SchemaNegotiation::Selected(selection) => selection,
        SchemaNegotiation::Rejected(_) => panic!("equal v1 offers were rejected"),
    }
}

fn wrap_node(tag: u16, payload: &[u8]) -> Vec<u8> {
    let node = raw_node(tag, payload);
    wrap_root_node(&node)
}

fn raw_node(tag: u16, payload: &[u8]) -> Vec<u8> {
    let node_len = 8 + payload.len();
    let mut node = Vec::with_capacity(node_len);
    node.extend_from_slice(&tag.to_be_bytes());
    node.extend_from_slice(&0_u16.to_be_bytes());
    node.extend_from_slice(&(node_len as u32).to_be_bytes());
    node.extend_from_slice(payload);
    node
}

fn wrap_root_node(node: &[u8]) -> Vec<u8> {
    let node_len = node.len();
    let total = 16 + node_len;
    let mut record = Vec::with_capacity(total);
    record.extend_from_slice(OVALUE_WIRE_MAGIC);
    record.extend_from_slice(&OVALUE_WIRE_SCHEMA_V1.to_be_bytes());
    record.extend_from_slice(&0_u16.to_be_bytes());
    record.extend_from_slice(&(total as u32).to_be_bytes());
    record.extend_from_slice(node);
    record
}

#[test]
fn pinned_v1_corpus_is_exact_hash_stable_and_roundtrips() {
    let fixture_hex = include_str!("fixtures/world_value_v1.hex").trim();
    assert_eq!(fixture_hex, fixture_hex.to_ascii_lowercase());
    assert!(fixture_hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let expected = hex::decode(fixture_hex).unwrap();
    let encoded = world_value_v1_conformance_bytes().unwrap();
    if let Some(offset) = first_difference(&encoded, &expected) {
        panic!(
            "World value-v1 corpus differs at byte {offset}: Rust length {}, fixture length {}; actual_hex={}",
            encoded.len(),
            expected.len(),
            hex::encode(&encoded)
        );
    }

    let records = world_value_v1_conformance_records();
    assert_eq!(records.len(), 19);
    let mut offset = 0;
    for expected_record in records {
        let declared =
            u32::from_be_bytes(expected[offset + 12..offset + 16].try_into().unwrap()) as usize;
        let end = offset + declared;
        let fixture_record = &expected[offset..end];
        let decoded = PortableValueRecord::decode(fixture_record).unwrap();
        assert_eq!(decoded, expected_record);
        assert_eq!(decoded.encode().unwrap(), fixture_record);
        assert_eq!(
            decoded.canonical_sha256().unwrap().as_slice(),
            Sha256::digest(fixture_record).as_slice()
        );
        offset = end;
    }
    assert_eq!(offset, expected.len());

    let corpus_digest = hex::encode(world_value_v1_conformance_sha256().unwrap());
    assert_eq!(
        corpus_digest,
        "264e00550bbbe7561412d9a43f89036667ffbcf27add522131f8e650abef19bc"
    );
}

#[test]
fn headers_and_top_level_tags_are_fixed_big_endian() {
    let expected_tags = [
        0x0000, 0x0001, 0x0001, 0x0010, 0x0010, 0x0010, 0x0011, 0x0011, 0x0020, 0x0021, 0x0022,
        0x0030, 0x0031, 0x0032, 0x0040, 0x0041, 0x0042, 0x0043, 0x7f00,
    ];
    for (record, expected_tag) in world_value_v1_conformance_records()
        .into_iter()
        .zip(expected_tags)
    {
        let wire = record.encode().unwrap();
        assert_eq!(&wire[..8], OVALUE_WIRE_MAGIC);
        assert_eq!(u16::from_be_bytes([wire[8], wire[9]]), 1);
        assert_eq!(u16::from_be_bytes([wire[10], wire[11]]), 0);
        assert_eq!(
            u32::from_be_bytes(wire[12..16].try_into().unwrap()) as usize,
            wire.len()
        );
        assert_eq!(u16::from_be_bytes([wire[16], wire[17]]), expected_tag);
        assert_eq!(u16::from_be_bytes([wire[18], wire[19]]), 0);
        assert_eq!(
            u32::from_be_bytes(wire[20..24].try_into().unwrap()) as usize,
            wire.len() - 16
        );
    }
}

#[test]
fn every_strict_conformance_prefix_is_rejected() {
    for record in world_value_v1_conformance_records() {
        let wire = record.encode().unwrap();
        for length in 0..wire.len() {
            assert!(
                PortableValueRecord::decode(&wire[..length]).is_err(),
                "accepted prefix {length}/{}",
                wire.len()
            );
        }
    }
}

#[test]
fn negotiated_schema_and_extension_admission_are_contextual() {
    let core = world_value_v1_conformance_records()[8].encode().unwrap();
    let selected = negotiated(MAX_OVALUE_RECORD_BYTES);
    assert!(PortableValueRecord::decode_with_negotiated_schema(&core, selected).is_ok());

    let small = negotiated(24);
    assert!(matches!(
        PortableValueRecord::decode_with_negotiated_schema(&core, small),
        Err(PortableValueError::RecordExceedsLimit { .. })
    ));

    let extension = world_value_v1_conformance_records().pop().unwrap();
    let PortableValueRecord::Extension(expected) = &extension else {
        panic!("last corpus record is not an extension");
    };
    let wire = extension.encode().unwrap();
    let admitted = PortableValueRecord::decode_extension_with_negotiated_schema(
        &wire,
        selected,
        expected.namespace(),
        expected.name(),
        expected.version(),
        expected.schema_digest(),
    )
    .unwrap();
    assert_eq!(admitted.value(), expected.value());
    assert_eq!(
        PortableValueRecord::decode_extension_with_negotiated_schema(
            &wire,
            selected,
            "org.ostadix.other",
            expected.name(),
            expected.version(),
            expected.schema_digest(),
        ),
        Err(PortableValueError::ExtensionSchemaMismatch)
    );
    assert_eq!(
        PortableValueRecord::decode_extension_with_negotiated_schema(
            &core,
            selected,
            expected.namespace(),
            expected.name(),
            expected.version(),
            expected.schema_digest(),
        ),
        Err(PortableValueError::ExpectedExtension)
    );
}

#[test]
fn hosted_projection_is_an_explicit_recursive_allowlist() {
    let hosted = OValue::object(BTreeMap::from([
        ("answer".to_owned(), OValue::int(42)),
        (
            "values".to_owned(),
            OValue::list(vec![
                OValue::bool_(true),
                OValue::text("hello"),
                OValue::bytes(vec![0, 1], Some("application/octet-stream".to_owned())),
            ]),
        ),
    ]));
    let projected = PortableOValue::try_from(&hosted).unwrap();
    let PortableOValue::Record(fields) = projected else {
        panic!("hosted object did not project to a record");
    };
    assert_eq!(fields[0].0, "answer");

    let entries = OValue::entries_map(vec![
        (OValue::str_("b"), OValue::int(2)),
        (OValue::bool_(false), OValue::int(1)),
    ]);
    assert!(matches!(
        PortableOValue::try_from(&entries),
        Ok(PortableOValue::Map(_))
    ));

    let oversized_list = OValue::list(vec![OValue::Null; MAX_OVALUE_LIST_ITEMS + 1]);
    assert!(matches!(
        PortableOValue::try_from(&oversized_list),
        Err(HostedValueError::InvalidPortable(
            PortableValueError::EntryLimit { kind: "list", .. }
        ))
    ));
}

#[test]
fn hosted_authority_capsules_effects_and_unadapted_values_are_rejected_recursively() {
    let capability = OValue::Capability {
        kind: CapabilityKind::File,
        identity: "looks-like-a-bearer".to_owned(),
        metadata: HashMap::new(),
    };
    assert!(matches!(
        PortableOValue::try_from(&capability),
        Err(HostedValueError::AuthorityBearing { .. })
    ));
    assert!(matches!(
        PortableOValue::try_from(&OValue::list(vec![capability])),
        Err(HostedValueError::AuthorityBearing { .. })
    ));
    assert!(matches!(
        PortableOValue::try_from(&OValue::System {
            profile_path: "/live/system".to_owned()
        }),
        Err(HostedValueError::CapsuleBound { .. })
    ));
    assert!(matches!(
        PortableOValue::try_from(&OValue::Expr {
            src: "python^(42)_python".to_owned()
        }),
        Err(HostedValueError::Effectful { .. })
    ));
    assert!(matches!(
        PortableOValue::try_from(&OValue::Html {
            v: "<b>x</b>".to_owned()
        }),
        Err(HostedValueError::Unsupported { .. })
    ));
    assert!(matches!(
        PortableOValue::try_from(&OValue::Number {
            v: ONumber::Rational {
                num: BigInt::from(1),
                den: BigInt::from(2),
            }
        }),
        Err(HostedValueError::Unsupported { .. })
    ));
}

#[test]
fn header_tag_reserved_length_and_caller_bounds_fail_closed() {
    let valid = world_value_v1_conformance_records()[0].encode().unwrap();
    let mut bad = valid.clone();
    bad[0] ^= 1;
    assert_eq!(
        PortableValueRecord::decode(&bad),
        Err(PortableValueError::BadMagic)
    );

    bad = valid.clone();
    set_u16(&mut bad, 8, 2);
    assert_eq!(
        PortableValueRecord::decode(&bad),
        Err(PortableValueError::UnsupportedSchema { found: 2 })
    );
    bad = valid.clone();
    set_u16(&mut bad, 10, 1);
    assert_eq!(
        PortableValueRecord::decode(&bad),
        Err(PortableValueError::NonzeroReserved { found: 1 })
    );
    bad = valid.clone();
    set_u32(&mut bad, 12, valid.len() as u32 + 1);
    assert!(matches!(
        PortableValueRecord::decode(&bad),
        Err(PortableValueError::LengthMismatch { .. })
    ));
    bad = valid.clone();
    set_u16(&mut bad, 16, 0x5000);
    assert_eq!(
        PortableValueRecord::decode(&bad),
        Err(PortableValueError::UnknownTag { found: 0x5000 })
    );
    bad = valid.clone();
    set_u16(&mut bad, 18, 1);
    assert!(matches!(
        PortableValueRecord::decode(&bad),
        Err(PortableValueError::NonzeroNodeReserved { .. })
    ));
    bad = valid.clone();
    set_u32(&mut bad, 20, 7);
    assert!(matches!(
        PortableValueRecord::decode(&bad),
        Err(PortableValueError::InvalidNodeLength { .. })
    ));
    assert!(matches!(
        PortableValueRecord::decode_with_limit(&valid, valid.len() as u32 - 1),
        Err(PortableValueError::RecordExceedsLimit { .. })
    ));
    assert!(matches!(
        PortableValueRecord::decode(&vec![0; MAX_OVALUE_RECORD_BYTES as usize + 1]),
        Err(PortableValueError::RecordTooLarge { .. })
    ));
}

#[test]
fn scalar_shapes_utf8_unicode_and_media_are_strict() {
    let mut bool_wire = world_value_v1_conformance_records()[1].encode().unwrap();
    bool_wire[24] = 2;
    assert!(PortableValueRecord::decode(&bool_wire).is_err());

    let mut integer = world_value_v1_conformance_records()[5].encode().unwrap();
    integer[24] = 2;
    assert_eq!(
        PortableValueRecord::decode(&integer),
        Err(PortableValueError::InvalidInteger)
    );
    let mut zero = world_value_v1_conformance_records()[3].encode().unwrap();
    zero[24] = 1;
    assert_eq!(
        PortableValueRecord::decode(&zero),
        Err(PortableValueError::InvalidInteger)
    );

    let mut float = world_value_v1_conformance_records()[6].encode().unwrap();
    float[25] = 1;
    assert!(matches!(
        PortableValueRecord::decode(&float),
        Err(PortableValueError::NonzeroNodeReserved { .. })
    ));

    let mut text = world_value_v1_conformance_records()[8].encode().unwrap();
    *text.last_mut().unwrap() = 0xff;
    assert_eq!(
        PortableValueRecord::decode(&text),
        Err(PortableValueError::InvalidUtf8)
    );

    let mut scalar = world_value_v1_conformance_records()[9].encode().unwrap();
    scalar[24..28].copy_from_slice(&0x0000_d800_u32.to_be_bytes());
    assert!(matches!(
        PortableValueRecord::decode(&scalar),
        Err(PortableValueError::InvalidUnicodeScalar { .. })
    ));

    assert!(matches!(
        PortableOValue::bytes(OBytes {
            bytes: vec![],
            media_type: Some("not a media type".to_owned())
        }),
        Err(PortableValueError::InvalidMediaType)
    ));
    for media_type in ["/json", "application/", "application/json/extra"] {
        assert!(matches!(
            PortableOValue::bytes(OBytes {
                bytes: vec![],
                media_type: Some(media_type.to_owned())
            }),
            Err(PortableValueError::InvalidMediaType)
        ));
    }
    assert!(matches!(
        PortableOValue::text(OText {
            utf8: "x".to_owned(),
            encoding: Some("UTF-8".to_owned())
        }),
        Err(PortableValueError::InvalidTextEncoding)
    ));
}

#[test]
fn collection_order_duplicates_and_scalar_map_keys_are_enforced() {
    assert_eq!(
        PortableValueRecord::Core(PortableOValue::Record(vec![
            ("b".to_owned(), PortableOValue::Null),
            ("a".to_owned(), PortableOValue::Null),
        ]))
        .encode(),
        Err(PortableValueError::NonCanonicalRecordOrder)
    );
    assert_eq!(
        PortableOValue::record(vec![
            ("a".to_owned(), PortableOValue::Null),
            ("a".to_owned(), PortableOValue::Bool(true)),
        ]),
        Err(PortableValueError::DuplicateRecordKey)
    );
    assert_eq!(
        PortableValueRecord::Core(PortableOValue::Map(vec![
            (
                PortableOValue::text(OText {
                    utf8: "z".to_owned(),
                    encoding: None,
                })
                .unwrap(),
                PortableOValue::Null,
            ),
            (PortableOValue::Bool(false), PortableOValue::Null),
        ]))
        .encode(),
        Err(PortableValueError::NonCanonicalMapOrder)
    );
    assert_eq!(
        PortableOValue::map(vec![
            (PortableOValue::Bool(false), PortableOValue::Null),
            (PortableOValue::Bool(false), PortableOValue::Bool(true)),
        ]),
        Err(PortableValueError::DuplicateMapKey)
    );
    assert_eq!(
        PortableOValue::map(vec![(PortableOValue::List(vec![]), PortableOValue::Null,)]),
        Err(PortableValueError::NonScalarMapKey)
    );

    let canonical_record = PortableValueRecord::Core(
        PortableOValue::record(vec![
            ("a".to_owned(), PortableOValue::Null),
            ("b".to_owned(), PortableOValue::Null),
        ])
        .unwrap(),
    )
    .encode()
    .unwrap();
    let mut out_of_order = canonical_record.clone();
    out_of_order.swap(30, 41);
    assert_eq!(
        PortableValueRecord::decode(&out_of_order),
        Err(PortableValueError::NonCanonicalRecordOrder)
    );
    let mut duplicate = canonical_record;
    duplicate[41] = duplicate[30];
    assert_eq!(
        PortableValueRecord::decode(&duplicate),
        Err(PortableValueError::DuplicateRecordKey)
    );

    let canonical_map = PortableValueRecord::Core(
        PortableOValue::map(vec![
            (PortableOValue::Bool(false), PortableOValue::Null),
            (PortableOValue::Bool(true), PortableOValue::Null),
        ])
        .unwrap(),
    )
    .encode()
    .unwrap();
    let mut map_out_of_order = canonical_map.clone();
    map_out_of_order.swap(36, 53);
    assert_eq!(
        PortableValueRecord::decode(&map_out_of_order),
        Err(PortableValueError::NonCanonicalMapOrder)
    );
    let mut map_duplicate = canonical_map;
    map_duplicate[53] = map_duplicate[36];
    assert_eq!(
        PortableValueRecord::decode(&map_duplicate),
        Err(PortableValueError::DuplicateMapKey)
    );
}

#[test]
fn depth_node_entry_and_payload_limits_are_enforced() {
    let mut nested = PortableOValue::Null;
    for _ in 0..MAX_OVALUE_DEPTH {
        nested = PortableOValue::List(vec![nested]);
    }
    assert!(matches!(
        PortableValueRecord::Core(nested).encode(),
        Err(PortableValueError::DepthLimit { .. })
    ));

    let many_nodes = PortableOValue::List(
        (0..64)
            .map(|_| PortableOValue::List(vec![PortableOValue::Null]))
            .collect(),
    );
    assert!(matches!(
        PortableValueRecord::Core(many_nodes).encode(),
        Err(PortableValueError::NodeLimit { actual, maximum })
            if actual == MAX_OVALUE_NODES + 1 && maximum == MAX_OVALUE_NODES
    ));
    assert!(matches!(
        PortableValueRecord::Core(PortableOValue::List(vec![
            PortableOValue::Null;
            MAX_OVALUE_LIST_ITEMS + 1
        ]))
        .encode(),
        Err(PortableValueError::EntryLimit { kind: "list", .. })
    ));
    assert!(matches!(
        PortableOValue::record(
            (0..=MAX_OVALUE_MAP_ENTRIES)
                .map(|index| (format!("k{index}"), PortableOValue::Null))
                .collect()
        ),
        Err(PortableValueError::EntryLimit { kind: "record", .. })
    ));
    assert!(matches!(
        PortableOValue::integer(BigInt::from_bytes_be(
            num_bigint::Sign::Plus,
            &vec![1; MAX_OVALUE_INTEGER_BYTES + 1]
        )),
        Err(PortableValueError::IntegerTooLarge { .. })
    ));
    assert!(matches!(
        PortableOValue::bytes(OBytes {
            bytes: vec![0; MAX_OVALUE_BYTES_BYTES + 1],
            media_type: None
        }),
        Err(PortableValueError::BytesTooLong { .. })
    ));

    let mut raw_depth = raw_node(0x0000, &[]);
    for _ in 0..MAX_OVALUE_DEPTH {
        let mut payload = vec![0, 1, 0, 0];
        payload.extend_from_slice(&raw_depth);
        raw_depth = raw_node(0x0030, &payload);
    }
    assert!(matches!(
        PortableValueRecord::decode(&wrap_root_node(&raw_depth)),
        Err(PortableValueError::DepthLimit { .. })
    ));

    let raw_leaf = raw_node(0x0000, &[]);
    let mut raw_child_payload = vec![0, 1, 0, 0];
    raw_child_payload.extend_from_slice(&raw_leaf);
    let raw_child = raw_node(0x0030, &raw_child_payload);
    let mut raw_many_payload = vec![0, 64, 0, 0];
    for _ in 0..64 {
        raw_many_payload.extend_from_slice(&raw_child);
    }
    assert!(matches!(
        PortableValueRecord::decode(&wrap_node(0x0030, &raw_many_payload)),
        Err(PortableValueError::NodeLimit { .. })
    ));

    let excessive_count = wrap_node(0x0030, &[0, 65, 0, 0]);
    assert!(matches!(
        PortableValueRecord::decode(&excessive_count),
        Err(PortableValueError::EntryLimit { kind: "list", .. })
    ));
}

#[test]
fn object_references_and_extensions_remain_descriptive_and_strict() {
    let capability = identity_v1_conformance_records()
        .into_iter()
        .find(|record| matches!(record, IdentityWireRecord::Capability(_)))
        .unwrap()
        .encode()
        .unwrap();
    let wrong_identity = wrap_node(0x0042, &capability);
    assert!(matches!(
        PortableValueRecord::decode(&wrong_identity),
        Err(PortableValueError::ExpectedObjectIdentity)
    ));

    let extension = world_value_v1_conformance_records().pop().unwrap();
    let wire = extension.encode().unwrap();
    let mut zero_version = wire.clone();
    set_u16(&mut zero_version, 28, 0);
    assert_eq!(
        PortableValueRecord::decode(&zero_version),
        Err(PortableValueError::ZeroExtensionVersion)
    );
    let mut zero_digest = wire.clone();
    zero_digest[32..64].fill(0);
    assert_eq!(
        PortableValueRecord::decode(&zero_digest),
        Err(PortableValueError::ZeroExtensionSchemaDigest)
    );
    let mut bad_namespace = wire.clone();
    let namespace_start = 64;
    let namespace_len = u16::from_be_bytes([wire[24], wire[25]]) as usize;
    for byte in &mut bad_namespace[namespace_start..namespace_start + namespace_len] {
        if *byte == b'.' {
            *byte = b'_';
        }
    }
    assert_eq!(
        PortableValueRecord::decode(&bad_namespace),
        Err(PortableValueError::InvalidExtensionNamespace)
    );

    let extension_node = &wire[16..];
    let mut list_payload = Vec::new();
    list_payload.extend_from_slice(&1_u16.to_be_bytes());
    list_payload.extend_from_slice(&0_u16.to_be_bytes());
    list_payload.extend_from_slice(extension_node);
    let nested_extension = wrap_node(0x0030, &list_payload);
    assert_eq!(
        PortableValueRecord::decode(&nested_extension),
        Err(PortableValueError::NestedExtension)
    );
}

#[test]
fn arbitrary_text_and_bytes_never_become_authority() {
    let value = PortableValueRecord::Core(
        PortableOValue::record(vec![
            (
                "text".to_owned(),
                PortableOValue::text(OText {
                    utf8: "ocore-live:slot=1,generation=7".to_owned(),
                    encoding: Some("utf-8".to_owned()),
                })
                .unwrap(),
            ),
            (
                "bytes".to_owned(),
                PortableOValue::bytes(OBytes {
                    bytes: b"capability-looking-data".to_vec(),
                    media_type: None,
                })
                .unwrap(),
            ),
        ])
        .unwrap(),
    );
    let decoded = PortableValueRecord::decode(&value.encode().unwrap()).unwrap();
    assert_eq!(decoded, value);
}
