use o_lang::world::{
    identity_v1_conformance_bytes, identity_v1_conformance_records, DomainGeneration, DomainId,
    DomainIdentity, IdentityWireError, IdentityWireKind, IdentityWireRecord, NodeGeneration,
    NodeId, NodeIdentity, ProcessGeneration, ProcessId, ProcessIdentity, ResourceGeneration,
    ResourceId, ResourceIdentity, ResourceOwner, WorldEpoch, WorldId, WorldIdentity,
    WorldIdentityError, IDENTITY_WIRE_HEADER_BYTES, IDENTITY_WIRE_MAGIC, IDENTITY_WIRE_VERSION,
    MAX_IDENTITY_WIRE_RECORD_BYTES,
};

fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right)
        .position(|(left, right)| left != right)
        .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())))
}

fn skip_text(record: &[u8], cursor: &mut usize) {
    let length = u16::from_be_bytes([record[*cursor], record[*cursor + 1]]) as usize;
    *cursor += 2 + length;
}

fn note_counter(cursor: &mut usize, offsets: &mut Vec<usize>) {
    offsets.push(*cursor);
    *cursor += 8;
}

fn note_node(record: &[u8], cursor: &mut usize, offsets: &mut Vec<usize>) {
    skip_text(record, cursor);
    skip_text(record, cursor);
    note_counter(cursor, offsets);
}

fn note_domain(record: &[u8], cursor: &mut usize, offsets: &mut Vec<usize>) {
    note_node(record, cursor, offsets);
    skip_text(record, cursor);
    note_counter(cursor, offsets);
}

fn note_process(record: &[u8], cursor: &mut usize, offsets: &mut Vec<usize>) {
    note_domain(record, cursor, offsets);
    skip_text(record, cursor);
    note_counter(cursor, offsets);
}

fn counter_offsets(record: &[u8]) -> Vec<usize> {
    let kind = u16::from_be_bytes([record[10], record[11]]);
    let mut cursor = IDENTITY_WIRE_HEADER_BYTES;
    let mut offsets = Vec::new();
    match kind {
        1 => {
            skip_text(record, &mut cursor);
            note_counter(&mut cursor, &mut offsets);
        }
        2 => {
            skip_text(record, &mut cursor);
            note_counter(&mut cursor, &mut offsets);
            note_counter(&mut cursor, &mut offsets);
            note_counter(&mut cursor, &mut offsets);
        }
        3 => note_node(record, &mut cursor, &mut offsets),
        4 => note_domain(record, &mut cursor, &mut offsets),
        5 => note_process(record, &mut cursor, &mut offsets),
        6 => {
            let owner = u16::from_be_bytes([record[cursor], record[cursor + 1]]);
            cursor += 2;
            match owner {
                1 => {
                    skip_text(record, &mut cursor);
                    note_counter(&mut cursor, &mut offsets);
                }
                2 => note_node(record, &mut cursor, &mut offsets),
                3 => note_domain(record, &mut cursor, &mut offsets),
                4 => note_process(record, &mut cursor, &mut offsets),
                _ => panic!("conformance resource has unknown owner tag {owner}"),
            }
            skip_text(record, &mut cursor);
            note_counter(&mut cursor, &mut offsets);
        }
        7 => {
            skip_text(record, &mut cursor);
            skip_text(record, &mut cursor);
            note_counter(&mut cursor, &mut offsets);
        }
        8..=10 | 13 => {
            skip_text(record, &mut cursor);
            skip_text(record, &mut cursor);
        }
        11 => {
            skip_text(record, &mut cursor);
            skip_text(record, &mut cursor);
            note_counter(&mut cursor, &mut offsets);
        }
        12 => {
            skip_text(record, &mut cursor);
            skip_text(record, &mut cursor);
            note_counter(&mut cursor, &mut offsets);
            skip_text(record, &mut cursor);
        }
        _ => panic!("conformance record has unknown kind {kind}"),
    }
    assert_eq!(cursor, record.len());
    offsets
}

