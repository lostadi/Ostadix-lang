use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    canonical_bytes, AtomIdV1, BlobIdV1, EntityIdV1, InformationErrorV1, MAX_T0_CANONICAL_BYTES,
    MAX_T1_BYTES,
};

pub const INFORMATION_ATOM_SCHEMA_V1: &str = "ostadix.info-atom/v1";
pub const ENTITY_DESCRIPTOR_SCHEMA_V1: &str = "ostadix.info-entity/v1";

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/' | b':' | b'+')
        })
}

fn reject_forbidden_payload_schema(schema: &str) -> Result<(), InformationErrorV1> {
    let normalized = schema.to_ascii_lowercase();
    let forbidden = [
        "bearer",
        "capability",
        "credential",
        "environment-value",
        "executable-handle",
        "private-key",
        "session-secret",
        "signing-key",
        "tls-private",
    ];
    if forbidden.iter().any(|needle| normalized.contains(needle)) {
        Err(InformationErrorV1::ForbiddenPayload(schema.to_string()))
    } else {
        Ok(())
    }
}

fn valid_payload_schema(value: &str) -> bool {
    let Some((namespace, version)) = value.split_once('/') else {
        return false;
    };
    !namespace.is_empty()
        && !namespace.contains('/')
        && namespace.split('.').all(|component| {
            !component.is_empty()
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && component
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_lowercase)
                && component
                    .as_bytes()
                    .last()
                    .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
        && version.strip_prefix('v').is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn valid_media_type(value: &str) -> bool {
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && kind
            .bytes()
            .chain(subtype.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EntityDescriptorV1 {
    schema: String,
    namespace: String,
    kind: String,
    coordinates: BTreeMap<String, String>,
}

impl EntityDescriptorV1 {
    pub fn new(
        namespace: impl Into<String>,
        kind: impl Into<String>,
        coordinates: BTreeMap<String, String>,
    ) -> Result<Self, InformationErrorV1> {
        let namespace = namespace.into();
        let kind = kind.into();
        if !valid_token(&namespace) || !valid_token(&kind) {
            return Err(InformationErrorV1::InvalidRecord(
                "entity namespace and kind must be non-empty canonical tokens".to_string(),
            ));
        }
        if coordinates.is_empty()
            || coordinates
                .iter()
                .any(|(key, value)| !valid_token(key) || value.is_empty())
        {
            return Err(InformationErrorV1::InvalidRecord(
                "entity coordinates must contain non-empty canonical keys and values".to_string(),
            ));
        }
        Ok(Self {
            schema: ENTITY_DESCRIPTOR_SCHEMA_V1.to_string(),
            namespace,
            kind,
            coordinates,
        })
    }

    pub fn id(&self) -> Result<EntityIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(EntityIdV1::digest(&canonical_bytes(self)?))
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != ENTITY_DESCRIPTOR_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported entity descriptor schema `{}`",
                self.schema
            )));
        }
        let normalized = Self::new(
            self.namespace.clone(),
            self.kind.clone(),
            self.coordinates.clone(),
        )?;
        if normalized == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "entity descriptor is not in normalized canonical form".to_string(),
            ))
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum PublicScalarV1 {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64Bits(u64),
    Text(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManagedPayloadRefV1 {
    pub schema: String,
    pub media_type: String,
    pub blob_id: BlobIdV1,
    pub logical_len: u64,
}

impl ManagedPayloadRefV1 {
    pub fn new(
        schema: impl Into<String>,
        media_type: impl Into<String>,
        blob_id: BlobIdV1,
        logical_len: u64,
    ) -> Result<Self, InformationErrorV1> {
        let schema = schema.into();
        let media_type = media_type.into();
        reject_forbidden_payload_schema(&schema)?;
        if !valid_payload_schema(&schema) || !valid_media_type(&media_type) {
            return Err(InformationErrorV1::InvalidRecord(
                "T1 schema and media type must be non-empty".to_string(),
            ));
        }
        if logical_len > MAX_T1_BYTES {
            return Err(InformationErrorV1::T1TooLarge {
                actual: logical_len,
                maximum: MAX_T1_BYTES,
            });
        }
        Ok(Self {
            schema,
            media_type,
            blob_id,
            logical_len,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalPayloadRefV1 {
    pub schema: String,
    pub media_type: String,
    pub sha256: String,
    pub logical_len: u64,
}

impl ExternalPayloadRefV1 {
    pub fn new(
        schema: impl Into<String>,
        media_type: impl Into<String>,
        sha256: impl Into<String>,
        logical_len: u64,
    ) -> Result<Self, InformationErrorV1> {
        let schema = schema.into();
        let media_type = media_type.into();
        let sha256 = sha256.into();
        reject_forbidden_payload_schema(&schema)?;
        BlobIdV1::from_sha256(sha256.clone())?;
        if !valid_payload_schema(&schema) || !valid_media_type(&media_type) {
            return Err(InformationErrorV1::InvalidRecord(
                "T2 schema and media type must be canonical non-locator tokens".to_string(),
            ));
        }
        Ok(Self {
            schema,
            media_type,
            sha256,
            logical_len,
        })
    }
}

pub type NativeRecordRefV1 = ExternalPayloadRefV1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "tier", content = "payload", rename_all = "kebab-case")]
pub enum PayloadRefV1 {
    T0(PublicScalarV1),
    T1(ManagedPayloadRefV1),
    T2(ExternalPayloadRefV1),
}

impl PayloadRefV1 {
    pub fn public(value: PublicScalarV1) -> Result<Self, InformationErrorV1> {
        let payload = Self::T0(value);
        let actual = canonical_bytes(&payload)?.len();
        if actual > MAX_T0_CANONICAL_BYTES {
            return Err(InformationErrorV1::T0TooLarge {
                actual,
                maximum: MAX_T0_CANONICAL_BYTES,
            });
        }
        Ok(payload)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        let normalized = match self {
            Self::T0(value) => Self::public(value.clone())?,
            Self::T1(value) => Self::T1(ManagedPayloadRefV1::new(
                value.schema.clone(),
                value.media_type.clone(),
                value.blob_id.clone(),
                value.logical_len,
            )?),
            Self::T2(value) => Self::T2(ExternalPayloadRefV1::new(
                value.schema.clone(),
                value.media_type.clone(),
                value.sha256.clone(),
                value.logical_len,
            )?),
        };
        if normalized == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "payload reference is not in normalized canonical form".to_string(),
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcquisitionModalityV1 {
    Declared,
    Derived,
    Enforced,
    Observed,
    Predicted,
    Counterfactual,
    Contradicted,
    Invalidated,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeV1 {
    pub artifact_sha256: Option<String>,
    pub execution_id: Option<String>,
    pub attempt_id: Option<String>,
    pub environment_sha256: Option<String>,
    pub node_id: Option<String>,
    pub generation: Option<u64>,
    pub valid_from_unix_ms: Option<u64>,
    pub valid_until_unix_ms: Option<u64>,
}

impl ScopeV1 {
    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        for (kind, digest) in [
            ("artifact", self.artifact_sha256.as_deref()),
            ("environment", self.environment_sha256.as_deref()),
        ] {
            if let Some(digest) = digest {
                BlobIdV1::from_sha256(digest.to_string()).map_err(|_| {
                    InformationErrorV1::InvalidRecord(format!(
                        "scope {kind} digest must be lowercase sha256"
                    ))
                })?;
            }
        }
        if let (Some(from), Some(until)) = (self.valid_from_unix_ms, self.valid_until_unix_ms) {
            if from > until {
                return Err(InformationErrorV1::InvalidRecord(
                    "scope validity interval is inverted".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ParticipantV1 {
    pub role: String,
    pub entity: EntityIdV1,
}

impl ParticipantV1 {
    pub fn new(role: impl Into<String>, entity: EntityIdV1) -> Result<Self, InformationErrorV1> {
        let role = role.into();
        if !valid_token(&role) {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "invalid participant role `{role}`"
            )));
        }
        Ok(Self { role, entity })
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if valid_token(&self.role) {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(format!(
                "invalid participant role `{}`",
                self.role
            )))
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InformationAtomV1 {
    schema: String,
    participants: Vec<ParticipantV1>,
    predicate_schema: String,
    payload: PayloadRefV1,
    modality: AcquisitionModalityV1,
    scope: ScopeV1,
    producer: EntityIdV1,
    derivation_identity: Option<String>,
    support: Vec<AtomIdV1>,
    confidence_ppm: Option<u32>,
    affordances: Vec<String>,
    transparency_consequences: Vec<String>,
}

impl InformationAtomV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut participants: Vec<ParticipantV1>,
        predicate_schema: impl Into<String>,
        payload: PayloadRefV1,
        modality: AcquisitionModalityV1,
        scope: ScopeV1,
        producer: EntityIdV1,
        mut support: Vec<AtomIdV1>,
    ) -> Result<Self, InformationErrorV1> {
        let predicate_schema = predicate_schema.into();
        if participants.is_empty() || !valid_token(&predicate_schema) {
            return Err(InformationErrorV1::InvalidRecord(
                "atom requires participants and a canonical predicate schema".to_string(),
            ));
        }
        for participant in &participants {
            participant.validate()?;
        }
        payload.validate()?;
        scope.validate()?;
        participants.sort();
        participants.dedup();
        support.sort();
        support.dedup();
        Ok(Self {
            schema: INFORMATION_ATOM_SCHEMA_V1.to_string(),
            participants,
            predicate_schema,
            payload,
            modality,
            scope,
            producer,
            derivation_identity: None,
            support,
            confidence_ppm: None,
            affordances: Vec::new(),
            transparency_consequences: Vec::new(),
        })
    }

    pub fn with_confidence_ppm(mut self, confidence: u32) -> Result<Self, InformationErrorV1> {
        if confidence > 1_000_000 {
            return Err(InformationErrorV1::InvalidRecord(
                "confidence_ppm exceeds 1,000,000".to_string(),
            ));
        }
        self.confidence_ppm = Some(confidence);
        Ok(self)
    }

    pub fn with_derivation_identity(mut self, identity: impl Into<String>) -> Self {
        self.derivation_identity = Some(identity.into());
        self
    }

    pub fn with_affordances(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.affordances = values
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self
    }

    pub fn with_transparency_consequences(
        mut self,
        values: impl IntoIterator<Item = String>,
    ) -> Self {
        self.transparency_consequences = values
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self
    }

    pub fn id(&self) -> Result<AtomIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(AtomIdV1::digest(&canonical_bytes(self)?))
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != INFORMATION_ATOM_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported information atom schema `{}`",
                self.schema
            )));
        }
        self.payload.validate()?;
        for participant in &self.participants {
            participant.validate()?;
        }
        if self
            .derivation_identity
            .as_ref()
            .is_some_and(|identity| identity.is_empty())
            || self.affordances.iter().any(String::is_empty)
            || self.transparency_consequences.iter().any(String::is_empty)
        {
            return Err(InformationErrorV1::InvalidRecord(
                "atom derivation, affordance, and transparency values must be non-empty"
                    .to_string(),
            ));
        }
        let mut normalized = Self::new(
            self.participants.clone(),
            self.predicate_schema.clone(),
            self.payload.clone(),
            self.modality,
            self.scope.clone(),
            self.producer.clone(),
            self.support.clone(),
        )?;
        if let Some(identity) = &self.derivation_identity {
            normalized = normalized.with_derivation_identity(identity.clone());
        }
        if let Some(confidence) = self.confidence_ppm {
            normalized = normalized.with_confidence_ppm(confidence)?;
        }
        normalized = normalized
            .with_affordances(self.affordances.clone())
            .with_transparency_consequences(self.transparency_consequences.clone());
        if normalized == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "information atom is not in normalized canonical form".to_string(),
            ))
        }
    }

    pub fn modality(&self) -> AcquisitionModalityV1 {
        self.modality
    }

    pub fn participants(&self) -> &[ParticipantV1] {
        &self.participants
    }

    pub fn predicate_schema(&self) -> &str {
        &self.predicate_schema
    }

    pub fn payload(&self) -> &PayloadRefV1 {
        &self.payload
    }

    pub fn producer(&self) -> &EntityIdV1 {
        &self.producer
    }

    pub fn support(&self) -> &[AtomIdV1] {
        &self.support
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(label: &str) -> EntityIdV1 {
        EntityDescriptorV1::new(
            "oexec",
            "operation",
            BTreeMap::from([("id".to_string(), label.to_string())]),
        )
        .unwrap()
        .id()
        .unwrap()
    }

    #[test]
    fn atom_identity_is_independent_of_input_set_order() {
        let a = ParticipantV1::new("subject", entity("a")).unwrap();
        let b = ParticipantV1::new("input", entity("b")).unwrap();
        let payload = PayloadRefV1::public(PublicScalarV1::Bool(true)).unwrap();
        let left = InformationAtomV1::new(
            vec![a.clone(), b.clone()],
            "ostadix.test/fact-v1",
            payload.clone(),
            AcquisitionModalityV1::Derived,
            ScopeV1::default(),
            entity("producer"),
            vec![],
        )
        .unwrap();
        let right = InformationAtomV1::new(
            vec![b, a],
            "ostadix.test/fact-v1",
            payload,
            AcquisitionModalityV1::Derived,
            ScopeV1::default(),
            entity("producer"),
            vec![],
        )
        .unwrap();
        assert_eq!(left.id().unwrap(), right.id().unwrap());
    }

    #[test]
    fn payload_policy_rejects_bearers_and_oversized_inline_text() {
        assert!(ExternalPayloadRefV1::new(
            "ostadix.session-bearer/v1",
            "application/octet-stream",
            "00".repeat(32),
            1,
        )
        .is_err());
        assert!(PayloadRefV1::public(PublicScalarV1::Text("x".repeat(5000))).is_err());
    }

    #[test]
    fn external_payload_metadata_cannot_be_a_path_url_or_credential_locator() {
        for schema in [
            "/tmp/result",
            "tmp/result",
            "file:/tmp/result",
            "file:///tmp/result",
            "urn:ostadix:result",
            "C:/result",
            "C:\\result",
            "https://example.invalid/result",
            "ostadix.native/result/v1",
            "ostadix.credential-locator/v1",
        ] {
            assert!(ExternalPayloadRefV1::new(
                schema,
                "application/octet-stream",
                "00".repeat(32),
                1,
            )
            .is_err());
        }
        assert!(ExternalPayloadRefV1::new(
            "ostadix.native-record/v1",
            "https://example.invalid/result",
            "00".repeat(32),
            1,
        )
        .is_err());
        assert!(ExternalPayloadRefV1::new(
            "ostadix.native-record/v1",
            "application/octet-stream",
            "00".repeat(32),
            1,
        )
        .is_ok());
    }

    #[test]
    fn atom_construction_validates_direct_participant_and_payload_fields() {
        let invalid_participant = ParticipantV1 {
            role: String::new(),
            entity: entity("subject"),
        };
        let valid_payload = PayloadRefV1::public(PublicScalarV1::Bool(true)).unwrap();
        assert!(InformationAtomV1::new(
            vec![invalid_participant],
            "ostadix.test/fact-v1",
            valid_payload,
            AcquisitionModalityV1::Declared,
            ScopeV1::default(),
            entity("producer"),
            vec![],
        )
        .is_err());

        let invalid_payload = PayloadRefV1::T2(ExternalPayloadRefV1 {
            schema: "file:/tmp/result".to_string(),
            media_type: "application/octet-stream".to_string(),
            sha256: "00".repeat(32),
            logical_len: 1,
        });
        assert!(InformationAtomV1::new(
            vec![ParticipantV1::new("subject", entity("subject")).unwrap()],
            "ostadix.test/fact-v1",
            invalid_payload,
            AcquisitionModalityV1::Declared,
            ScopeV1::default(),
            entity("producer"),
            vec![],
        )
        .is_err());
    }

    #[test]
    fn contradiction_and_invalidation_remain_provenance_bearing_atoms() {
        for modality in [
            AcquisitionModalityV1::Contradicted,
            AcquisitionModalityV1::Invalidated,
        ] {
            let atom = InformationAtomV1::new(
                vec![ParticipantV1::new("subject", entity("claim")).unwrap()],
                "ostadix.claim-state/v1",
                PayloadRefV1::public(PublicScalarV1::Bool(true)).unwrap(),
                modality,
                ScopeV1::default(),
                entity("reviewer"),
                vec![],
            )
            .unwrap();
            atom.validate().unwrap();
            assert_eq!(atom.modality(), modality);
        }
    }
}
