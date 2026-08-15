use std::collections::{BTreeMap, BTreeSet};

use super::crypto::{registry_public_key_id, verify_event_signature};
use super::model::{
    is_within_scope, ProfileStalenessPolicyV1, RegistryEventBodyV1, RegistryProfileKeyV1,
    RegistryPublicKeyV1, RegistrySnapshotV1, RegistryStoreV1, RegistryTrustV1,
    VerifiedRegistryProfileV1, VerifiedRegistryV1,
};
use super::{canonical_registry_bytes, registry_event_sha256, RegistryError};

#[derive(Clone, Debug)]
struct Authority {
    scope: String,
    public_key: RegistryPublicKeyV1,
    valid_from_ms: u64,
    expires_at_ms: u64,
}

impl Authority {
    fn authorizes(&self, namespace: &str, public_key: &RegistryPublicKeyV1, at_ms: u64) -> bool {
        &self.public_key == public_key
            && is_within_scope(namespace, &self.scope)
            && self.valid_from_ms <= at_ms
            && at_ms < self.expires_at_ms
    }

    fn authorizes_root(
        &self,
        namespace: &str,
        public_key: &RegistryPublicKeyV1,
        valid_from_ms: u64,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> bool {
        &self.public_key == public_key
            && namespace == self.scope
            && self.valid_from_ms <= valid_from_ms
            && expires_at_ms <= self.expires_at_ms
            && self.valid_from_ms <= now_ms
            && now_ms < self.expires_at_ms
    }

    fn contains_interval(&self, valid_from_ms: u64, expires_at_ms: u64) -> bool {
        self.valid_from_ms <= valid_from_ms && expires_at_ms <= self.expires_at_ms
    }
}

struct SnapshotVerification {
    root_namespace: String,
    profiles: BTreeMap<RegistryProfileKeyV1, VerifiedRegistryProfileV1>,
    active_delegations: Vec<Authority>,
    last_sequence: u64,
}

pub fn verify_registry_store(
    store: &RegistryStoreV1,
    trust: &RegistryTrustV1,
    now_ms: u64,
    staleness: ProfileStalenessPolicyV1,
) -> Result<VerifiedRegistryV1, RegistryError> {
    store.validate_shape()?;
    trust.validate()?;

    let mut root_names = BTreeSet::new();
    let mut root_identities = Vec::with_capacity(store.snapshots().len());
    for snapshot in store.snapshots() {
        let (namespace, public_key) = snapshot_root_identity(snapshot)?;
        if !root_names.insert(namespace.clone()) {
            return Err(RegistryError::Equivocation {
                namespace,
                sequence: 1,
            });
        }
        root_identities.push((namespace, public_key));
    }
    let sorted = root_identities.windows(2).all(|pair| pair[0] < pair[1]);
    if !sorted && root_identities.len() > 1 {
        return Err(RegistryError::NonCanonicalEncoding);
    }

    let mut authorities: Vec<Authority> = trust
        .roots()
        .iter()
        .map(|root| Authority {
            scope: root.namespace().to_owned(),
            public_key: *root.public_key(),
            valid_from_ms: 0,
            expires_at_ms: u64::MAX,
        })
        .collect();
    let mut unresolved: BTreeSet<usize> = (0..store.snapshots().len()).collect();
    let mut verified = Vec::with_capacity(store.snapshots().len());

    while !unresolved.is_empty() {
        let mut progress = false;
        let indices: Vec<_> = unresolved.iter().copied().collect();
        for index in indices {
            let snapshot = &store.snapshots()[index];
            let (namespace, public_key) = &root_identities[index];
            let matching = authorities.iter().find(|authority| {
                authority.public_key == *public_key && namespace == &authority.scope
            });
            let Some(anchor) = matching else {
                continue;
            };
            let checked = verify_snapshot(snapshot, anchor, now_ms, staleness)?;
            authorities.extend(checked.active_delegations.iter().cloned());
            verified.push(checked);
            unresolved.remove(&index);
            progress = true;
        }
        if !progress {
            let index = *unresolved.iter().next().expect("unresolved is nonempty");
            return Err(RegistryError::MissingDelegation {
                namespace: root_identities[index].0.clone(),
            });
        }
    }

    let mut profiles: BTreeMap<RegistryProfileKeyV1, VerifiedRegistryProfileV1> = BTreeMap::new();
    let mut last_sequences = BTreeMap::new();
    for snapshot in verified {
        last_sequences.insert(snapshot.root_namespace, snapshot.last_sequence);
        for (key, profile) in snapshot.profiles {
            if let Some(existing) = profiles.get(&key) {
                let generation = profile.publication().profile().profile_generation().get();
                let existing_generation =
                    existing.publication().profile().profile_generation().get();
                return Err(RegistryError::ProfileEquivocation {
                    namespace: key.namespace().to_owned(),
                    node_id: key.node_id().to_owned(),
                    generation: generation.max(existing_generation),
                });
            }
            profiles.insert(key, profile);
        }
    }

    Ok(VerifiedRegistryV1::new(
        profiles,
        store.snapshots().len(),
        last_sequences,
    ))
}

fn verify_snapshot(
    snapshot: &RegistrySnapshotV1,
    anchor: &Authority,
    now_ms: u64,
    staleness: ProfileStalenessPolicyV1,
) -> Result<SnapshotVerification, RegistryError> {
    snapshot.validate_shape()?;
    let first = snapshot
        .events()
        .first()
        .ok_or(RegistryError::EmptySnapshot)?;
    first.event().validate_shape()?;
    if first.event().sequence() != 1 || first.event().previous_event_sha256().is_some() {
        return Err(RegistryError::InvalidRootEvent);
    }
    verify_event_signature(first)?;
    let RegistryEventBodyV1::NamespaceRoot(root) = first.event().body() else {
        return Err(RegistryError::InvalidRootEvent);
    };
    if first.event().namespace() != root.namespace()
        || first.event().signer_public_key() != root.public_key()
        || first.event().issued_at_ms() != root.valid_from_ms()
    {
        return Err(RegistryError::InvalidRootEvent);
    }
    if !anchor.authorizes_root(
        root.namespace(),
        root.public_key(),
        root.valid_from_ms(),
        root.expires_at_ms(),
        now_ms,
    ) {
        return Err(RegistryError::MissingDelegation {
            namespace: root.namespace().to_owned(),
        });
    }
    if now_ms < root.valid_from_ms() || now_ms >= root.expires_at_ms() {
        return Err(RegistryError::InvalidValidity {
            record: "current namespace root",
        });
    }

    let mut authorities = vec![Authority {
        scope: root.namespace().to_owned(),
        public_key: *root.public_key(),
        valid_from_ms: root.valid_from_ms().max(anchor.valid_from_ms),
        expires_at_ms: root.expires_at_ms().min(anchor.expires_at_ms),
    }];
    let mut active_delegations = Vec::new();
    let mut profiles: BTreeMap<RegistryProfileKeyV1, VerifiedRegistryProfileV1> = BTreeMap::new();
    let mut previous_digest = registry_event_sha256(first)?;
    let mut previous_issued_at = first.event().issued_at_ms();

    for (index, signed) in snapshot.events().iter().enumerate().skip(1) {
        let event = signed.event();
        event.validate_shape()?;
        let expected_sequence = (index as u64) + 1;
        if event.sequence() != expected_sequence {
            return Err(RegistryError::SequenceMismatch {
                expected: expected_sequence,
                found: event.sequence(),
            });
        }
        if event.previous_event_sha256() != Some(&previous_digest) {
            return Err(RegistryError::PreviousEventMismatch {
                sequence: event.sequence(),
            });
        }
        if event.issued_at_ms() < previous_issued_at {
            return Err(RegistryError::TimestampRollback {
                sequence: event.sequence(),
            });
        }
        if event.issued_at_ms() > now_ms {
            return Err(RegistryError::FutureEvent {
                sequence: event.sequence(),
                issued_at_ms: event.issued_at_ms(),
                now_ms,
            });
        }
        verify_event_signature(signed)?;
        let authorizing: Vec<_> = authorities
            .iter()
            .filter(|authority| {
                authority.authorizes(
                    event.namespace(),
                    event.signer_public_key(),
                    event.issued_at_ms(),
                )
            })
            .collect();
        if authorizing.is_empty() {
            return Err(RegistryError::UnauthorizedSigner {
                sequence: event.sequence(),
                namespace: event.namespace().to_owned(),
            });
        }

        match event.body() {
            RegistryEventBodyV1::NamespaceRoot(_) => return Err(RegistryError::InvalidRootEvent),
            RegistryEventBodyV1::NamespaceDelegation(delegation) => {
                if event.namespace() != delegation.parent_namespace() {
                    return Err(RegistryError::NamespaceMismatch {
                        event: event.namespace().to_owned(),
                        body: delegation.parent_namespace().to_owned(),
                    });
                }
                if delegation.valid_from_ms() < event.issued_at_ms()
                    || !authorizing.iter().any(|authority| {
                        authority.contains_interval(
                            delegation.valid_from_ms(),
                            delegation.expires_at_ms(),
                        )
                    })
                {
                    return Err(RegistryError::InvalidValidity {
                        record: "bounded namespace delegation",
                    });
                }
                let delegated = Authority {
                    scope: delegation.child_namespace().to_owned(),
                    public_key: *delegation.delegate_public_key(),
                    valid_from_ms: delegation.valid_from_ms(),
                    expires_at_ms: delegation.expires_at_ms(),
                };
                if delegated.valid_from_ms <= now_ms && now_ms < delegated.expires_at_ms {
                    active_delegations.push(delegated.clone());
                }
                authorities.push(delegated);
            }
            RegistryEventBodyV1::PublishProfile(publication) => {
                if event.namespace() != publication.namespace() {
                    return Err(RegistryError::NamespaceMismatch {
                        event: event.namespace().to_owned(),
                        body: publication.namespace().to_owned(),
                    });
                }
                validate_profile_issuer(publication, event.signer_public_key())?;
                let (issued_at_ms, expires_at_ms) = profile_validity(publication)?;
                if event.issued_at_ms() < issued_at_ms
                    || event.issued_at_ms() >= expires_at_ms
                    || !authorizing
                        .iter()
                        .any(|authority| authority.contains_interval(issued_at_ms, expires_at_ms))
                {
                    return Err(RegistryError::InvalidValidity {
                        record: "authority-bounded profile publication",
                    });
                }
                let key = RegistryProfileKeyV1::new(
                    publication.namespace().to_owned(),
                    publication.node_id().to_owned(),
                );
                let generation = publication.profile().profile_generation().get();
                if let Some(existing) = profiles.get(&key) {
                    let current = existing.publication().profile().profile_generation().get();
                    if generation < current {
                        return Err(RegistryError::ProfileRollback {
                            namespace: key.namespace().to_owned(),
                            node_id: key.node_id().to_owned(),
                            current,
                            incoming: generation,
                        });
                    }
                    if generation == current {
                        return Err(RegistryError::ProfileEquivocation {
                            namespace: key.namespace().to_owned(),
                            node_id: key.node_id().to_owned(),
                            generation,
                        });
                    }
                }
                profiles.insert(
                    key,
                    VerifiedRegistryProfileV1::new(
                        publication.clone(),
                        registry_event_sha256(signed)?,
                        issued_at_ms,
                        expires_at_ms,
                        now_ms >= expires_at_ms,
                    ),
                );
            }
        }
        previous_digest = registry_event_sha256(signed)?;
        previous_issued_at = event.issued_at_ms();
    }

    for (key, profile) in &profiles {
        if now_ms < profile.issued_at_ms() {
            return Err(RegistryError::ProfileNotYetValid {
                namespace: key.namespace().to_owned(),
                node_id: key.node_id().to_owned(),
            });
        }
        if profile.is_stale() && staleness == ProfileStalenessPolicyV1::Reject {
            return Err(RegistryError::StaleProfile {
                namespace: key.namespace().to_owned(),
                node_id: key.node_id().to_owned(),
                expires_at_ms: profile.expires_at_ms(),
            });
        }
    }

    Ok(SnapshotVerification {
        root_namespace: root.namespace().to_owned(),
        profiles,
        active_delegations,
        last_sequence: snapshot.last_sequence(),
    })
}

fn validate_profile_issuer(
    publication: &super::ProfilePublicationV1,
    signer_public_key: &RegistryPublicKeyV1,
) -> Result<(), RegistryError> {
    let expected = hex::encode(registry_public_key_id(signer_public_key));
    if publication.profile().issuer_key().as_sha256() == expected {
        Ok(())
    } else {
        Err(RegistryError::ProfileIssuerMismatch)
    }
}

fn profile_validity(
    publication: &super::ProfilePublicationV1,
) -> Result<(u64, u64), RegistryError> {
    Ok((
        publication.profile().issued_at().get(),
        publication.profile().expires_at().get(),
    ))
}

pub(crate) fn snapshot_root_identity(
    snapshot: &RegistrySnapshotV1,
) -> Result<(String, RegistryPublicKeyV1), RegistryError> {
    snapshot.validate_shape()?;
    let first = snapshot
        .events()
        .first()
        .ok_or(RegistryError::EmptySnapshot)?;
    let RegistryEventBodyV1::NamespaceRoot(root) = first.event().body() else {
        return Err(RegistryError::InvalidRootEvent);
    };
    Ok((root.namespace().to_owned(), *root.public_key()))
}

pub fn merge_registry_store(
    current: &RegistryStoreV1,
    incoming: &RegistryStoreV1,
) -> Result<RegistryStoreV1, RegistryError> {
    current.validate_shape()?;
    incoming.validate_shape()?;
    let mut merged = current.clone();
    for candidate in incoming.snapshots() {
        let candidate_identity = snapshot_root_identity(candidate)?;
        let existing = merged.snapshots_mut().iter_mut().find(|snapshot| {
            snapshot_root_identity(snapshot).ok() == Some(candidate_identity.clone())
        });
        if let Some(existing) = existing {
            merge_snapshot(existing, candidate, &candidate_identity.0)?;
        } else {
            merged.snapshots_mut().push(candidate.clone());
        }
    }
    merged.snapshots_mut().sort_by(|left, right| {
        snapshot_root_identity(left)
            .expect("snapshot identity was validated")
            .cmp(&snapshot_root_identity(right).expect("snapshot identity was validated"))
    });
    Ok(merged)
}

fn merge_snapshot(
    current: &mut RegistrySnapshotV1,
    incoming: &RegistrySnapshotV1,
    namespace: &str,
) -> Result<(), RegistryError> {
    if incoming.events().len() < current.events().len() {
        return Err(RegistryError::SnapshotRollback {
            namespace: namespace.to_owned(),
            current: current.last_sequence(),
            incoming: incoming.last_sequence(),
        });
    }
    for (index, (left, right)) in current.events().iter().zip(incoming.events()).enumerate() {
        if canonical_registry_bytes(left)? != canonical_registry_bytes(right)? {
            return Err(RegistryError::Equivocation {
                namespace: namespace.to_owned(),
                sequence: (index as u64) + 1,
            });
        }
    }
    if incoming.events().len() > current.events().len() {
        *current = incoming.clone();
    }
    Ok(())
}
