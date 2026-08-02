//! Canonical `OWRECEIPT` v1 framing and hosted Ed25519 provider.
//!
//! The public key is deliberately resolved outside the record. A matching
//! signature proves possession of that key; whether the key is trusted for a
//! World is caller policy and is not inferred from receipt bytes.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use super::identity::{
    ArtifactId, AttemptIdentity, CapabilityIdentity, CheckpointIdentity, DomainIdentity,
    GovernorIdentity, NodeIdentity, ObjectIdentity, ProcessIdentity, ReceiptIdentity,
    ResourceIdentity, WorldIdentity,
};
use super::identity_wire::IdentityWireRecord;
use super::receipt::{
    CapabilityObservationV1, CapsuleObservationV1, CheckpointObservationV1, ComponentKindV1,
    ComponentObservationV1, EffectObservationV1, EvidenceObservationV1, ExecutionReceiptV1,
    ObjectObservationV1, ObjectRoleV1, PlacementRejectionV1, ReceiptCommitFenceV1,
    ReceiptContextV1, ReceiptCurrentStateV1, ReceiptError, ReceiptPlacementV1, ReceiptRight,
    ReceiptSubjectV1, ReceiptTerminalV1, MAX_RECEIPT_CAPABILITIES, MAX_RECEIPT_CAPSULES,
    MAX_RECEIPT_CHECKPOINTS, MAX_RECEIPT_COMPONENTS, MAX_RECEIPT_EFFECTS,
    MAX_RECEIPT_IDENTIFIER_BYTES, MAX_RECEIPT_OBJECTS, MAX_RECEIPT_REJECTIONS, MAX_RECEIPT_RIGHTS,
};
use super::value::PortableValueRecord;

pub const WORLD_RECEIPT_MAGIC: &[u8; 8] = b"OWRCPT\0\0";
pub const WORLD_RECEIPT_SCHEMA_V1: u16 = 1;
pub const ED25519_SIGNATURE_ALGORITHM_V1: u16 = 1;
pub const WORLD_RECEIPT_HEADER_BYTES: usize = 24;
pub const WORLD_RECEIPT_KEY_ID_BYTES: usize = 32;
pub const WORLD_RECEIPT_SIGNATURE_BYTES: usize = 64;
pub const WORLD_RECEIPT_TRAILER_BYTES: usize = 96;
pub const MIN_WORLD_RECEIPT_BYTES: usize = 120;
pub const MAX_WORLD_RECEIPT_BYTES: usize = 16 * 1024;
pub const MAX_WORLD_RECEIPT_BODY_BYTES: usize =
    MAX_WORLD_RECEIPT_BYTES - WORLD_RECEIPT_HEADER_BYTES - WORLD_RECEIPT_TRAILER_BYTES;
pub const RECEIPT_SIGNING_DOMAIN_V1: &[u8; 21] = b"OSTADIX/OWRECEIPT/V1\0";
pub const RECEIPT_SIGNING_PREFIX_BYTES: usize = 61;

const DIGEST_SOURCE: u8 = 1;
const DIGEST_BUNDLE: u8 = 2;
const DIGEST_PACKAGE: u8 = 4;
const DIGEST_HGRAPH: u8 = 8;
const DIGEST_EFFECTS: u8 = 16;
const DIGEST_MASK: u8 = 31;

const TERMINAL_SUCCESS: u8 = 1;
const TERMINAL_FAILURE: u8 = 2;
const TERMINAL_CANCELLED: u8 = 3;
const TERMINAL_DEADLINE: u8 = 4;
const TERMINAL_WORLD_FAILED: u8 = 5;
const TERMINAL_WORLD_STOPPED: u8 = 6;

pub trait ReceiptKeyResolver {
    /// Resolve one independently trusted Ed25519 public key. Returning `None`
    /// means the signer is not trusted, even if its signature is mathematically
    /// valid under some other key.
    fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]>;
}

#[derive(Clone)]
pub struct Ed25519ReceiptSigner {
    signing_key: SigningKey,
}

