use o_lang::world::{
    negotiate_schema, validate_rejection, validate_selection, world_protocol_v1_conformance_bytes,
    world_protocol_v1_conformance_records, IdentityWireKind, SchemaNegotiation, SchemaOffer,
    SchemaRejection, SchemaSelection, WorldCodecError, WorldProtocolError, WorldWireKind,
    WorldWireRecord, MAX_WORLD_WIRE_RECORD_BYTES, WORLD_SCHEMA_V1, WORLD_WIRE_CODEC_VERSION,
    WORLD_WIRE_HEADER_BYTES, WORLD_WIRE_MAGIC, WORLD_WIRE_MIN_RECORD_BYTES,
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

#[test]
fn pinned_v1_corpus_is_exact_and_each_record_roundtrips() {
    let fixture_hex = include_str!("fixtures/world_protocol_v1.hex").trim();
    assert_eq!(fixture_hex, fixture_hex.to_ascii_lowercase());
    assert!(fixture_hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let expected = hex::decode(fixture_hex).unwrap();
    let encoded = world_protocol_v1_conformance_bytes().unwrap();

    if let Some(offset) = first_difference(&encoded, &expected) {
        panic!(
            "World protocol-v1 corpus differs at byte {offset}: Rust length {}, fixture length {}",
            encoded.len(),
            expected.len()
        );
    }
    assert_eq!(encoded.len(), 1254);

    let records = world_protocol_v1_conformance_records();
    assert_eq!(records.len(), 20);
    let mut offset = 0;
    for expected_record in records {
        let declared =
            u32::from_be_bytes(expected[offset + 12..offset + 16].try_into().unwrap()) as usize;
        let end = offset + declared;
        let fixture_record = &expected[offset..end];
        let decoded = WorldWireRecord::decode(fixture_record).unwrap();
        assert_eq!(decoded, expected_record);
        assert_eq!(decoded.encode().unwrap(), fixture_record);
        offset = end;
    }
    assert_eq!(offset, expected.len());
}

#[test]
fn header_and_negotiation_payloads_are_fixed_big_endian() {
    let record = world_protocol_v1_conformance_records().remove(0);
    let wire = record.encode().unwrap();
    assert_eq!(wire.len(), 24);
    assert_eq!(&wire[..8], WORLD_WIRE_MAGIC);
    assert_eq!(
        u16::from_be_bytes([wire[8], wire[9]]),
        WORLD_WIRE_CODEC_VERSION
    );
    assert_eq!(
        u16::from_be_bytes([wire[10], wire[11]]),
        WorldWireKind::SchemaOffer as u16
    );
    assert_eq!(
        u32::from_be_bytes([wire[12], wire[13], wire[14], wire[15]]) as usize,
        wire.len()
    );
    assert_eq!(
        &wire[WORLD_WIRE_HEADER_BYTES..],
        &[0, 1, 0, 1, 0, 0, 0x40, 0]
    );
}

#[test]
fn every_strict_conformance_prefix_is_rejected_without_panicking() {
    for record in world_protocol_v1_conformance_records() {
        let wire = record.encode().unwrap();
        for length in 0..wire.len() {
            assert!(
                WorldWireRecord::decode(&wire[..length]).is_err(),
                "{:?} accepted strict prefix of {length} / {} bytes",
                record.kind(),
                wire.len()
            );
        }
    }
}

#[test]
fn negotiation_selects_highest_overlap_and_smallest_limit() {
    let local = SchemaOffer::new(1, 3, 4096).unwrap();
    let peer = SchemaOffer::new(2, 4, 1024).unwrap();
    let selected = match negotiate_schema(local, peer) {
        SchemaNegotiation::Selected(selected) => selected,
        SchemaNegotiation::Rejected(_) => panic!("overlapping offers were rejected"),
    };
    assert_eq!(selected.selection(), SchemaSelection::new(3, 1024).unwrap());
    let admitted = validate_selection(local, peer, selected.selection()).unwrap();
    assert_eq!(admitted, selected);

    assert!(matches!(
        validate_selection(local, peer, SchemaSelection::new(2, 1024).unwrap()),
        Err(WorldProtocolError::NonCanonicalSelection { .. })
    ));
    assert!(matches!(
        validate_selection(local, peer, SchemaSelection::new(3, 2048).unwrap()),
        Err(WorldProtocolError::NonCanonicalSelection { .. })
    ));
}

#[test]
fn negotiation_rejection_is_contextual_and_canonical() {
    let local = SchemaOffer::new(1, 1, 4096).unwrap();
    let peer = SchemaOffer::new(2, 3, 1024).unwrap();
    let rejected = match negotiate_schema(local, peer) {
        SchemaNegotiation::Rejected(rejected) => rejected,
        SchemaNegotiation::Selected(_) => panic!("disjoint offers selected a schema"),
    };
    validate_rejection(local, peer, rejected).unwrap();

    let tampered = SchemaRejection::no_common_version(1, 1, 3, 4).unwrap();
    assert_eq!(
        validate_rejection(local, peer, tampered),
        Err(WorldProtocolError::NonCanonicalRejectionForOffers)
    );
    assert_eq!(
        SchemaRejection::no_common_version(1, 2, 2, 3),
        Err(WorldProtocolError::NonCanonicalRejection)
    );
    assert_eq!(
        validate_selection(local, peer, SchemaSelection::new(1, 1024).unwrap()),
        Err(WorldProtocolError::SelectionWithoutOverlap)
    );

    let overlapping_local = SchemaOffer::new(1, 2, 4096).unwrap();
    let overlapping_peer = SchemaOffer::new(2, 3, 1024).unwrap();
    let syntactic_rejection = SchemaRejection::no_common_version(1, 1, 3, 3).unwrap();
    assert_eq!(
        validate_rejection(overlapping_local, overlapping_peer, syntactic_rejection),
        Err(WorldProtocolError::RejectionWithOverlap)
    );
}

#[test]
fn decode_rejects_header_length_kind_and_bound_failures() {
    let valid = world_protocol_v1_conformance_records()[0].encode().unwrap();

    assert!(matches!(
        WorldWireRecord::decode(&valid[..15]),
        Err(WorldCodecError::Truncated { .. })
    ));

    let mut bad_magic = valid.clone();
    bad_magic[0] ^= 1;
    assert_eq!(
        WorldWireRecord::decode(&bad_magic),
        Err(WorldCodecError::BadMagic)
    );

    let mut bad_version = valid.clone();
    set_u16(&mut bad_version, 8, 2);
    assert_eq!(
        WorldWireRecord::decode(&bad_version),
        Err(WorldCodecError::UnsupportedCodecVersion { found: 2 })
    );

    let mut bad_kind = valid.clone();
    set_u16(&mut bad_kind, 10, 0xffff);
    assert_eq!(
        WorldWireRecord::decode(&bad_kind),
        Err(WorldCodecError::UnknownKind { found: 0xffff })
    );

    let mut wrong_length = valid.clone();
    set_u32(&mut wrong_length, 12, valid.len() as u32 + 1);
    assert!(matches!(
        WorldWireRecord::decode(&wrong_length),
        Err(WorldCodecError::LengthMismatch { .. })
    ));

    assert_eq!(
        WorldWireRecord::decode_with_limit(&valid, valid.len() as u32 - 1),
        Err(WorldCodecError::RecordExceedsLimit {
            actual: valid.len(),
            limit: valid.len() as u32 - 1,
        })
    );

    let oversized = vec![0_u8; MAX_WORLD_WIRE_RECORD_BYTES as usize + 1];
    assert_eq!(
        WorldWireRecord::decode(&oversized),
        Err(WorldCodecError::RecordTooLarge {
            actual: oversized.len(),
            maximum: MAX_WORLD_WIRE_RECORD_BYTES as usize,
        })
    );

    let mut oversized_declaration = valid;
    set_u32(
        &mut oversized_declaration,
        12,
        MAX_WORLD_WIRE_RECORD_BYTES + 1,
    );
    assert!(matches!(
        WorldWireRecord::decode(&oversized_declaration),
        Err(WorldCodecError::RecordTooLarge { .. })
    ));
}

#[test]
fn decode_rejects_noncanonical_negotiation_payloads() {
    let records = world_protocol_v1_conformance_records();

    let mut zero_min = records[0].encode().unwrap();
    set_u16(&mut zero_min, 16, 0);
    assert!(matches!(
        WorldWireRecord::decode(&zero_min),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::ZeroSchemaVersion
        ))
    ));

    let mut reversed = records[0].encode().unwrap();
    set_u16(&mut reversed, 16, 4);
    set_u16(&mut reversed, 18, 3);
    assert!(matches!(
        WorldWireRecord::decode(&reversed),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::InvertedSchemaRange { .. }
        ))
    ));

    let mut too_small = records[0].encode().unwrap();
    set_u32(&mut too_small, 20, WORLD_WIRE_MIN_RECORD_BYTES - 1);
    assert!(matches!(
        WorldWireRecord::decode(&too_small),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::RecordLimitTooSmall { .. }
        ))
    ));

    let mut too_large = records[0].encode().unwrap();
    set_u32(&mut too_large, 20, MAX_WORLD_WIRE_RECORD_BYTES + 1);
    assert!(matches!(
        WorldWireRecord::decode(&too_large),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::RecordLimitTooLarge { .. }
        ))
    ));

    let mut selection_reserved = records[2].encode().unwrap();
    set_u16(&mut selection_reserved, 18, 1);
    assert_eq!(
        WorldWireRecord::decode(&selection_reserved),
        Err(WorldCodecError::NonzeroReserved {
            kind: WorldWireKind::SchemaSelection,
            found: 1,
        })
    );

    let mut selection_zero = records[2].encode().unwrap();
    set_u16(&mut selection_zero, 16, 0);
    assert!(matches!(
        WorldWireRecord::decode(&selection_zero),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::ZeroSchemaVersion
        ))
    ));

    let mut selection_too_small = records[2].encode().unwrap();
    set_u32(
        &mut selection_too_small,
        20,
        WORLD_WIRE_MIN_RECORD_BYTES - 1,
    );
    assert!(matches!(
        WorldWireRecord::decode(&selection_too_small),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::RecordLimitTooSmall { .. }
        ))
    ));

    let mut selection_too_large = records[2].encode().unwrap();
    set_u32(
        &mut selection_too_large,
        20,
        MAX_WORLD_WIRE_RECORD_BYTES + 1,
    );
    assert!(matches!(
        WorldWireRecord::decode(&selection_too_large),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::RecordLimitTooLarge { .. }
        ))
    ));

    let mut overlapping_rejection = records[3].encode().unwrap();
    set_u16(&mut overlapping_rejection, 20, 1);
    set_u16(&mut overlapping_rejection, 22, 1);
    assert!(matches!(
        WorldWireRecord::decode(&overlapping_rejection),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::NonCanonicalRejection
        ))
    ));

    let mut zero_rejection_range = records[3].encode().unwrap();
    set_u16(&mut zero_rejection_range, 16, 0);
    assert!(matches!(
        WorldWireRecord::decode(&zero_rejection_range),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::ZeroSchemaVersion
        ))
    ));

    let mut inverted_rejection_range = records[3].encode().unwrap();
    set_u16(&mut inverted_rejection_range, 20, 4);
    set_u16(&mut inverted_rejection_range, 22, 3);
    assert!(matches!(
        WorldWireRecord::decode(&inverted_rejection_range),
        Err(WorldCodecError::Protocol(
            WorldProtocolError::InvertedSchemaRange { .. }
        ))
    ));

    let mut trailing = records[0].encode().unwrap();
    trailing.push(0);
    let trailing_len = trailing.len() as u32;
    set_u32(&mut trailing, 12, trailing_len);
    assert!(matches!(
        WorldWireRecord::decode(&trailing),
        Err(WorldCodecError::InvalidPayloadLength { .. })
    ));

    for record in [&records[0], &records[2], &records[3]] {
        let mut short = record.encode().unwrap();
        short.pop();
        let short_len = short.len() as u32;
        set_u32(&mut short, 12, short_len);
        assert!(matches!(
            WorldWireRecord::decode(&short),
            Err(WorldCodecError::InvalidPayloadLength {
                expected: 8,
                actual: 7,
                ..
            })
        ));
    }
}

