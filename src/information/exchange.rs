use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::id::domain_digest;
use super::{
    canonical_bytes, EntityDescriptorV1, InformationAtomV1, InformationDeltaV1, InformationErrorV1,
    InformationObjectKindV1, InformationRevisionV1, InformationSnapshotV1,
};

pub const INFORMATION_DELTA_PACK_SCHEMA_V1: &str = "ostadix.info-delta-pack/v1";
pub const SIGNED_INFORMATION_DELTA_PACK_SCHEMA_V1: &str = "ostadix.signed-info-delta-pack/v1";
const PACK_SIGNING_DOMAIN_V1: &[u8] = b"OSTADIX/INFORMATION-DELTA-PACK/V1\0";
const PACK_KEY_ID_DOMAIN_V1: &[u8] = b"OSTADIX/INFORMATION-PACK-KEY-ID/V1\0";
pub const MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1: usize = 1_024;
pub const MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1: usize = 256 * 1024;
pub const MAX_OFFLINE_INFORMATION_PACK_BODY_BYTES_V1: usize = 768 * 1024;
pub const MAX_SIGNED_INFORMATION_PACK_BYTES_V1: usize = 1024 * 1024;
const MAX_INFORMATION_DECODE_ITEMS: usize = 1_000_000;
const MAX_INFORMATION_DECODE_DEPTH: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackedInformationObjectV1 {
    pub kind: InformationObjectKindV1,
    pub sha256: String,
    pub canonical_bytes: Vec<u8>,
}

