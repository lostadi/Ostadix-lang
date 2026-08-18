use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use super::model::{
    NamespaceDelegationV1, NamespaceRootV1, ProfilePublicationV1, ProfileStalenessPolicyV1,
    RegistryEventBodyV1, RegistryEventV1, RegistryPublicKeyV1, RegistrySnapshotV1, RegistryStoreV1,
    RegistryTrustV1, SignedRegistryEventV1,
};
use super::{canonical_registry_bytes, registry_event_sha256, RegistryError};

const REGISTRY_SIGNING_DOMAIN_V1: &[u8] = b"OSTADIX/REGISTRY-EVENT/V1\0";
const REGISTRY_KEY_ID_DOMAIN_V1: &[u8] = b"OSTADIX/REGISTRY-ED25519-KEY/V1\0";

/// In-memory Ed25519 authority. Debug is intentionally redacted and secret
/// bytes are exposed only to the persistence module.
#[derive(Clone)]
pub struct RegistrySignerV1 {
    signing_key: SigningKey,
}

impl std::fmt::Debug for RegistrySignerV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RegistrySignerV1([redacted])")
    }
}

impl RegistrySignerV1 {
    pub fn generate() -> Result<Self, RegistryError> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| {
            RegistryError::Canonical(format!("operating-system entropy failed: {error}"))
        })?;
        Ok(Self::from_secret_bytes(secret))
    }

    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public_key(&self) -> RegistryPublicKeyV1 {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn public_key_id(&self) -> [u8; 32] {
        registry_public_key_id(&self.public_key())
    }

    pub(crate) fn secret_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    fn sign_event(&self, event: RegistryEventV1) -> Result<SignedRegistryEventV1, RegistryError> {
        let preimage = registry_signing_preimage(&event)?;
        Ok(SignedRegistryEventV1::new(
            event,
            self.signing_key.sign(&preimage).to_bytes(),
        ))
    }
}

pub fn registry_public_key_id(public_key: &RegistryPublicKeyV1) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(REGISTRY_KEY_ID_DOMAIN_V1);
    hasher.update(public_key);
    hasher.finalize().into()
}

pub fn create_registry_root(
    namespace: impl Into<String>,
    valid_from_ms: u64,
    expires_at_ms: u64,
    signer: &RegistrySignerV1,
) -> Result<RegistrySnapshotV1, RegistryError> {
    let namespace = namespace.into();
    let root = NamespaceRootV1::new(
        namespace.clone(),
        signer.public_key(),
        valid_from_ms,
        expires_at_ms,
    )?;
    let event = RegistryEventV1::new(
        1,
        None,
        valid_from_ms,
        namespace,
        signer.public_key(),
        RegistryEventBodyV1::NamespaceRoot(root),
    )?;
    Ok(RegistrySnapshotV1::new(signer.sign_event(event)?))
}

pub fn append_namespace_delegation(
    snapshot: &mut RegistrySnapshotV1,
    delegation: NamespaceDelegationV1,
    issued_at_ms: u64,
    signer: &RegistrySignerV1,
) -> Result<(), RegistryError> {
    append_event(
        snapshot,
        delegation.parent_namespace().to_owned(),
        issued_at_ms,
        RegistryEventBodyV1::NamespaceDelegation(delegation),
        signer,
    )
}

pub fn append_profile_publication(
    snapshot: &mut RegistrySnapshotV1,
    publication: ProfilePublicationV1,
    issued_at_ms: u64,
    signer: &RegistrySignerV1,
) -> Result<(), RegistryError> {
    append_event(
        snapshot,
        publication.namespace().to_owned(),
        issued_at_ms,
        RegistryEventBodyV1::PublishProfile(publication),
        signer,
    )
}

/// Append a profile to the most specific snapshot in which this key currently
/// has authority. The complete candidate store is verified before mutation.
pub fn append_profile_to_store(
    store: &mut RegistryStoreV1,
    publication: ProfilePublicationV1,
    issued_at_ms: u64,
    signer: &RegistrySignerV1,
    trust: &RegistryTrustV1,
    staleness: ProfileStalenessPolicyV1,
) -> Result<(), RegistryError> {
    let expected_issuer = hex::encode(registry_public_key_id(&signer.public_key()));
    if publication.profile().issuer_key().as_sha256() != expected_issuer {
        return Err(RegistryError::ProfileIssuerMismatch);
    }
    let mut order: Vec<_> = (0..store.snapshots().len()).collect();
    order.sort_by_key(|index| {
        std::cmp::Reverse(
            super::verify::snapshot_root_identity(&store.snapshots()[*index])
                .map(|identity| identity.0.len())
                .unwrap_or(0),
        )
    });
    for index in order {
        let mut candidate = store.clone();
        append_profile_publication(
            &mut candidate.snapshots_mut()[index],
            publication.clone(),
            issued_at_ms,
            signer,
        )?;
        if super::verify::verify_registry_store(&candidate, trust, issued_at_ms, staleness).is_ok()
        {
            *store = candidate;
            return Ok(());
        }
    }
    Err(RegistryError::NoWritableSnapshot(
        publication.namespace().to_owned(),
    ))
}

fn append_event(
    snapshot: &mut RegistrySnapshotV1,
    namespace: String,
    issued_at_ms: u64,
    body: RegistryEventBodyV1,
    signer: &RegistrySignerV1,
) -> Result<(), RegistryError> {
    snapshot.validate_shape()?;
    if snapshot.events().len() >= super::MAX_REGISTRY_EVENTS {
        return Err(RegistryError::TooManyEvents {
            maximum: super::MAX_REGISTRY_EVENTS,
        });
    }
    let previous = snapshot
        .events()
        .last()
        .ok_or(RegistryError::EmptySnapshot)?;
    let sequence =
        previous
            .event()
            .sequence()
            .checked_add(1)
            .ok_or(RegistryError::SequenceMismatch {
                expected: u64::MAX,
                found: u64::MAX,
            })?;
    let event = RegistryEventV1::new(
        sequence,
        Some(registry_event_sha256(previous)?),
        issued_at_ms,
        namespace,
        signer.public_key(),
        body,
    )?;
    snapshot.events_mut().push(signer.sign_event(event)?);
    Ok(())
}

pub(crate) fn verify_event_signature(event: &SignedRegistryEventV1) -> Result<(), RegistryError> {
    let signature: [u8; 64] =
        event
            .signature()
            .try_into()
            .map_err(|_| RegistryError::InvalidSignature {
                sequence: event.event().sequence(),
            })?;
    let verifying_key =
        VerifyingKey::from_bytes(event.event().signer_public_key()).map_err(|_| {
            RegistryError::InvalidSignature {
                sequence: event.event().sequence(),
            }
        })?;
    verifying_key
        .verify_strict(
            &registry_signing_preimage(event.event())?,
            &Signature::from_bytes(&signature),
        )
        .map_err(|_| RegistryError::InvalidSignature {
            sequence: event.event().sequence(),
        })
}

fn registry_signing_preimage(event: &RegistryEventV1) -> Result<Vec<u8>, RegistryError> {
    let body = canonical_registry_bytes(event)?;
    let mut preimage = Vec::with_capacity(REGISTRY_SIGNING_DOMAIN_V1.len() + 8 + body.len());
    preimage.extend_from_slice(REGISTRY_SIGNING_DOMAIN_V1);
    preimage.extend_from_slice(&(body.len() as u64).to_be_bytes());
    preimage.extend_from_slice(&body);
    Ok(preimage)
}
