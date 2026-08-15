use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use super::PlacementValidationError;

const SHA256_HEX_BYTES: usize = 64;
pub(crate) const MAX_TOKEN_BYTES: usize = 128;
pub(crate) const MAX_LABEL_BYTES: usize = 256;

/// Domain-separated SHA-256 identity for an immutable placement-protocol record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SemanticDigestV1(String);

impl SemanticDigestV1 {
    pub fn from_sha256(value: impl Into<String>) -> Result<Self, PlacementValidationError> {
        let value = value.into();
        if value.len() != SHA256_HEX_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(PlacementValidationError::InvalidDigest {
                field: "semantic digest",
            });
        }
        Ok(Self(value))
    }

    pub fn hash_bytes(domain: &'static str, bytes: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update((domain.len() as u64).to_be_bytes());
        hash.update(domain.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
        Self(hex::encode(hash.finalize()))
    }

    pub fn as_sha256(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SemanticDigestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SemanticDigestV1 {
    type Err = PlacementValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_sha256(value)
    }
}

impl<'de> Deserialize<'de> for SemanticDigestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_sha256(value).map_err(serde::de::Error::custom)
    }
}

/// Nonzero monotonically increasing generation within one named scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct GenerationV1(NonZeroU64);

impl GenerationV1 {
    pub fn new(value: u64) -> Result<Self, PlacementValidationError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(PlacementValidationError::Zero {
                field: "generation",
            })
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl<'de> Deserialize<'de> for GenerationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Unix timestamp in milliseconds.  Callers supply time explicitly to keep
/// validation deterministic and testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillisV1(u64);

impl UnixMillisV1 {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, duration_ms: u64) -> Option<Self> {
        self.0.checked_add(duration_ms).map(Self)
    }
}

/// Deterministic digest projection for placement records.
///
/// Implementations in this module contain only structs, enums, integers,
/// strings, and ordered `BTree*` collections.  Their serde JSON form is
/// therefore deterministic and is domain-separated before hashing.  Network
/// transports may wrap the resulting digest in any authenticated envelope;
/// that envelope is deliberately outside this trait.
pub trait CanonicalPlacementRecordV1: Serialize {
    const DIGEST_DOMAIN: &'static str;

    fn canonical_bytes(&self) -> Result<Vec<u8>, PlacementValidationError> {
        serde_json::to_vec(self)
            .map_err(|error| PlacementValidationError::CanonicalSerialization(error.to_string()))
    }

    fn semantic_digest(&self) -> Result<SemanticDigestV1, PlacementValidationError> {
        Ok(SemanticDigestV1::hash_bytes(
            Self::DIGEST_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }
}

pub(crate) fn validate_token(
    field: &'static str,
    value: &str,
) -> Result<(), PlacementValidationError> {
    if value.is_empty() {
        return Err(PlacementValidationError::Empty { field });
    }
    if value.len() > MAX_TOKEN_BYTES {
        return Err(PlacementValidationError::TooLong {
            field,
            limit: MAX_TOKEN_BYTES,
        });
    }
    if matches!(value, "." | "..")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b':' | b'/')
        })
    {
        return Err(PlacementValidationError::InvalidToken {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_label(
    field: &'static str,
    value: &str,
) -> Result<(), PlacementValidationError> {
    if value.is_empty() {
        return Err(PlacementValidationError::Empty { field });
    }
    if value.len() > MAX_LABEL_BYTES {
        return Err(PlacementValidationError::TooLong {
            field,
            limit: MAX_LABEL_BYTES,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(PlacementValidationError::InvalidToken {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_window(
    record: &'static str,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
    maximum_ms: u64,
) -> Result<(), PlacementValidationError> {
    if expires_at <= issued_at {
        return Err(PlacementValidationError::InvalidValidity { record });
    }
    if expires_at.get() - issued_at.get() > maximum_ms {
        return Err(PlacementValidationError::LifetimeExceeded { record, maximum_ms });
    }
    Ok(())
}

pub(crate) fn validate_fresh(
    record: &'static str,
    issued_at: UnixMillisV1,
    expires_at: UnixMillisV1,
    now: UnixMillisV1,
) -> Result<(), PlacementValidationError> {
    if now < issued_at {
        return Err(PlacementValidationError::NotYetValid { record });
    }
    if now >= expires_at {
        return Err(PlacementValidationError::Expired { record });
    }
    Ok(())
}
