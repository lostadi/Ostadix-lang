use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use super::InformationErrorV1;

fn validate_sha256(kind: &'static str, value: &str) -> Result<(), InformationErrorV1> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(InformationErrorV1::InvalidDigest {
            kind,
            value: value.to_string(),
        })
    }
}

pub(crate) fn domain_digest(domain: &'static [u8], bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ostadix.information-domain/v1\0");
    hash.update((domain.len() as u64).to_be_bytes());
    hash.update(domain);
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    hex::encode(hash.finalize())
}

macro_rules! digest_id {
    ($name:ident, $kind:literal, $domain:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::from_sha256(value).map_err(de::Error::custom)
            }
        }

        impl $name {
            pub fn from_sha256(value: impl Into<String>) -> Result<Self, InformationErrorV1> {
                let value = value.into();
                validate_sha256($kind, &value)?;
                Ok(Self(value))
            }

            pub(in crate::information) fn digest(bytes: &[u8]) -> Self {
                Self(domain_digest($domain, bytes))
            }

            pub fn as_sha256(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

digest_id!(BlobIdV1, "blob", b"ostadix.info-blob/v1");

impl BlobIdV1 {
    /// Derive the managed-blob identity without storing or ingesting the bytes.
    pub fn from_content_bytes(bytes: &[u8]) -> Self {
        Self::digest(bytes)
    }
}

digest_id!(EntityIdV1, "entity", b"ostadix.info-entity/v1");
digest_id!(AtomIdV1, "atom", b"ostadix.info-atom/v1");
digest_id!(
    SnapshotRootIdV1,
    "snapshot root",
    b"ostadix.info-snapshot/v1"
);
digest_id!(RevisionIdV1, "revision", b"ostadix.info-revision/v1");
digest_id!(
    ProjectionReceiptIdV1,
    "projection receipt",
    b"ostadix.info-projection/v1"
);
digest_id!(DeltaIdV1, "delta", b"ostadix.info-delta/v1");
digest_id!(DecisionIdV1, "decision", b"ostadix.info-decision/v1");
digest_id!(
    ObservationIdV1,
    "observation",
    b"ostadix.info-observation/v1"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_separate_identical_bytes() {
        assert_ne!(
            BlobIdV1::digest(b"same").as_sha256(),
            AtomIdV1::digest(b"same").as_sha256()
        );
    }

    #[test]
    fn external_digests_are_strict_lowercase_sha256() {
        assert!(BlobIdV1::from_sha256("00".repeat(32)).is_ok());
        assert!(BlobIdV1::from_sha256("AA".repeat(32)).is_err());
        assert!(BlobIdV1::from_sha256("0").is_err());
        assert!(serde_json::from_str::<BlobIdV1>(&format!("\"{}\"", "AA".repeat(32))).is_err());
        assert!(serde_json::from_str::<BlobIdV1>("\"00\"").is_err());
    }
}