impl PackedInformationObjectV1 {
    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        let actual = domain_digest(self.kind.domain(), &self.canonical_bytes);
        if actual == self.sha256 {
            Ok(())
        } else {
            Err(InformationErrorV1::ObjectDigestMismatch {
                expected: self.sha256.clone(),
                actual,
            })
        }
    }

    pub fn from_entity(value: &EntityDescriptorV1) -> Result<Self, InformationErrorV1> {
        value.validate()?;
        pack_typed(
            InformationObjectKindV1::Entity,
            value.id()?.as_sha256(),
            value,
        )
    }

    pub fn from_atom(value: &InformationAtomV1) -> Result<Self, InformationErrorV1> {
        value.validate()?;
        pack_typed(
            InformationObjectKindV1::Atom,
            value.id()?.as_sha256(),
            value,
        )
    }

    pub fn from_snapshot(value: &InformationSnapshotV1) -> Result<Self, InformationErrorV1> {
        value.validate()?;
        pack_typed(
            InformationObjectKindV1::Snapshot,
            value.id()?.as_sha256(),
            value,
        )
    }

    pub fn from_revision(value: &InformationRevisionV1) -> Result<Self, InformationErrorV1> {
        value.validate()?;
        pack_typed(
            InformationObjectKindV1::Revision,
            value.id()?.as_sha256(),
            value,
        )
    }

    pub fn from_delta(value: &InformationDeltaV1) -> Result<Self, InformationErrorV1> {
        value.validate()?;
        pack_typed(
            InformationObjectKindV1::Delta,
            value.id()?.as_sha256(),
            value,
        )
    }

    /// Decode and validate the bounded object vocabulary accepted for a
    /// current local head. Unsupported kinds may still be retained as signed
    /// historical packs, but they cannot be promoted by this V1 validator.
    pub fn decode_typed(&self) -> Result<TypedInformationObjectV1, InformationErrorV1> {
        self.validate()?;
        match self.kind {
            InformationObjectKindV1::Entity => {
                let value: EntityDescriptorV1 = decode_information_canonical(
                    &self.canonical_bytes,
                    MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1,
                )?;
                value.validate()?;
                if value.id()?.as_sha256() != self.sha256 {
                    return Err(typed_identity_mismatch(self));
                }
                Ok(TypedInformationObjectV1::Entity(value))
            }
            InformationObjectKindV1::Atom => {
                let value: InformationAtomV1 = decode_information_canonical(
                    &self.canonical_bytes,
                    MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1,
                )?;
                value.validate()?;
                if value.id()?.as_sha256() != self.sha256 {
                    return Err(typed_identity_mismatch(self));
                }
                Ok(TypedInformationObjectV1::Atom(Box::new(value)))
            }
            InformationObjectKindV1::Snapshot => {
                let value: InformationSnapshotV1 = decode_information_canonical(
                    &self.canonical_bytes,
                    MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1,
                )?;
                value.validate()?;
                if value.id()?.as_sha256() != self.sha256 {
                    return Err(typed_identity_mismatch(self));
                }
                Ok(TypedInformationObjectV1::Snapshot(value))
            }
            InformationObjectKindV1::Revision => {
                let value: InformationRevisionV1 = decode_information_canonical(
                    &self.canonical_bytes,
                    MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1,
                )?;
                value.validate()?;
                if value.id()?.as_sha256() != self.sha256 {
                    return Err(typed_identity_mismatch(self));
                }
                Ok(TypedInformationObjectV1::Revision(value))
            }
            InformationObjectKindV1::Delta => {
                let value: InformationDeltaV1 = decode_information_canonical(
                    &self.canonical_bytes,
                    MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1,
                )?;
                value.validate()?;
                if value.id()?.as_sha256() != self.sha256 {
                    return Err(typed_identity_mismatch(self));
                }
                Ok(TypedInformationObjectV1::Delta(value))
            }
            kind => Err(InformationErrorV1::InvalidRecord(format!(
                "{kind:?} objects are outside the bounded typed offline-import vocabulary"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypedInformationObjectV1 {
    Entity(EntityDescriptorV1),
    Atom(Box<InformationAtomV1>),
    Snapshot(InformationSnapshotV1),
    Revision(InformationRevisionV1),
    Delta(InformationDeltaV1),
}

fn pack_typed<T: Serialize>(
    kind: InformationObjectKindV1,
    expected_sha256: &str,
    value: &T,
) -> Result<PackedInformationObjectV1, InformationErrorV1> {
    let object = PackedInformationObjectV1 {
        kind,
        sha256: expected_sha256.to_string(),
        canonical_bytes: canonical_bytes(value)?,
    };
    object.validate()?;
    Ok(object)
}

fn decode_information_canonical<T: DeserializeOwned + Serialize>(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<T, InformationErrorV1> {
    let decoded: T = crate::canonical_cbor::decode_bounded(
        bytes,
        crate::canonical_cbor::DecodeLimits {
            max_bytes,
            max_items: MAX_INFORMATION_DECODE_ITEMS,
            max_depth: MAX_INFORMATION_DECODE_DEPTH,
        },
    )
    .map_err(|error| InformationErrorV1::Canonical(error.to_string()))?;
    if canonical_bytes(&decoded)? != bytes {
        return Err(InformationErrorV1::InvalidRecord(
            "information object encoding is not canonical".to_string(),
        ));
    }
    Ok(decoded)
}

fn typed_identity_mismatch(object: &PackedInformationObjectV1) -> InformationErrorV1 {
    InformationErrorV1::InvalidRecord(format!(
        "typed {:?} identity does not match packed object digest {}",
        object.kind, object.sha256
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OfflinePackPolicyV1 {
    pub allow_managed_blobs: bool,
    pub max_objects: usize,
    pub max_canonical_bytes: usize,
}

impl Default for OfflinePackPolicyV1 {
    fn default() -> Self {
        Self {
            allow_managed_blobs: false,
            max_objects: MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1,
            max_canonical_bytes: MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InformationDeltaPackV1 {
    schema: String,
    delta: InformationDeltaV1,
    objects: Vec<PackedInformationObjectV1>,
}

impl InformationDeltaPackV1 {
    pub fn new(
        delta: InformationDeltaV1,
        mut objects: Vec<PackedInformationObjectV1>,
        policy: OfflinePackPolicyV1,
    ) -> Result<Self, InformationErrorV1> {
        objects.sort();
        objects.dedup();
        let pack = Self {
            schema: INFORMATION_DELTA_PACK_SCHEMA_V1.to_string(),
            delta,
            objects,
        };
        pack.validate(policy)?;
        Ok(pack)
    }

    pub fn validate(&self, policy: OfflinePackPolicyV1) -> Result<(), InformationErrorV1> {
        self.validate_hard_bounds()?;
        let maximum_objects = policy
            .max_objects
            .min(MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1);
        let maximum_canonical_bytes = policy
            .max_canonical_bytes
            .min(MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1);
        if self.objects.len() > maximum_objects {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "information delta pack has {} objects; maximum is {maximum_objects}",
                self.objects.len()
            )));
        }
        let mut total = 0_usize;
        for object in &self.objects {
            if object.kind == InformationObjectKindV1::Blob && !policy.allow_managed_blobs {
                return Err(InformationErrorV1::ForbiddenPayload(
                    "generic managed blobs are disabled for offline packs".to_string(),
                ));
            }
            total = total
                .checked_add(object.canonical_bytes.len())
                .ok_or_else(|| {
                    InformationErrorV1::InvalidRecord(
                        "information delta pack byte count overflow".to_string(),
                    )
                })?;
            if total > maximum_canonical_bytes {
                return Err(InformationErrorV1::InvalidRecord(format!(
                    "information delta pack has {total} canonical object bytes; maximum is {maximum_canonical_bytes}"
                )));
            }
        }
        Ok(())
    }

    fn validate_hard_bounds(&self) -> Result<(), InformationErrorV1> {
        if self.schema != INFORMATION_DELTA_PACK_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported information delta pack schema `{}`",
                self.schema
            )));
        }
        self.delta.validate()?;
        if self.objects.len() > MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "information delta pack has {} objects; maximum is {}",
                self.objects.len(),
                MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1
            )));
        }
        let mut normalized_objects = self.objects.clone();
        normalized_objects.sort();
        normalized_objects.dedup();
        if normalized_objects != self.objects {
            return Err(InformationErrorV1::InvalidRecord(
                "information delta pack objects are not normalized".to_string(),
            ));
        }
        for object in &self.objects {
            if object.canonical_bytes.len() > MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1 {
                return Err(InformationErrorV1::InvalidRecord(format!(
                    "packed information object has {} canonical bytes; maximum is {}",
                    object.canonical_bytes.len(),
                    MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1
                )));
            }
            object.validate()?;
        }
        let body_len = canonical_bytes(self)?.len();
        if body_len > MAX_OFFLINE_INFORMATION_PACK_BODY_BYTES_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "information delta pack body has {body_len} canonical bytes; maximum is {MAX_OFFLINE_INFORMATION_PACK_BODY_BYTES_V1}"
            )));
        }
        Ok(())
    }

    pub fn delta(&self) -> &InformationDeltaV1 {
        &self.delta
    }

    pub fn objects(&self) -> &[PackedInformationObjectV1] {
        &self.objects
    }
}

