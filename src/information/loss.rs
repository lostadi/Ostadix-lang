use std::collections::BTreeSet;

use serde::{de, Deserialize, Deserializer, Serialize};

use super::InformationErrorV1;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct LossKindV1(String);

impl<'de> Deserialize<'de> for LossKindV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl LossKindV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, InformationErrorV1> {
        let value = value.into();
        if value.is_empty()
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'/' | b':')
            })
        {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "invalid loss kind `{value}`"
            )));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionDispositionV1 {
    Exact,
    Lossy,
    Opaque,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LossContractV1 {
    definite: BTreeSet<LossKindV1>,
    possible: BTreeSet<LossKindV1>,
    opaque: bool,
}

impl LossContractV1 {
    pub fn exact() -> Self {
        Self {
            definite: BTreeSet::new(),
            possible: BTreeSet::new(),
            opaque: false,
        }
    }

    pub fn new(
        definite: impl IntoIterator<Item = LossKindV1>,
        possible: impl IntoIterator<Item = LossKindV1>,
        opaque: bool,
    ) -> Result<Self, InformationErrorV1> {
        let contract = Self {
            definite: definite.into_iter().collect(),
            possible: possible.into_iter().collect(),
            opaque,
        };
        contract.validate()?;
        Ok(contract)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.definite.is_subset(&self.possible) {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidLossContract)
        }
    }

    pub fn definite(&self) -> &BTreeSet<LossKindV1> {
        &self.definite
    }

    pub fn possible(&self) -> &BTreeSet<LossKindV1> {
        &self.possible
    }

    pub fn disposition(&self) -> ProjectionDispositionV1 {
        if self.opaque {
            ProjectionDispositionV1::Opaque
        } else if self.definite.is_empty() && self.possible.is_empty() {
            ProjectionDispositionV1::Exact
        } else {
            ProjectionDispositionV1::Lossy
        }
    }

    pub fn sequence(&self, next: &Self) -> Self {
        Self {
            definite: self.definite.union(&next.definite).cloned().collect(),
            possible: self.possible.union(&next.possible).cloned().collect(),
            opaque: self.opaque || next.opaque,
        }
    }

    pub fn alternative_join(&self, alternative: &Self) -> Self {
        Self {
            definite: self
                .definite
                .intersection(&alternative.definite)
                .cloned()
                .collect(),
            possible: self
                .possible
                .union(&alternative.possible)
                .cloned()
                .collect(),
            opaque: self.opaque || alternative.opaque,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loss(name: &str) -> LossKindV1 {
        LossKindV1::new(name).unwrap()
    }

    #[test]
    fn definite_loss_must_also_be_possible() {
        assert!(LossContractV1::new([loss("encoding")], [], false).is_err());
    }

    #[test]
    fn composition_and_join_follow_declared_algebra() {
        let left = LossContractV1::new(
            [loss("encoding")],
            [loss("encoding"), loss("ordering")],
            false,
        )
        .unwrap();
        let right = LossContractV1::new([loss("ordering")], [loss("ordering")], false).unwrap();

        let sequence = left.sequence(&right);
        assert_eq!(sequence.definite().len(), 2);
        assert_eq!(sequence.possible().len(), 2);

        let alternative = left.alternative_join(&right);
        assert!(alternative.definite().is_empty());
        assert_eq!(alternative.possible().len(), 2);
    }
}