impl Ed25519ReceiptSigner {
    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn key_id(&self) -> [u8; 32] {
        Sha256::digest(self.public_key_bytes()).into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedExecutionReceiptV1 {
    bytes: Vec<u8>,
    receipt: ExecutionReceiptV1,
    signer_key_id: [u8; 32],
    signature: [u8; 64],
}

impl SignedExecutionReceiptV1 {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn receipt(&self) -> &ExecutionReceiptV1 {
        &self.receipt
    }
    pub fn signer_key_id(&self) -> &[u8; 32] {
        &self.signer_key_id
    }
    pub fn signature(&self) -> &[u8; 64] {
        &self.signature
    }

    pub fn signing_preimage(&self) -> Result<Vec<u8>, ReceiptError> {
        receipt_signing_preimage_v1(&self.bytes)
    }

    pub fn verify(
        self,
        resolver: &impl ReceiptKeyResolver,
    ) -> Result<VerifiedExecutionReceiptV1, ReceiptError> {
        let public = resolver
            .resolve_ed25519(&self.signer_key_id)
            .ok_or_else(|| ReceiptError::UntrustedSigner(hex::encode(self.signer_key_id)))?;
        let resolved_key_id: [u8; 32] = Sha256::digest(public).into();
        if resolved_key_id != self.signer_key_id {
            return Err(ReceiptError::SignerKeyIdMismatch);
        }
        let verifying_key =
            VerifyingKey::from_bytes(&public).map_err(|_| ReceiptError::InvalidSignature)?;
        let signature = Signature::from_bytes(&self.signature);
        verifying_key
            .verify_strict(&self.signing_preimage()?, &signature)
            .map_err(|_| ReceiptError::InvalidSignature)?;
        Ok(VerifiedExecutionReceiptV1 {
            signed: self,
            signer_public_key: public,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedExecutionReceiptV1 {
    signed: SignedExecutionReceiptV1,
    signer_public_key: [u8; 32],
}

impl VerifiedExecutionReceiptV1 {
    pub fn receipt(&self) -> &ExecutionReceiptV1 {
        self.signed.receipt()
    }
    pub fn signed(&self) -> &SignedExecutionReceiptV1 {
        &self.signed
    }
    pub fn signer_public_key(&self) -> &[u8; 32] {
        &self.signer_public_key
    }
}

pub fn encode_signed_receipt_v1(
    receipt: &ExecutionReceiptV1,
    current: &ReceiptCurrentStateV1,
    signer: &Ed25519ReceiptSigner,
) -> Result<Vec<u8>, ReceiptError> {
    receipt.validate_current(current)?;
    let body = encode_body(receipt)?;
    let key_id = signer.key_id();
    let preimage = build_preimage(&body, &key_id);
    let signature = signer.signing_key.sign(&preimage).to_bytes();
    encode_envelope(&body, &key_id, &signature)
}

pub fn inspect_signed_receipt_v1(record: &[u8]) -> Result<SignedExecutionReceiptV1, ReceiptError> {
    let parsed = parse_envelope(record)?;
    let receipt = decode_body(parsed.body)?;
    let canonical_body = encode_body(&receipt)?;
    if canonical_body != parsed.body {
        return Err(ReceiptError::Malformed("unsigned body is not canonical"));
    }
    let canonical = encode_envelope(&canonical_body, &parsed.key_id, &parsed.signature)?;
    if canonical != record {
        return Err(ReceiptError::Malformed("signed envelope is not canonical"));
    }
    Ok(SignedExecutionReceiptV1 {
        bytes: record.to_vec(),
        receipt,
        signer_key_id: parsed.key_id,
        signature: parsed.signature,
    })
}

pub fn decode_signed_receipt_v1(record: &[u8]) -> Result<SignedExecutionReceiptV1, ReceiptError> {
    inspect_signed_receipt_v1(record)
}

pub fn verify_signed_receipt_v1(
    record: &[u8],
    resolver: &impl ReceiptKeyResolver,
) -> Result<VerifiedExecutionReceiptV1, ReceiptError> {
    inspect_signed_receipt_v1(record)?.verify(resolver)
}

pub fn receipt_signing_preimage_v1(record: &[u8]) -> Result<Vec<u8>, ReceiptError> {
    // A signing preimage is public only for a complete canonical receipt. The
    // raw envelope parser is intentionally insufficient here.
    let inspected = inspect_signed_receipt_v1(record)?;
    let parsed = parse_envelope(inspected.bytes())?;
    Ok(build_preimage(parsed.body, &parsed.key_id))
}

pub fn receipt_v1_sha256(record: &[u8]) -> Result<[u8; 32], ReceiptError> {
    inspect_signed_receipt_v1(record)?;
    Ok(Sha256::digest(record).into())
}

fn build_preimage(body: &[u8], key_id: &[u8; 32]) -> Vec<u8> {
    let mut output = Vec::with_capacity(RECEIPT_SIGNING_PREFIX_BYTES + body.len());
    output.extend_from_slice(RECEIPT_SIGNING_DOMAIN_V1);
    output.extend_from_slice(&WORLD_RECEIPT_SCHEMA_V1.to_be_bytes());
    output.extend_from_slice(&ED25519_SIGNATURE_ALGORITHM_V1.to_be_bytes());
    output.extend_from_slice(&(body.len() as u32).to_be_bytes());
    output.extend_from_slice(key_id);
    output.extend_from_slice(body);
    output
}

fn encode_envelope(
    body: &[u8],
    key_id: &[u8; 32],
    signature: &[u8; 64],
) -> Result<Vec<u8>, ReceiptError> {
    let total = WORLD_RECEIPT_HEADER_BYTES
        .checked_add(body.len())
        .and_then(|value| value.checked_add(WORLD_RECEIPT_TRAILER_BYTES))
        .ok_or(ReceiptError::RecordTooLarge {
            actual: usize::MAX,
            maximum: MAX_WORLD_RECEIPT_BYTES,
        })?;
    if body.len() > MAX_WORLD_RECEIPT_BODY_BYTES || total > MAX_WORLD_RECEIPT_BYTES {
        return Err(ReceiptError::RecordTooLarge {
            actual: total,
            maximum: MAX_WORLD_RECEIPT_BYTES,
        });
    }
    let mut output = Vec::with_capacity(total);
    output.extend_from_slice(WORLD_RECEIPT_MAGIC);
    put_u16(&mut output, WORLD_RECEIPT_SCHEMA_V1);
    put_u16(&mut output, ED25519_SIGNATURE_ALGORITHM_V1);
    put_u32(&mut output, total as u32);
    put_u32(&mut output, body.len() as u32);
    put_u32(&mut output, 0);
    output.extend_from_slice(body);
    output.extend_from_slice(key_id);
    output.extend_from_slice(signature);
    Ok(output)
}

struct ParsedEnvelope<'a> {
    body: &'a [u8],
    key_id: [u8; 32],
    signature: [u8; 64],
}

fn parse_envelope(record: &[u8]) -> Result<ParsedEnvelope<'_>, ReceiptError> {
    if record.len() < MIN_WORLD_RECEIPT_BYTES {
        return Err(ReceiptError::Malformed("truncated envelope"));
    }
    if record.len() > MAX_WORLD_RECEIPT_BYTES {
        return Err(ReceiptError::RecordTooLarge {
            actual: record.len(),
            maximum: MAX_WORLD_RECEIPT_BYTES,
        });
    }
    if &record[..8] != WORLD_RECEIPT_MAGIC {
        return Err(ReceiptError::Malformed("bad magic"));
    }
    let schema = u16::from_be_bytes([record[8], record[9]]);
    if schema != WORLD_RECEIPT_SCHEMA_V1 {
        return Err(ReceiptError::UnsupportedSchema(schema));
    }
    let algorithm = u16::from_be_bytes([record[10], record[11]]);
    if algorithm != ED25519_SIGNATURE_ALGORITHM_V1 {
        return Err(ReceiptError::UnsupportedSignatureAlgorithm(algorithm));
    }
    let declared = u32::from_be_bytes(record[12..16].try_into().unwrap()) as usize;
    let body_length = u32::from_be_bytes(record[16..20].try_into().unwrap()) as usize;
    let reserved = u32::from_be_bytes(record[20..24].try_into().unwrap());
    if declared != record.len() || reserved != 0 {
        return Err(ReceiptError::Malformed(
            "length mismatch or nonzero reserved field",
        ));
    }
    let expected = WORLD_RECEIPT_HEADER_BYTES
        .checked_add(body_length)
        .and_then(|value| value.checked_add(WORLD_RECEIPT_TRAILER_BYTES))
        .ok_or(ReceiptError::Malformed("envelope length overflow"))?;
    if expected != record.len() || body_length > MAX_WORLD_RECEIPT_BODY_BYTES {
        return Err(ReceiptError::Malformed("invalid unsigned body length"));
    }
    let body_end = WORLD_RECEIPT_HEADER_BYTES + body_length;
    let key_end = body_end + WORLD_RECEIPT_KEY_ID_BYTES;
    if record[body_end..key_end].iter().all(|byte| *byte == 0) {
        return Err(ReceiptError::Malformed(
            "all-zero signer key ID is reserved",
        ));
    }
    Ok(ParsedEnvelope {
        body: &record[WORLD_RECEIPT_HEADER_BYTES..body_end],
        key_id: record[body_end..key_end].try_into().unwrap(),
        signature: record[key_end..].try_into().unwrap(),
    })
}

fn encode_body(receipt: &ExecutionReceiptV1) -> Result<Vec<u8>, ReceiptError> {
    let mut output = Vec::new();
    let context = receipt.context();
    put_identity(
        &mut output,
        IdentityWireRecord::Receipt(context.receipt().clone()),
    )?;
    put_identity(
        &mut output,
        IdentityWireRecord::World(context.world().clone()),
    )?;
    put_identity(
        &mut output,
        IdentityWireRecord::Governor(context.governor().clone()),
    )?;
    put_identity(
        &mut output,
        IdentityWireRecord::Attempt(context.attempt().clone()),
    )?;
    put_identity(
        &mut output,
        IdentityWireRecord::Node(context.placement().node().clone()),
    )?;
    put_identity(
        &mut output,
        IdentityWireRecord::Domain(context.placement().domain().clone()),
    )?;
    if let Some(process) = context.placement().process() {
        put_identity(&mut output, IdentityWireRecord::Process(process.clone()))?;
    } else {
        put_u16(&mut output, 0);
    }

    let subject = receipt.subject();
    let mut mask = 0_u8;
    mask |= subject.source().is_some() as u8 * DIGEST_SOURCE;
    mask |= subject.bundle().is_some() as u8 * DIGEST_BUNDLE;
    mask |= subject.package().is_some() as u8 * DIGEST_PACKAGE;
    mask |= subject.logical_hgraph().is_some() as u8 * DIGEST_HGRAPH;
    mask |= subject.effects().is_some() as u8 * DIGEST_EFFECTS;
    output.push(mask);
    output.extend_from_slice(&[0; 3]);
    for digest in [
        subject.source(),
        subject.bundle(),
        subject.package(),
        subject.logical_hgraph(),
        subject.effects(),
    ]
    .into_iter()
    .flatten()
    {
        put_digest(&mut output, digest)?;
    }

    put_count(&mut output, receipt.components().len())?;
    for component in receipt.components() {
        output.push(component.kind() as u8);
        output.extend_from_slice(&[0; 3]);
        put_u64(&mut output, component.generation());
        put_text(&mut output, component.identity())?;
        put_digest(&mut output, component.digest())?;
    }

    put_count(&mut output, receipt.capabilities().len())?;
    for capability in receipt.capabilities() {
        put_identity(
            &mut output,
            IdentityWireRecord::Capability(capability.capability().clone()),
        )?;
        put_count(&mut output, capability.rights().len())?;
        for right in capability.rights() {
            put_text(&mut output, right.as_str())?;
        }
    }

    put_count(&mut output, receipt.objects().len())?;
    for object in receipt.objects() {
        put_identity(
            &mut output,
            IdentityWireRecord::Object(object.object().clone()),
        )?;
        output.push(object.role() as u8);
        output.extend_from_slice(&[0; 3]);
        put_digest(&mut output, object.content())?;
        put_u64(&mut output, object.bytes_len());
    }

    put_count(&mut output, receipt.capsules().len())?;
    for capsule in receipt.capsules() {
        put_digest(&mut output, capsule.digest())?;
        put_text(&mut output, capsule.evaluator())?;
        put_identity(
            &mut output,
            IdentityWireRecord::Node(capsule.affinity().clone()),
        )?;
    }

    put_count(&mut output, receipt.effects().len())?;
    for effect in receipt.effects() {
        put_identity(
            &mut output,
            IdentityWireRecord::Resource(effect.resource().clone()),
        )?;
        put_digest(&mut output, effect.before())?;
        put_digest(&mut output, effect.after())?;
    }

    put_count(&mut output, context.placement().rejected().len())?;
    for rejection in context.placement().rejected() {
        put_identity(
            &mut output,
            IdentityWireRecord::Node(rejection.node().clone()),
        )?;
        put_text(&mut output, rejection.reason())?;
    }

    put_count(&mut output, receipt.checkpoints().len())?;
    for checkpoint in receipt.checkpoints() {
        put_identity(
            &mut output,
            IdentityWireRecord::Checkpoint(checkpoint.checkpoint().clone()),
        )?;
        put_digest(&mut output, checkpoint.state())?;
        output.push(checkpoint.recovered() as u8);
        output.extend_from_slice(&[0; 3]);
    }

    match receipt.terminal() {
        ReceiptTerminalV1::Success(value) => {
            output.push(TERMINAL_SUCCESS);
            output.extend_from_slice(&[0; 3]);
            let value = value.encode()?;
            put_u32(&mut output, value.len() as u32);
            output.extend_from_slice(&value);
        }
        ReceiptTerminalV1::Failure {
            code,
            detail_digest,
        } => {
            output.push(TERMINAL_FAILURE);
            output.extend_from_slice(&[0; 3]);
            put_text(&mut output, code)?;
            put_digest(&mut output, detail_digest)?;
        }
        ReceiptTerminalV1::Cancelled => put_tag(&mut output, TERMINAL_CANCELLED),
        ReceiptTerminalV1::DeadlineExceeded => put_tag(&mut output, TERMINAL_DEADLINE),
        ReceiptTerminalV1::WorldFailed => put_tag(&mut output, TERMINAL_WORLD_FAILED),
        ReceiptTerminalV1::WorldStopped => put_tag(&mut output, TERMINAL_WORLD_STOPPED),
    }

    match receipt.commit() {
        ReceiptCommitFenceV1::Uncommitted => put_tag(&mut output, 0),
        ReceiptCommitFenceV1::Governed(governor) => {
            put_tag(&mut output, 1);
            put_identity(&mut output, IdentityWireRecord::Governor(governor.clone()))?;
        }
    }

    if let Some(evidence) = receipt.evidence() {
        put_tag(&mut output, 1);
        put_text(&mut output, evidence.gate())?;
        put_digest(&mut output, evidence.transcript())?;
    } else {
        put_tag(&mut output, 0);
    }
    if output.len() > MAX_WORLD_RECEIPT_BODY_BYTES {
        return Err(ReceiptError::RecordTooLarge {
            actual: WORLD_RECEIPT_HEADER_BYTES + output.len() + WORLD_RECEIPT_TRAILER_BYTES,
            maximum: MAX_WORLD_RECEIPT_BYTES,
        });
    }
    Ok(output)
}

fn decode_body(body: &[u8]) -> Result<ExecutionReceiptV1, ReceiptError> {
    let mut cursor = Cursor::new(body);
    let receipt = expect_receipt(cursor.identity()?)?;
    let world = expect_world(cursor.identity()?)?;
    let governor = expect_governor(cursor.identity()?)?;
    let attempt = expect_attempt(cursor.identity()?)?;
    let node = expect_node(cursor.identity()?)?;
    let domain = expect_domain(cursor.identity()?)?;
    let process = if cursor.peek_u16()? == 0 {
        cursor.u16()?;
        None
    } else {
        Some(expect_process(cursor.identity()?)?)
    };

    let mask = cursor.u8()?;
    if mask == 0 || mask & !DIGEST_MASK != 0 {
        return Err(ReceiptError::Malformed("invalid subject digest mask"));
    }
    cursor.zeros(3)?;
    let mut next_digest = |bit| -> Result<Option<ArtifactId>, ReceiptError> {
        if mask & bit != 0 {
            Ok(Some(cursor.digest()?))
        } else {
            Ok(None)
        }
    };
    let subject = ReceiptSubjectV1::new(
        next_digest(DIGEST_SOURCE)?,
        next_digest(DIGEST_BUNDLE)?,
        next_digest(DIGEST_PACKAGE)?,
        next_digest(DIGEST_HGRAPH)?,
        next_digest(DIGEST_EFFECTS)?,
    )?;

    let component_count = cursor.count(MAX_RECEIPT_COMPONENTS, "components")?;
    let mut components = Vec::with_capacity(component_count);
    for _ in 0..component_count {
        let kind = ComponentKindV1::from_u8(cursor.u8()?)?;
        cursor.zeros(3)?;
        let generation = cursor.u64()?;
        let identity = cursor.text()?;
        let digest = cursor.digest()?;
        components.push(ComponentObservationV1::new(
            kind, identity, generation, digest,
        )?);
    }

    let capability_count = cursor.count(MAX_RECEIPT_CAPABILITIES, "capabilities")?;
    let mut capabilities = Vec::with_capacity(capability_count);
    for _ in 0..capability_count {
        let capability = expect_capability(cursor.identity()?)?;
        let rights_count = cursor.count(MAX_RECEIPT_RIGHTS, "rights")?;
        if rights_count == 0 {
            return Err(ReceiptError::Malformed("capability has no rights"));
        }
        let mut rights = Vec::with_capacity(rights_count);
        for _ in 0..rights_count {
            rights.push(ReceiptRight::new(cursor.text()?)?);
        }
        capabilities.push(CapabilityObservationV1::new(capability, rights)?);
    }

    let object_count = cursor.count(MAX_RECEIPT_OBJECTS, "objects")?;
    let mut objects = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        let object = expect_object(cursor.identity()?)?;
        let role = ObjectRoleV1::from_u8(cursor.u8()?)?;
        cursor.zeros(3)?;
        let digest = cursor.digest()?;
        let bytes_len = cursor.u64()?;
        objects.push(ObjectObservationV1::new(object, role, digest, bytes_len)?);
    }

    let capsule_count = cursor.count(MAX_RECEIPT_CAPSULES, "capsules")?;
    let mut capsules = Vec::with_capacity(capsule_count);
    for _ in 0..capsule_count {
        let digest = cursor.digest()?;
        let evaluator = cursor.text()?;
        let affinity = expect_node(cursor.identity()?)?;
        capsules.push(CapsuleObservationV1::new(digest, evaluator, affinity)?);
    }

    let effect_count = cursor.count(MAX_RECEIPT_EFFECTS, "effects")?;
    let mut effects = Vec::with_capacity(effect_count);
    for _ in 0..effect_count {
        let resource = expect_resource(cursor.identity()?)?;
        let before = cursor.digest()?;
        let after = cursor.digest()?;
        effects.push(EffectObservationV1::new(resource, before, after)?);
    }

    let rejection_count = cursor.count(MAX_RECEIPT_REJECTIONS, "placement rejections")?;
    let mut rejected = Vec::with_capacity(rejection_count);
    for _ in 0..rejection_count {
        rejected.push(PlacementRejectionV1::new(
            expect_node(cursor.identity()?)?,
            cursor.text()?,
        )?);
    }

    let checkpoint_count = cursor.count(MAX_RECEIPT_CHECKPOINTS, "checkpoints")?;
    let mut checkpoints = Vec::with_capacity(checkpoint_count);
    for _ in 0..checkpoint_count {
        let checkpoint = expect_checkpoint(cursor.identity()?)?;
        let state = cursor.digest()?;
        let recovered = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return Err(ReceiptError::Malformed("invalid recovery flag")),
        };
        cursor.zeros(3)?;
        checkpoints.push(CheckpointObservationV1::new(checkpoint, state, recovered)?);
    }

    let terminal_tag = cursor.u8()?;
    cursor.zeros(3)?;
    let terminal = match terminal_tag {
        TERMINAL_SUCCESS => {
            let length = cursor.u32()? as usize;
            let value = PortableValueRecord::decode(cursor.take(length)?)?;
            ReceiptTerminalV1::Success(value)
        }
        TERMINAL_FAILURE => ReceiptTerminalV1::failure(cursor.text()?, cursor.digest()?)?,
        TERMINAL_CANCELLED => ReceiptTerminalV1::Cancelled,
        TERMINAL_DEADLINE => ReceiptTerminalV1::DeadlineExceeded,
        TERMINAL_WORLD_FAILED => ReceiptTerminalV1::WorldFailed,
        TERMINAL_WORLD_STOPPED => ReceiptTerminalV1::WorldStopped,
        _ => return Err(ReceiptError::Malformed("unknown terminal tag")),
    };

    let commit_tag = cursor.u8()?;
    cursor.zeros(3)?;
    let commit = match commit_tag {
        0 => ReceiptCommitFenceV1::Uncommitted,
        1 => ReceiptCommitFenceV1::Governed(expect_governor(cursor.identity()?)?),
        _ => return Err(ReceiptError::Malformed("unknown commit tag")),
    };

    let evidence_tag = cursor.u8()?;
    cursor.zeros(3)?;
    let evidence = match evidence_tag {
        0 => None,
        1 => Some(EvidenceObservationV1::new(
            cursor.text()?,
            cursor.digest()?,
        )?),
        _ => return Err(ReceiptError::Malformed("unknown evidence tag")),
    };
    if !cursor.is_empty() {
        return Err(ReceiptError::Malformed("trailing unsigned body bytes"));
    }

    let placement = ReceiptPlacementV1::new(node, domain, process, rejected)?;
    let context = ReceiptContextV1::new(receipt, world, governor, attempt, placement)?;
    ExecutionReceiptV1::new(
        context,
        subject,
        components,
        capabilities,
        objects,
        capsules,
        effects,
        checkpoints,
        terminal,
        commit,
        evidence,
    )
}

fn put_identity(output: &mut Vec<u8>, record: IdentityWireRecord) -> Result<(), ReceiptError> {
    let bytes = record.encode()?;
    let length = u16::try_from(bytes.len())
        .map_err(|_| ReceiptError::Malformed("nested identity exceeds u16 length"))?;
    put_u16(output, length);
    output.extend_from_slice(&bytes);
    Ok(())
}

fn put_digest(output: &mut Vec<u8>, digest: &ArtifactId) -> Result<(), ReceiptError> {
    let bytes = hex::decode(digest.as_sha256())
        .map_err(|_| ReceiptError::Malformed("invalid artifact digest"))?;
    if bytes.len() != 32 {
        return Err(ReceiptError::Malformed("artifact digest is not SHA-256"));
    }
    output.extend_from_slice(&bytes);
    Ok(())
}

fn put_text(output: &mut Vec<u8>, value: &str) -> Result<(), ReceiptError> {
    if value.is_empty() || value.len() > MAX_RECEIPT_IDENTIFIER_BYTES {
        return Err(ReceiptError::Malformed("invalid bounded identifier"));
    }
    put_u16(output, value.len() as u16);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_count(output: &mut Vec<u8>, count: usize) -> Result<(), ReceiptError> {
    let count = u16::try_from(count).map_err(|_| ReceiptError::Malformed("count overflow"))?;
    put_u16(output, count);
    Ok(())
}

fn put_tag(output: &mut Vec<u8>, value: u8) {
    output.push(value);
    output.extend_from_slice(&[0; 3]);
}
fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}
fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}
fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, length: usize) -> Result<&'a [u8], ReceiptError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ReceiptError::Malformed("body offset overflow"))?;
        if end > self.bytes.len() {
            return Err(ReceiptError::Malformed("truncated unsigned body"));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, ReceiptError> {
        Ok(self.take(1)?[0])
    }
    fn peek_u16(&self) -> Result<u16, ReceiptError> {
        if self.offset + 2 > self.bytes.len() {
            return Err(ReceiptError::Malformed("truncated u16"));
        }
        Ok(u16::from_be_bytes(
            self.bytes[self.offset..self.offset + 2].try_into().unwrap(),
        ))
    }
    fn u16(&mut self) -> Result<u16, ReceiptError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, ReceiptError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, ReceiptError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn zeros(&mut self, length: usize) -> Result<(), ReceiptError> {
        if self.take(length)?.iter().any(|byte| *byte != 0) {
            return Err(ReceiptError::Malformed("nonzero reserved field"));
        }
        Ok(())
    }
    fn text(&mut self) -> Result<String, ReceiptError> {
        let length = self.u16()? as usize;
        if length == 0 || length > MAX_RECEIPT_IDENTIFIER_BYTES {
            return Err(ReceiptError::Malformed("invalid text length"));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| ReceiptError::Malformed("text is not UTF-8"))
    }
    fn digest(&mut self) -> Result<ArtifactId, ReceiptError> {
        ArtifactId::from_sha256(hex::encode(self.take(32)?)).map_err(ReceiptError::from)
    }
    fn identity(&mut self) -> Result<IdentityWireRecord, ReceiptError> {
        let length = self.u16()? as usize;
        if length == 0 {
            return Err(ReceiptError::Malformed("missing required identity"));
        }
        Ok(IdentityWireRecord::decode(self.take(length)?)?)
    }
    fn count(&mut self, maximum: usize, kind: &'static str) -> Result<usize, ReceiptError> {
        let actual = self.u16()? as usize;
        if actual > maximum {
            return Err(ReceiptError::Limit {
                kind,
                actual,
                maximum,
            });
        }
        Ok(actual)
    }
    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

macro_rules! expect_identity {
    ($name:ident, $variant:ident, $ty:ty) => {
        fn $name(record: IdentityWireRecord) -> Result<$ty, ReceiptError> {
            match record {
                IdentityWireRecord::$variant(value) => Ok(value),
                _ => Err(ReceiptError::Malformed(concat!(
                    "wrong nested identity kind for ",
                    stringify!($variant)
                ))),
            }
        }
    };
}

expect_identity!(expect_receipt, Receipt, ReceiptIdentity);
expect_identity!(expect_world, World, WorldIdentity);
expect_identity!(expect_governor, Governor, GovernorIdentity);
expect_identity!(expect_attempt, Attempt, AttemptIdentity);
expect_identity!(expect_node, Node, NodeIdentity);
expect_identity!(expect_domain, Domain, DomainIdentity);
expect_identity!(expect_process, Process, ProcessIdentity);
expect_identity!(expect_capability, Capability, CapabilityIdentity);
expect_identity!(expect_object, Object, ObjectIdentity);
expect_identity!(expect_resource, Resource, ResourceIdentity);
expect_identity!(expect_checkpoint, Checkpoint, CheckpointIdentity);
