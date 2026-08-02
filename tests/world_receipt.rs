use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use o_lang::value::{CapabilityKind, OText, OValue};
use o_lang::world::*;
use sha2::{Digest, Sha256};

const CONFORMANCE_SECRET: [u8; 32] = [0x42; 32];

struct ExactResolver {
    key_id: [u8; 32],
    public: [u8; 32],
}

struct DenyResolver;

impl ReceiptKeyResolver for DenyResolver {
    fn resolve_ed25519(&self, _key_id: &[u8; 32]) -> Option<[u8; 32]> {
        None
    }
}

impl ReceiptKeyResolver for ExactResolver {
    fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]> {
        (key_id == &self.key_id).then_some(self.public)
    }
}

fn artifact(label: &str) -> ArtifactId {
    ArtifactId::from_sha256(hex::encode(Sha256::digest(label.as_bytes()))).unwrap()
}

fn ids(
    node_generation: u64,
    domain_generation: u64,
    process_generation: u64,
    attempt_generation: u64,
    object_version: u64,
    receipt_name: &str,
) -> (
    ReceiptContextV1,
    ReceiptCurrentStateV1,
    ObjectIdentity,
    ObjectIdentity,
    ResourceIdentity,
    CheckpointIdentity,
) {
    let world_id = WorldId::new("world-a").unwrap();
    let world = WorldIdentity::new(world_id.clone(), WorldEpoch::new(7).unwrap());
    let governor = GovernorIdentity::new(
        world.clone(),
        GovernorTerm::new(3).unwrap(),
        GovernorLogIndex::new(9).unwrap(),
    );
    let node = NodeIdentity::new(
        world_id.clone(),
        NodeId::new("node-a").unwrap(),
        NodeGeneration::new(node_generation).unwrap(),
    );
    let domain = DomainIdentity::new(
        node.clone(),
        DomainId::new("domain-a").unwrap(),
        DomainGeneration::new(domain_generation).unwrap(),
    );
    let process = ProcessIdentity::new(
        domain.clone(),
        ProcessId::new("process-a").unwrap(),
        ProcessGeneration::new(process_generation).unwrap(),
    );
    let attempt = AttemptIdentity::new(
        world_id.clone(),
        TaskId::new("task-a").unwrap(),
        AttemptGeneration::new(attempt_generation).unwrap(),
    );
    let rejected = NodeIdentity::new(
        world_id.clone(),
        NodeId::new("node-b").unwrap(),
        NodeGeneration::new(1).unwrap(),
    );
    let placement = ReceiptPlacementV1::new(
        node.clone(),
        domain.clone(),
        Some(process.clone()),
        vec![PlacementRejectionV1::new(rejected, "resource-busy").unwrap()],
    )
    .unwrap();
    let context = ReceiptContextV1::new(
        ReceiptIdentity::new(world_id.clone(), ReceiptId::new(receipt_name).unwrap()),
        world.clone(),
        governor.clone(),
        attempt.clone(),
        placement,
    )
    .unwrap();
    let input = ObjectIdentity::new(
        world_id.clone(),
        ObjectId::new("object-input").unwrap(),
        ObjectVersion::new(object_version).unwrap(),
    );
    let output = ObjectIdentity::new(
        world_id,
        ObjectId::new("object-output").unwrap(),
        ObjectVersion::new(1).unwrap(),
    );
    let resource = ResourceIdentity::new(
        ResourceOwner::Domain {
            domain: domain.clone(),
        },
        ResourceId::new("fs/input").unwrap(),
        ResourceGeneration::new(2).unwrap(),
    );
    let checkpoint =
        CheckpointIdentity::new(attempt.clone(), CheckpointId::new("checkpoint-a").unwrap());
    let current = ReceiptCurrentStateV1::new(
        world,
        governor,
        node,
        domain,
        Some(process),
        attempt,
        vec![input.clone(), output.clone()],
    )
    .unwrap();
    (context, current, input, output, resource, checkpoint)
}