pub trait InformationPackKeyResolverV1 {
    /// Return a public key trusted by the caller's independent local policy.
    fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]>;
}

#[derive(Clone)]
pub struct InformationPackSignerV1 {
    signing_key: SigningKey,
}

impl std::fmt::Debug for InformationPackSignerV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InformationPackSignerV1([redacted])")
    }
}

impl InformationPackSignerV1 {
    pub fn generate() -> Result<Self, InformationErrorV1> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| {
            InformationErrorV1::Signature(format!(
                "failed to obtain entropy for information pack key: {error}"
            ))
        })?;
        Ok(Self::from_secret_bytes(secret))
    }

    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn key_id(&self) -> [u8; 32] {
        information_pack_key_id_v1(&self.public_key())
    }

    pub fn sign(
        &self,
        pack: InformationDeltaPackV1,
    ) -> Result<SignedInformationDeltaPackV1, InformationErrorV1> {
        pack.validate_hard_bounds()?;
        let body = canonical_bytes(&pack)?;
        let signature = self.signing_key.sign(&signing_preimage(&body));
        Ok(SignedInformationDeltaPackV1 {
            schema: SIGNED_INFORMATION_DELTA_PACK_SCHEMA_V1.to_string(),
            pack,
            signer_public_key: hex::encode(self.public_key()),
            signer_key_id: hex::encode(self.key_id()),
            signature: hex::encode(signature.to_bytes()),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedInformationDeltaPackV1 {
    schema: String,
    pack: InformationDeltaPackV1,
    signer_public_key: String,
    signer_key_id: String,
    signature: String,
}

impl SignedInformationDeltaPackV1 {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, InformationErrorV1> {
        self.validate_envelope_shape()?;
        let bytes = canonical_bytes(self)?;
        if bytes.len() > MAX_SIGNED_INFORMATION_PACK_BYTES_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "signed information delta pack has {} canonical bytes; maximum is {MAX_SIGNED_INFORMATION_PACK_BYTES_V1}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, InformationErrorV1> {
        let decoded: Self =
            decode_information_canonical(bytes, MAX_SIGNED_INFORMATION_PACK_BYTES_V1)?;
        decoded.validate_envelope_shape()?;
        Ok(decoded)
    }

    fn validate_envelope_shape(&self) -> Result<(), InformationErrorV1> {
        if self.schema != SIGNED_INFORMATION_DELTA_PACK_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported signed information pack schema `{}`",
                self.schema
            )));
        }
        self.pack.validate_hard_bounds()?;
        let public = decode_fixed_hex::<32>("signer_public_key", &self.signer_public_key)?;
        let encoded_key_id = decode_fixed_hex::<32>("signer_key_id", &self.signer_key_id)?;
        decode_fixed_hex::<64>("signature", &self.signature)?;
        if information_pack_key_id_v1(&public) != encoded_key_id {
            return Err(InformationErrorV1::Signature(
                "information pack signer key identifier mismatch".to_string(),
            ));
        }
        Ok(())
    }

    pub fn verify(
        self,
        resolver: &impl InformationPackKeyResolverV1,
        policy: OfflinePackPolicyV1,
        trust_policy_sha256: impl Into<String>,
    ) -> Result<VerifiedInformationDeltaPackV1, InformationErrorV1> {
        self.validate_envelope_shape()?;
        self.pack.validate(policy)?;
        let public = decode_fixed_hex::<32>("signer_public_key", &self.signer_public_key)?;
        let encoded_key_id = decode_fixed_hex::<32>("signer_key_id", &self.signer_key_id)?;
        let expected_key_id = information_pack_key_id_v1(&public);
        if expected_key_id != encoded_key_id {
            return Err(InformationErrorV1::Signature(
                "information pack signer key identifier mismatch".to_string(),
            ));
        }
        let trusted_public = resolver
            .resolve_ed25519(&encoded_key_id)
            .ok_or_else(|| InformationErrorV1::UntrustedSigner(self.signer_key_id.clone()))?;
        if trusted_public != public {
            return Err(InformationErrorV1::Signature(
                "information pack resolver returned a different public key".to_string(),
            ));
        }
        let verifying_key = VerifyingKey::from_bytes(&public).map_err(|error| {
            InformationErrorV1::Signature(format!(
                "information pack public key is invalid: {error}"
            ))
        })?;
        let signature = decode_fixed_hex::<64>("signature", &self.signature)?;
        let body = canonical_bytes(&self.pack)?;
        verifying_key
            .verify_strict(&signing_preimage(&body), &Signature::from_bytes(&signature))
            .map_err(|_| {
                InformationErrorV1::Signature(
                    "information delta pack signature is invalid".to_string(),
                )
            })?;
        let trust_policy_sha256 = trust_policy_sha256.into();
        validate_plain_sha256("trust_policy_sha256", &trust_policy_sha256)?;
        Ok(VerifiedInformationDeltaPackV1 {
            signed: self,
            trust_policy_sha256,
        })
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedInformationDeltaPackV1 {
    signed: SignedInformationDeltaPackV1,
    trust_policy_sha256: String,
}

impl VerifiedInformationDeltaPackV1 {
    pub fn pack(&self) -> &InformationDeltaPackV1 {
        &self.signed.pack
    }

    pub fn signer_key_id(&self) -> &str {
        &self.signed.signer_key_id
    }

    pub fn trust_policy_sha256(&self) -> &str {
        &self.trust_policy_sha256
    }
}

pub fn information_pack_key_id_v1(public: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(PACK_KEY_ID_DOMAIN_V1);
    hash.update(public);
    hash.finalize().into()
}

fn signing_preimage(body: &[u8]) -> Vec<u8> {
    let mut preimage = Vec::with_capacity(PACK_SIGNING_DOMAIN_V1.len() + 8 + body.len());
    preimage.extend_from_slice(PACK_SIGNING_DOMAIN_V1);
    preimage.extend_from_slice(&(body.len() as u64).to_be_bytes());
    preimage.extend_from_slice(body);
    preimage
}

fn validate_plain_sha256(label: &str, value: &str) -> Result<(), InformationErrorV1> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(InformationErrorV1::InvalidRecord(format!(
            "{label} must be lowercase sha256"
        )))
    }
}