#[test]
fn identity_descriptions_validate_schema_reserved_and_nested_record() {
    let records = world_protocol_v1_conformance_records();
    let first_identity = records[4].encode().unwrap();
    assert_eq!(
        u16::from_be_bytes([first_identity[16], first_identity[17]]),
        WORLD_SCHEMA_V1
    );

    let mut bad_schema = first_identity.clone();
    set_u16(&mut bad_schema, 16, 2);
    assert_eq!(
        WorldWireRecord::decode(&bad_schema),
        Err(WorldCodecError::UnsupportedPayloadSchema {
            kind: WorldWireKind::IdentityDescription,
            found: 2,
        })
    );

    let mut bad_reserved = first_identity.clone();
    set_u16(&mut bad_reserved, 18, 1);
    assert_eq!(
        WorldWireRecord::decode(&bad_reserved),
        Err(WorldCodecError::NonzeroReserved {
            kind: WorldWireKind::IdentityDescription,
            found: 1,
        })
    );

    let mut bad_nested_version = first_identity;
    set_u16(&mut bad_nested_version, 28, 2);
    assert!(matches!(
        WorldWireRecord::decode(&bad_nested_version),
        Err(WorldCodecError::Identity(_))
    ));

    let mut short_identity = records[4].encode().unwrap();
    short_identity.truncate(WORLD_WIRE_HEADER_BYTES + 3);
    let short_len = short_identity.len() as u32;
    set_u32(&mut short_identity, 12, short_len);
    assert_eq!(
        WorldWireRecord::decode(&short_identity),
        Err(WorldCodecError::Truncated {
            needed: 4,
            remaining: 3,
        })
    );

    let mut nested_trailing = records[4].encode().unwrap();
    nested_trailing.push(0);
    let nested_trailing_len = nested_trailing.len() as u32;
    set_u32(&mut nested_trailing, 12, nested_trailing_len);
    assert!(matches!(
        WorldWireRecord::decode(&nested_trailing),
        Err(WorldCodecError::Identity(_))
    ));

    let capability = records
        .iter()
        .find(|record| {
            matches!(
                record,
                WorldWireRecord::IdentityDescription(identity)
                    if identity.kind() == IdentityWireKind::Capability
            )
        })
        .unwrap();
    let decoded = WorldWireRecord::decode(&capability.encode().unwrap()).unwrap();
    assert_eq!(&decoded, capability);
}