fn sample(
    node_generation: u64,
    domain_generation: u64,
    process_generation: u64,
    attempt_generation: u64,
    object_version: u64,
    receipt_name: &str,
    success: bool,
) -> (ExecutionReceiptV1, ReceiptCurrentStateV1) {
    let (context, current, input, output, resource, checkpoint) = ids(
        node_generation,
        domain_generation,
        process_generation,
        attempt_generation,
        object_version,
        receipt_name,
    );
    let world_id = context.world().world().clone();
    let terminal = if success {
        ReceiptTerminalV1::Success(PortableValueRecord::Core(
            PortableOValue::record(vec![
                ("ok".into(), PortableOValue::Bool(true)),
                (
                    "result".into(),
                    PortableOValue::text(OText {
                        utf8: "ostadix".into(),
                        encoding: Some("utf-8".into()),
                    })
                    .unwrap(),
                ),
            ])
            .unwrap(),
        ))
    } else {
        ReceiptTerminalV1::failure("stale-retry", artifact("failure-detail")).unwrap()
    };
    let receipt = ExecutionReceiptV1::new(
        context.clone(),
        ReceiptSubjectV1::new(
            Some(artifact("source")),
            Some(artifact("bundle")),
            Some(artifact("package")),
            Some(artifact("logical-hgraph")),
            Some(artifact("effects")),
        )
        .unwrap(),
        vec![
            ComponentObservationV1::new(
                ComponentKindV1::Project,
                "project/demo",
                0,
                artifact("bundle"),
            )
            .unwrap(),
            ComponentObservationV1::new(
                ComponentKindV1::LiveSystem,
                "live/echo",
                4,
                artifact("live-package"),
            )
            .unwrap(),
            ComponentObservationV1::new(
                ComponentKindV1::KernelWorld,
                "kernel/linux",
                2,
                artifact("kernel-package"),
            )
            .unwrap(),
        ],
        vec![CapabilityObservationV1::new(
            CapabilityIdentity::new(world_id, CapabilityId::new("capability-a").unwrap()),
            vec![
                ReceiptRight::new("invoke").unwrap(),
                ReceiptRight::new("read").unwrap(),
            ],
        )
        .unwrap()],
        vec![
            ObjectObservationV1::new(input, ObjectRoleV1::Input, artifact("input"), 17).unwrap(),
            ObjectObservationV1::new(output, ObjectRoleV1::Output, artifact("output"), 23).unwrap(),
        ],
        vec![CapsuleObservationV1::new(
            artifact("capsule"),
            "python",
            context.placement().node().clone(),
        )
        .unwrap()],
        vec![EffectObservationV1::new(
            resource,
            artifact("effect-before"),
            artifact("effect-after"),
        )
        .unwrap()],
        vec![CheckpointObservationV1::new(checkpoint, artifact("checkpoint"), !success).unwrap()],
        terminal,
        ReceiptCommitFenceV1::Governed(context.governor().clone()),
        Some(EvidenceObservationV1::new("world-receipt-v1", artifact("transcript")).unwrap()),
    )
    .unwrap();
    (receipt, current)
}

fn corpus() -> Vec<u8> {
    let signer = Ed25519ReceiptSigner::from_secret_bytes(CONFORMANCE_SECRET);
    let (current_receipt, current) = sample(4, 2, 5, 6, 2, "receipt-current", true);
    let (stale_receipt, stale_at_issuance) = sample(3, 1, 4, 5, 1, "receipt-stale", false);
    let mut bytes = encode_signed_receipt_v1(&current_receipt, &current, &signer).unwrap();
    bytes.extend_from_slice(
        &encode_signed_receipt_v1(&stale_receipt, &stale_at_issuance, &signer).unwrap(),
    );
    bytes
}