#[test]
fn pinned_v1_corpus_is_exact_and_each_record_roundtrips() {
    let fixture_hex = include_str!("fixtures/world_identity_v1.hex").trim();
    assert_eq!(fixture_hex, fixture_hex.to_ascii_lowercase());
    assert!(fixture_hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let expected = hex::decode(fixture_hex).unwrap();
    let encoded = identity_v1_conformance_bytes().unwrap();

    if let Some(offset) = first_difference(&encoded, &expected) {
        panic!(
            "identity-v1 corpus differs at byte {offset}: Rust length {}, fixture length {}",
            encoded.len(),
            expected.len()
        );
    }
    assert_eq!(encoded.len(), 838);

    let mut offset = 0;
    let records = identity_v1_conformance_records();
    assert_eq!(records.len(), 16);
    for expected_record in records {
        let wire = expected_record.encode().unwrap();
        let end = offset + wire.len();
        let fixture_record = &expected[offset..end];
        let decoded = IdentityWireRecord::decode(fixture_record).unwrap();
        assert_eq!(decoded, expected_record);
        assert_eq!(decoded.encode().unwrap(), fixture_record);
        offset = end;
    }
    assert_eq!(offset, expected.len());
}

#[test]
fn header_is_fixed_big_endian_and_total_length_includes_header() {
    let record = identity_v1_conformance_records().remove(0);
    let wire = record.encode().unwrap();
    assert_eq!(&wire[..8], IDENTITY_WIRE_MAGIC);
    assert_eq!(
        u16::from_be_bytes([wire[8], wire[9]]),
        IDENTITY_WIRE_VERSION
    );
    assert_eq!(
        u16::from_be_bytes([wire[10], wire[11]]),
        IdentityWireKind::World as u16
    );
    assert_eq!(
        u32::from_be_bytes([wire[12], wire[13], wire[14], wire[15]]) as usize,
        wire.len()
    );
    assert_eq!(wire.len(), IDENTITY_WIRE_HEADER_BYTES + 2 + 7 + 8);
    assert_eq!(&wire[wire.len() - 8..], &1_u64.to_be_bytes());
}

#[test]
fn all_resource_owner_tags_roundtrip_as_flattened_payloads() {
    let records = identity_v1_conformance_records();
    let resources: Vec<_> = records
        .into_iter()
        .filter(|record| record.kind() == IdentityWireKind::Resource)
        .collect();
    assert_eq!(resources.len(), 4);

    for (expected_tag, record) in (1_u16..=4).zip(resources) {
        let wire = record.encode().unwrap();
        assert_eq!(u16::from_be_bytes([wire[16], wire[17]]), expected_tag);
        assert_eq!(IdentityWireRecord::decode(&wire).unwrap(), record);
    }
}

#[test]
fn decode_rejects_zero_counter_and_invalid_text_without_partial_output() {
    let mut zero_epoch = identity_v1_conformance_records()[0].encode().unwrap();
    let counter = zero_epoch.len() - 8;
    zero_epoch[counter..].fill(0);
    assert!(matches!(
        IdentityWireRecord::decode(&zero_epoch),
        Err(IdentityWireError::InvalidIdentity(
            WorldIdentityError::ZeroGeneration {
                kind: "world epoch"
            }
        ))
    ));

    let mut invalid_id = identity_v1_conformance_records()[0].encode().unwrap();
    invalid_id[18] = b'/';
    assert!(matches!(
        IdentityWireRecord::decode(&invalid_id),
        Err(IdentityWireError::InvalidIdentity(
            WorldIdentityError::InvalidIdentifier {
                kind: "world identity",
                ..
            }
        ))
    ));

    let mut invalid_utf8 = identity_v1_conformance_records()[0].encode().unwrap();
    invalid_utf8[18] = 0xff;
    assert_eq!(
        IdentityWireRecord::decode(&invalid_utf8),
        Err(IdentityWireError::InvalidUtf8)
    );
}

#[test]
fn decode_rejects_zero_at_every_counter_position_in_the_conformance_corpus() {
    let mut checked = 0;
    for record in identity_v1_conformance_records() {
        let wire = record.encode().unwrap();
        for offset in counter_offsets(&wire) {
            let mut zeroed = wire.clone();
            zeroed[offset..offset + 8].fill(0);
            assert!(matches!(
                IdentityWireRecord::decode(&zeroed),
                Err(IdentityWireError::InvalidIdentity(
                    WorldIdentityError::ZeroGeneration { .. }
                ))
            ));
            checked += 1;
        }
    }
    assert_eq!(checked, 24);
}

#[test]
fn decode_rejects_malformed_header_kind_version_and_lengths() {
    let valid = identity_v1_conformance_records()[0].encode().unwrap();

    assert!(matches!(
        IdentityWireRecord::decode(&valid[..15]),
        Err(IdentityWireError::Truncated { .. })
    ));

    let mut bad_magic = valid.clone();
    bad_magic[0] ^= 0xff;
    assert_eq!(
        IdentityWireRecord::decode(&bad_magic),
        Err(IdentityWireError::BadMagic)
    );

    let mut bad_version = valid.clone();
    bad_version[8..10].copy_from_slice(&2_u16.to_be_bytes());
    assert_eq!(
        IdentityWireRecord::decode(&bad_version),
        Err(IdentityWireError::UnsupportedVersion { found: 2 })
    );

    let mut bad_kind = valid.clone();
    bad_kind[10..12].copy_from_slice(&14_u16.to_be_bytes());
    assert_eq!(
        IdentityWireRecord::decode(&bad_kind),
        Err(IdentityWireError::UnknownKind { found: 14 })
    );

    let mut bad_length = valid.clone();
    let declared = (valid.len() - 1) as u32;
    bad_length[12..16].copy_from_slice(&declared.to_be_bytes());
    assert_eq!(
        IdentityWireRecord::decode(&bad_length),
        Err(IdentityWireError::LengthMismatch {
            declared: valid.len() - 1,
            actual: valid.len()
        })
    );

    let mut appended = valid.clone();
    appended.push(0);
    assert_eq!(
        IdentityWireRecord::decode(&appended),
        Err(IdentityWireError::LengthMismatch {
            declared: valid.len(),
            actual: valid.len() + 1
        })
    );

    let appended_len = appended.len() as u32;
    appended[12..16].copy_from_slice(&appended_len.to_be_bytes());
    assert_eq!(
        IdentityWireRecord::decode(&appended),
        Err(IdentityWireError::TrailingBytes { remaining: 1 })
    );
}

#[test]
fn decode_rejects_unknown_resource_owner_and_record_over_limit() {
    let mut resource = identity_v1_conformance_records()[5].encode().unwrap();
    resource[16..18].copy_from_slice(&9_u16.to_be_bytes());
    assert_eq!(
        IdentityWireRecord::decode(&resource),
        Err(IdentityWireError::UnknownResourceOwner { found: 9 })
    );

    let oversized = vec![0; MAX_IDENTITY_WIRE_RECORD_BYTES + 1];
    assert_eq!(
        IdentityWireRecord::decode(&oversized),
        Err(IdentityWireError::RecordTooLarge {
            actual: MAX_IDENTITY_WIRE_RECORD_BYTES + 1,
            max: MAX_IDENTITY_WIRE_RECORD_BYTES
        })
    );

    let mut declared_oversized = identity_v1_conformance_records()[0].encode().unwrap();
    declared_oversized[12..16]
        .copy_from_slice(&((MAX_IDENTITY_WIRE_RECORD_BYTES + 1) as u32).to_be_bytes());
    assert_eq!(
        IdentityWireRecord::decode(&declared_oversized),
        Err(IdentityWireError::RecordTooLarge {
            actual: MAX_IDENTITY_WIRE_RECORD_BYTES + 1,
            max: MAX_IDENTITY_WIRE_RECORD_BYTES
        })
    );
}

#[test]
fn maximum_valid_identity_components_fit_and_roundtrip_below_record_cap() {
    let simple = "a".repeat(128);
    let resource_path = format!("{}/{}", "b".repeat(127), "c".repeat(128));
    let node = NodeIdentity::new(
        WorldId::new(simple.clone()).unwrap(),
        NodeId::new(simple.clone()).unwrap(),
        NodeGeneration::new(u64::MAX).unwrap(),
    );
    let domain = DomainIdentity::new(
        node,
        DomainId::new(simple.clone()).unwrap(),
        DomainGeneration::new(u64::MAX).unwrap(),
    );
    let process = ProcessIdentity::new(
        domain,
        ProcessId::new(simple).unwrap(),
        ProcessGeneration::new(u64::MAX).unwrap(),
    );
    let record = IdentityWireRecord::Resource(ResourceIdentity::new(
        ResourceOwner::Process { process },
        ResourceId::new(resource_path).unwrap(),
        ResourceGeneration::new(u64::MAX).unwrap(),
    ));

    let wire = record.encode().unwrap();
    assert!(wire.len() <= MAX_IDENTITY_WIRE_RECORD_BYTES);
    assert_eq!(IdentityWireRecord::decode(&wire).unwrap(), record);
}

#[test]
fn world_owner_carries_explicit_epoch() {
    let resource = IdentityWireRecord::Resource(ResourceIdentity::new(
        ResourceOwner::World {
            world: WorldIdentity::new(
                WorldId::new("world-a").unwrap(),
                WorldEpoch::new(7).unwrap(),
            ),
        },
        ResourceId::new("world/state").unwrap(),
        ResourceGeneration::new(2).unwrap(),
    ));
    let wire = resource.encode().unwrap();
    assert_eq!(IdentityWireRecord::decode(&wire).unwrap(), resource);
}