fn decode_fixed_hex<const N: usize>(
    label: &str,
    encoded: &str,
) -> Result<[u8; N], InformationErrorV1> {
    if encoded.len() != N * 2
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(InformationErrorV1::Signature(format!(
            "{label} must be lowercase hexadecimal with {} bytes",
            N
        )));
    }
    let bytes = hex::decode(encoded)
        .map_err(|_| InformationErrorV1::Signature(format!("{label} is not valid hexadecimal")))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        InformationErrorV1::Signature(format!("{label} has {} bytes; expected {N}", bytes.len()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::information::{AtomIdV1, EntityIdV1, RevisionIdV1};

    struct Resolver {
        key_id: [u8; 32],
        public: [u8; 32],
    }

    impl InformationPackKeyResolverV1 for Resolver {
        fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]> {
            (*key_id == self.key_id).then_some(self.public)
        }
    }

    fn pack(policy: OfflinePackPolicyV1) -> InformationDeltaPackV1 {
        let atom_bytes = b"atom".to_vec();
        let atom_sha256 = domain_digest(InformationObjectKindV1::Atom.domain(), &atom_bytes);
        let delta = InformationDeltaV1::new(
            RevisionIdV1::from_sha256("11".repeat(32)).unwrap(),
            EntityIdV1::from_sha256("22".repeat(32)).unwrap(),
            vec![AtomIdV1::from_sha256(atom_sha256.clone()).unwrap()],
            vec![],
        )
        .unwrap();
        InformationDeltaPackV1::new(
            delta,
            vec![PackedInformationObjectV1 {
                kind: InformationObjectKindV1::Atom,
                sha256: atom_sha256,
                canonical_bytes: atom_bytes,
            }],
            policy,
        )
        .unwrap()
    }

    #[test]
    fn signed_pack_round_trips_and_keeps_trust_separate() {
        let policy = OfflinePackPolicyV1::default();
        let signer = InformationPackSignerV1::from_secret_bytes([7; 32]);
        let signed = signer.sign(pack(policy)).unwrap();
        let bytes = signed.canonical_bytes().unwrap();
        let decoded = SignedInformationDeltaPackV1::decode_canonical(&bytes).unwrap();
        let resolver = Resolver {
            key_id: signer.key_id(),
            public: signer.public_key(),
        };
        let verified = decoded.verify(&resolver, policy, "33".repeat(32)).unwrap();
        assert_eq!(verified.signer_key_id(), hex::encode(signer.key_id()));
        assert_eq!(verified.trust_policy_sha256(), "33".repeat(32));
    }

    #[test]
    fn signature_mutation_and_untrusted_signers_fail() {
        let policy = OfflinePackPolicyV1::default();
        let signer = InformationPackSignerV1::from_secret_bytes([7; 32]);
        let mut signed = signer.sign(pack(policy)).unwrap();
        let replacement = if signed.signature.starts_with("00") {
            "01"
        } else {
            "00"
        };
        signed.signature.replace_range(0..2, replacement);
        let resolver = Resolver {
            key_id: signer.key_id(),
            public: signer.public_key(),
        };
        assert!(signed
            .clone()
            .verify(&resolver, policy, "33".repeat(32))
            .is_err());
        let untrusted = Resolver {
            key_id: [0; 32],
            public: signer.public_key(),
        };
        assert!(signer
            .sign(pack(policy))
            .unwrap()
            .verify(&untrusted, policy, "33".repeat(32))
            .is_err());
    }

    #[test]
    fn bounded_pack_decode_rejects_impossible_lengths_before_allocation() {
        let mut bytes = vec![0x9b];
        bytes.extend_from_slice(&u64::MAX.to_be_bytes());
        let error = SignedInformationDeltaPackV1::decode_canonical(&bytes).unwrap_err();
        assert!(error.to_string().contains("declares"));
    }

    #[test]
    fn bounded_pack_decode_rejects_excessive_nesting() {
        let mut bytes = vec![0x81; MAX_INFORMATION_DECODE_DEPTH + 2];
        bytes.push(0xf6);
        let error = SignedInformationDeltaPackV1::decode_canonical(&bytes).unwrap_err();
        assert!(error.to_string().contains("nesting depth"));
    }

    #[test]
    fn default_hard_limit_constructs_signs_and_decodes() {
        let object_bytes = vec![0xff; MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1];
        let object_sha256 = domain_digest(InformationObjectKindV1::Atom.domain(), &object_bytes);
        let delta = InformationDeltaV1::new(
            RevisionIdV1::from_sha256("11".repeat(32)).unwrap(),
            EntityIdV1::from_sha256("22".repeat(32)).unwrap(),
            vec![AtomIdV1::from_sha256(object_sha256.clone()).unwrap()],
            vec![],
        )
        .unwrap();
        let pack = InformationDeltaPackV1::new(
            delta,
            vec![PackedInformationObjectV1 {
                kind: InformationObjectKindV1::Atom,
                sha256: object_sha256,
                canonical_bytes: object_bytes,
            }],
            OfflinePackPolicyV1::default(),
        )
        .unwrap();
        let signer = InformationPackSignerV1::from_secret_bytes([17; 32]);
        let bytes = signer.sign(pack).unwrap().canonical_bytes().unwrap();
        assert!(bytes.len() <= MAX_SIGNED_INFORMATION_PACK_BYTES_V1);
        SignedInformationDeltaPackV1::decode_canonical(&bytes).unwrap();
    }

    #[test]
    fn combined_default_object_count_and_byte_limits_are_decode_closed() {
        let bytes_per_object =
            MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1 / MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1;
        assert_eq!(
            bytes_per_object * MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1,
            MAX_OFFLINE_INFORMATION_OBJECT_BYTES_V1
        );
        let mut additions = Vec::with_capacity(MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1);
        let mut objects = Vec::with_capacity(MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1);
        for ordinal in 0..MAX_OFFLINE_INFORMATION_PACK_OBJECTS_V1 {
            let mut object_bytes = vec![0xff; bytes_per_object];
            object_bytes[..8].copy_from_slice(&(ordinal as u64).to_be_bytes());
            let object_sha256 =
                domain_digest(InformationObjectKindV1::Atom.domain(), &object_bytes);
            additions.push(AtomIdV1::from_sha256(object_sha256.clone()).unwrap());
            objects.push(PackedInformationObjectV1 {
                kind: InformationObjectKindV1::Atom,
                sha256: object_sha256,
                canonical_bytes: object_bytes,
            });
        }
        let delta = InformationDeltaV1::new(
            RevisionIdV1::from_sha256("11".repeat(32)).unwrap(),
            EntityIdV1::from_sha256("22".repeat(32)).unwrap(),
            additions,
            vec![],
        )
        .unwrap();
        let pack =
            InformationDeltaPackV1::new(delta, objects, OfflinePackPolicyV1::default()).unwrap();
        let signer = InformationPackSignerV1::from_secret_bytes([19; 32]);
        let bytes = signer.sign(pack).unwrap().canonical_bytes().unwrap();
        assert!(bytes.len() <= MAX_SIGNED_INFORMATION_PACK_BYTES_V1);
        SignedInformationDeltaPackV1::decode_canonical(&bytes).unwrap();
    }

    #[test]
    fn verification_rejects_invalid_or_nonnormalized_delta_before_signature() {
        let policy = OfflinePackPolicyV1::default();
        let signer = InformationPackSignerV1::from_secret_bytes([7; 32]);
        let resolver = Resolver {
            key_id: signer.key_id(),
            public: signer.public_key(),
        };

        let mut wrong_schema = serde_json::to_value(signer.sign(pack(policy)).unwrap()).unwrap();
        wrong_schema["pack"]["delta"]["schema"] =
            serde_json::Value::String("ostadix.info-delta/v0".to_string());
        let wrong_schema: SignedInformationDeltaPackV1 =
            serde_json::from_value(wrong_schema).unwrap();
        assert!(wrong_schema
            .verify(&resolver, policy, "33".repeat(32))
            .unwrap_err()
            .to_string()
            .contains("unsupported information delta schema"));

        let mut nonnormalized = serde_json::to_value(signer.sign(pack(policy)).unwrap()).unwrap();
        let additions = nonnormalized["pack"]["delta"]["additions"]
            .as_array_mut()
            .unwrap();
        additions.push(additions[0].clone());
        let nonnormalized: SignedInformationDeltaPackV1 =
            serde_json::from_value(nonnormalized).unwrap();
        assert!(nonnormalized
            .verify(&resolver, policy, "33".repeat(32))
            .unwrap_err()
            .to_string()
            .contains("not in normalized canonical form"));
    }

    #[test]
    fn verification_rejects_noncanonical_signer_hex() {
        fn uppercase_one(value: &mut String) {
            let offset = value
                .bytes()
                .position(|byte| (b'a'..=b'f').contains(&byte))
                .expect("fixture digest contains a hexadecimal letter");
            let uppercase = (value.as_bytes()[offset] as char)
                .to_ascii_uppercase()
                .to_string();
            value.replace_range(offset..=offset, &uppercase);
        }

        let policy = OfflinePackPolicyV1::default();
        let signer = InformationPackSignerV1::from_secret_bytes([7; 32]);
        let resolver = Resolver {
            key_id: signer.key_id(),
            public: signer.public_key(),
        };
        let mut public = signer.sign(pack(policy)).unwrap();
        uppercase_one(&mut public.signer_public_key);
        assert!(public
            .verify(&resolver, policy, "33".repeat(32))
            .unwrap_err()
            .to_string()
            .contains("lowercase hexadecimal"));

        let mut key_id = signer.sign(pack(policy)).unwrap();
        uppercase_one(&mut key_id.signer_key_id);
        assert!(key_id
            .verify(&resolver, policy, "33".repeat(32))
            .unwrap_err()
            .to_string()
            .contains("lowercase hexadecimal"));
    }
}