fn split_corpus(corpus: &[u8]) -> Vec<&[u8]> {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < corpus.len() {
        let total =
            u32::from_be_bytes(corpus[offset + 12..offset + 16].try_into().unwrap()) as usize;
        records.push(&corpus[offset..offset + total]);
        offset += total;
    }
    assert_eq!(offset, corpus.len());
    records
}

#[test]
fn hosted_ed25519_sign_verify_and_canonical_roundtrip() {
    let signer = Ed25519ReceiptSigner::from_secret_bytes(CONFORMANCE_SECRET);
    let resolver = ExactResolver {
        key_id: signer.key_id(),
        public: signer.public_key_bytes(),
    };
    for record in split_corpus(&corpus()) {
        let inspected = inspect_signed_receipt_v1(record).unwrap();
        assert_eq!(inspected.bytes(), record);
        assert_eq!(
            inspected
                .clone()
                .verify(&resolver)
                .unwrap()
                .signed()
                .bytes(),
            record
        );
        assert_eq!(
            receipt_signing_preimage_v1(record).unwrap(),
            inspected.signing_preimage().unwrap()
        );
    }
}

#[test]
fn tamper_wrong_key_and_malformed_envelope_fail_closed() {
    let signer = Ed25519ReceiptSigner::from_secret_bytes(CONFORMANCE_SECRET);
    let resolver = ExactResolver {
        key_id: signer.key_id(),
        public: signer.public_key_bytes(),
    };
    let records = corpus();
    let first = split_corpus(&records)[0];

    assert!(matches!(
        inspect_signed_receipt_v1(first)
            .unwrap()
            .verify(&DenyResolver),
        Err(ReceiptError::UntrustedSigner(_))
    ));

    let mut signature_tamper = first.to_vec();
    *signature_tamper.last_mut().unwrap() ^= 1;
    assert_eq!(
        inspect_signed_receipt_v1(&signature_tamper)
            .unwrap()
            .verify(&resolver)
            .unwrap_err(),
        ReceiptError::InvalidSignature
    );

    // Change a content digest without violating the canonical body grammar.
    // Structural inspection must still succeed, while Ed25519 verification
    // must fail because the exact body is in the signed preimage.
    let source_digest = Sha256::digest(b"source");
    let digest_offset = first
        .windows(source_digest.len())
        .position(|window| window == source_digest.as_slice())
        .unwrap();
    let mut body_tamper = first.to_vec();
    body_tamper[digest_offset] ^= 1;
    assert_eq!(
        inspect_signed_receipt_v1(&body_tamper)
            .unwrap()
            .verify(&resolver)
            .unwrap_err(),
        ReceiptError::InvalidSignature
    );

    let wrong_signer = Ed25519ReceiptSigner::from_secret_bytes([0x24; 32]);
    let wrong = ExactResolver {
        key_id: signer.key_id(),
        public: wrong_signer.public_key_bytes(),
    };
    assert_eq!(
        inspect_signed_receipt_v1(first)
            .unwrap()
            .verify(&wrong)
            .unwrap_err(),
        ReceiptError::SignerKeyIdMismatch
    );

    let mut reserved = first.to_vec();
    reserved[23] = 1;
    assert!(inspect_signed_receipt_v1(&reserved).is_err());

    let body_len = u32::from_be_bytes(first[16..20].try_into().unwrap()) as usize;
    let mut zero_key_id = first.to_vec();
    zero_key_id[WORLD_RECEIPT_HEADER_BYTES + body_len..WORLD_RECEIPT_HEADER_BYTES + body_len + 32]
        .fill(0);
    assert!(inspect_signed_receipt_v1(&zero_key_id).is_err());

    let mut bad_body = first.to_vec();
    bad_body[26] ^= 1;
    assert!(inspect_signed_receipt_v1(&bad_body).is_err());
    assert!(receipt_signing_preimage_v1(&bad_body).is_err());

    let rights = b"\x00\x06invoke\x00\x04read";
    let rights_offset = first
        .windows(rights.len())
        .position(|window| window == rights)
        .unwrap();
    let mut noncanonical = first.to_vec();
    noncanonical[rights_offset..rights_offset + rights.len()]
        .copy_from_slice(b"\x00\x04read\x00\x06invoke");
    assert!(inspect_signed_receipt_v1(&noncanonical).is_err());
}