#[test]
fn encode_honors_negotiated_limit_before_emitting_a_record() {
    let identity = world_protocol_v1_conformance_records()[4].clone();
    let encoded = identity.encode().unwrap();
    assert_eq!(
        identity.encode_with_limit(encoded.len() as u32 - 1),
        Err(WorldCodecError::RecordExceedsLimit {
            actual: encoded.len(),
            limit: encoded.len() as u32 - 1,
        })
    );
}

#[test]
fn post_negotiation_decode_binds_identity_schema_and_record_limit() {
    let records = world_protocol_v1_conformance_records();
    let local = match records[0] {
        WorldWireRecord::SchemaOffer(offer) => offer,
        _ => panic!("conformance record zero is not an offer"),
    };
    let peer = match records[1] {
        WorldWireRecord::SchemaOffer(offer) => offer,
        _ => panic!("conformance record one is not an offer"),
    };
    let selection = match records[2] {
        WorldWireRecord::SchemaSelection(selection) => selection,
        _ => panic!("conformance record two is not a selection"),
    };
    let admitted = validate_selection(local, peer, selection).unwrap();
    let identity_wire = records[4].encode().unwrap();
    let identity =
        WorldWireRecord::decode_identity_with_negotiated_schema(&identity_wire, admitted).unwrap();
    assert_eq!(WorldWireRecord::IdentityDescription(identity), records[4]);

    let tiny_local = SchemaOffer::v1(WORLD_WIRE_MIN_RECORD_BYTES).unwrap();
    let tiny_peer = SchemaOffer::v1(WORLD_WIRE_MIN_RECORD_BYTES).unwrap();
    let tiny = match negotiate_schema(tiny_local, tiny_peer) {
        SchemaNegotiation::Selected(selected) => selected,
        SchemaNegotiation::Rejected(_) => panic!("identical v1 offers were rejected"),
    };
    assert!(matches!(
        WorldWireRecord::decode_identity_with_negotiated_schema(&identity_wire, tiny),
        Err(WorldCodecError::RecordExceedsLimit { .. })
    ));

    let future_local = SchemaOffer::new(2, 2, 1024).unwrap();
    let future_peer = SchemaOffer::new(2, 2, 1024).unwrap();
    let future = match negotiate_schema(future_local, future_peer) {
        SchemaNegotiation::Selected(selected) => selected,
        SchemaNegotiation::Rejected(_) => panic!("identical future offers were rejected"),
    };
    assert_eq!(
        WorldWireRecord::decode_identity_with_negotiated_schema(&identity_wire, future),
        Err(WorldCodecError::UnsupportedNegotiatedSchema { found: 2 })
    );

    assert_eq!(
        WorldWireRecord::decode_identity_with_negotiated_schema(
            &records[0].encode().unwrap(),
            admitted,
        ),
        Err(WorldCodecError::ExpectedIdentityDescription {
            found: WorldWireKind::SchemaOffer,
        })
    );
}
