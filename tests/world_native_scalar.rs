//! Differential corpus for native instruction execution and unsigned receipts.
//! Fixed identities are trusted gate fixtures, not evidence of a live Governor.

use std::fs;

use o_lang::world::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCase {
    request: String,
    context_sha256: String,
    expected_unsigned: String,
}

fn context() -> (ReceiptContextV1, ReceiptCurrentStateV1, Vec<u8>) {
    let id = WorldId::new("native-world").unwrap();
    let world = WorldIdentity::new(id.clone(), WorldEpoch::new(7).unwrap());
    let governor = GovernorIdentity::new(
        world.clone(),
        GovernorTerm::new(3).unwrap(),
        GovernorLogIndex::new(9).unwrap(),
    );
    let node = NodeIdentity::new(
        id.clone(),
        NodeId::new("native-node").unwrap(),
        NodeGeneration::new(4).unwrap(),
    );
    let domain = DomainIdentity::new(
        node.clone(),
        DomainId::new("native-domain").unwrap(),
        DomainGeneration::new(2).unwrap(),
    );
    let attempt = AttemptIdentity::new(
        id.clone(),
        TaskId::new("native-task").unwrap(),
        AttemptGeneration::new(5).unwrap(),
    );
    let receipt = ReceiptIdentity::new(id, ReceiptId::new("native-receipt").unwrap());
    let mut encoded_context = Vec::new();
    for identity in [
        IdentityWireRecord::Receipt(receipt.clone()),
        IdentityWireRecord::World(world.clone()),
        IdentityWireRecord::Governor(governor.clone()),
        IdentityWireRecord::Attempt(attempt.clone()),
        IdentityWireRecord::Node(node.clone()),
        IdentityWireRecord::Domain(domain.clone()),
    ] {
        let record = identity.encode().unwrap();
        encoded_context.extend_from_slice(&(record.len() as u16).to_be_bytes());
        encoded_context.extend(record);
    }
    encoded_context.extend_from_slice(&0_u16.to_be_bytes());
    let current = ReceiptCurrentStateV1::new(
        world.clone(),
        governor.clone(),
        node.clone(),
        domain.clone(),
        None,
        attempt.clone(),
        vec![],
    )
    .unwrap();
    let context = ReceiptContextV1::new(
        receipt,
        world,
        governor,
        attempt,
        ReceiptPlacementV1::new(node, domain, None, vec![]).unwrap(),
    )
    .unwrap();
    (context, current, encoded_context)
}

fn native_case(left: u32, right: u32) -> NativeCase {
    let (context, current, encoded_context) = context();
    let mut hash = Sha256::new();
    hash.update(b"OSTADIX/NATIVE-SCALAR-CONTEXT/V1\0");
    hash.update(&encoded_context);
    let binding = hash.finalize();
    let mut request = b"ONSCAL1\0".to_vec();
    request.extend_from_slice(&binding);
    request.extend_from_slice(&1_u64.to_be_bytes());
    request.extend_from_slice(&u64::from(left).to_be_bytes());
    request.extend_from_slice(&u64::from(right).to_be_bytes());
    assert_eq!(request.len(), 64);
    let result = PortableOValue::integer(u64::from(left) + u64::from(right)).unwrap();
    let receipt = ExecutionReceiptV1::new(
        context,
        ReceiptSubjectV1::new(
            Some(ArtifactId::from_sha256(hex::encode(Sha256::digest(&request))).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap(),
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        ReceiptTerminalV1::Success(PortableValueRecord::Core(result)),
        ReceiptCommitFenceV1::Uncommitted,
        None,
    )
    .unwrap();
    // The hosted signer is used only to reach the existing canonical encoder.
    // Native code emits the unsigned body, never this detached signature.
    let signed = encode_signed_receipt_v1(
        &receipt,
        &current,
        &Ed25519ReceiptSigner::from_secret_bytes([0x64; 32]),
    )
    .unwrap();
    let unsigned = &signed[24..signed.len() - 96];
    assert_eq!(inspect_unsigned_receipt_v1(unsigned).unwrap(), receipt);
    assert!(unsigned.starts_with(&encoded_context));
    NativeCase {
        request: hex::encode(request),
        context_sha256: hex::encode(binding),
        expected_unsigned: hex::encode(unsigned),
    }
}

#[test]
fn native_scalar_fixture_and_returned_receipts_use_exact_world_identity() {
    let cases: Vec<_> = [(0, 0), (17, 25), (u32::MAX, 1), (u32::MAX, u32::MAX)]
        .into_iter()
        .map(|(left, right)| native_case(left, right))
        .collect();
    if let Some(path) = std::env::var_os("O_NATIVE_SCALAR_FIXTURE_OUT") {
        fs::write(path, serde_json::to_vec_pretty(&cases).unwrap()).unwrap();
    }
    if let Some(path) = std::env::var_os("O_NATIVE_SCALAR_RESULTS_IN") {
        let returned: Vec<String> = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(returned.len(), cases.len());
        for (native, case) in returned.iter().zip(cases.iter()) {
            let native = hex::decode(native).unwrap();
            let expected = hex::decode(&case.expected_unsigned).unwrap();
            let receipt = inspect_unsigned_receipt_v1(&native).unwrap();
            assert_eq!(
                native, expected,
                "native result or identity differs from Rust"
            );
            assert_eq!(receipt, inspect_unsigned_receipt_v1(&expected).unwrap());
            receipt.validate_current(&context().1).unwrap();
            assert_eq!(receipt.commit(), &ReceiptCommitFenceV1::Uncommitted);
            assert!(receipt.capabilities().is_empty());
            assert!(receipt.effects().is_empty());
            assert!(receipt.evidence().is_none());
            assert!(receipt.context().placement().process().is_none());
        }
    }
}

#[test]
fn unsigned_inspection_rejects_malformed_and_noncanonical_receipts() {
    let case = native_case(17, 25);
    let bytes = hex::decode(case.expected_unsigned).unwrap();
    for prefix in [0, 1, 2, 16, bytes.len() - 1] {
        assert!(inspect_unsigned_receipt_v1(&bytes[..prefix]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(inspect_unsigned_receipt_v1(&trailing).is_err());
    let mut reserved = bytes.clone();
    // First nested identity's reserved header byte must remain canonical zero.
    reserved[2 + 12] = 1;
    assert!(inspect_unsigned_receipt_v1(&reserved).is_err());
    let mut invented_commit = bytes.clone();
    let commit_position = invented_commit.len() - 8;
    invented_commit[commit_position] = 1;
    assert!(inspect_unsigned_receipt_v1(&invented_commit).is_err());
    // Structural inspection does not invent a signature or a current fence.
    assert!(inspect_unsigned_receipt_v1(&bytes).is_ok());
}