#[test]
fn receipt_terminal_rejects_live_bearer_authority() {
    let bearer = OValue::Capability {
        kind: CapabilityKind::File,
        identity: "process-local-bearer".into(),
        metadata: HashMap::new(),
    };
    assert!(matches!(
        PortableOValue::try_from(&bearer),
        Err(HostedValueError::AuthorityBearing { .. })
    ));
}

#[test]
fn direct_failure_variant_cannot_bypass_canonical_validation() {
    let (valid, _) = sample(4, 2, 5, 6, 2, "receipt-terminal", true);
    let rebuild = |terminal| {
        ExecutionReceiptV1::new(
            valid.context().clone(),
            valid.subject().clone(),
            valid.components().to_vec(),
            valid.capabilities().to_vec(),
            valid.objects().to_vec(),
            valid.capsules().to_vec(),
            valid.effects().to_vec(),
            valid.checkpoints().to_vec(),
            terminal,
            valid.commit().clone(),
            valid.evidence().cloned(),
        )
    };
    assert!(rebuild(ReceiptTerminalV1::Failure {
        code: "Not Canonical".into(),
        detail_digest: artifact("failure"),
    })
    .is_err());
    assert!(rebuild(ReceiptTerminalV1::Failure {
        code: "failure".into(),
        detail_digest: ArtifactId::from_sha256("0".repeat(64)).unwrap(),
    })
    .is_err());
}

#[test]
fn stale_node_domain_attempt_and_object_are_rejected_before_signing() {
    let signer = Ed25519ReceiptSigner::from_secret_bytes(CONFORMANCE_SECRET);
    let (receipt, _) = sample(4, 2, 5, 6, 2, "receipt-fenced", true);

    for current in [
        sample(5, 2, 5, 6, 2, "receipt-other", true).1,
        sample(4, 3, 5, 6, 2, "receipt-other", true).1,
        sample(4, 2, 5, 7, 2, "receipt-other", true).1,
        sample(4, 2, 5, 6, 3, "receipt-other", true).1,
    ] {
        assert!(encode_signed_receipt_v1(&receipt, &current, &signer).is_err());
    }
}

#[test]
fn current_state_rejects_duplicate_logical_objects() {
    let (_, current, _, _, _, _) = ids(4, 2, 5, 6, 2, "receipt-objects");
    let first = current.objects()[0].clone();
    let newer = ObjectIdentity::new(
        first.world().clone(),
        first.object().clone(),
        ObjectVersion::new(first.version().get() + 1).unwrap(),
    );
    assert!(ReceiptCurrentStateV1::new(
        current.world().clone(),
        current.governor().clone(),
        current.node().clone(),
        current.domain().clone(),
        current.process().cloned(),
        current.attempt().clone(),
        vec![first, newer],
    )
    .is_err());
}

#[test]
fn pinned_cross_language_corpus_is_exact() {
    let corpus = corpus();
    if std::env::var_os("PRINT_WORLD_RECEIPT_FIXTURE").is_some() {
        println!("WORLD_RECEIPT_V1_HEX={}", hex::encode(&corpus));
        println!("WORLD_RECEIPT_V1_BYTES={}", corpus.len());
        println!(
            "WORLD_RECEIPT_V1_SHA256={}",
            hex::encode(Sha256::digest(&corpus))
        );
        return;
    }
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/world_receipt_v1.hex");
    let pinned = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("missing {}: {error}", path.display()));
    assert_eq!(pinned.trim(), hex::encode(&corpus));
    assert_eq!(split_corpus(&corpus).len(), 2);
}
