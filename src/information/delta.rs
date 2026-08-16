use serde::{Deserialize, Serialize};

use super::{canonical_bytes, AtomIdV1, DeltaIdV1, EntityIdV1, InformationErrorV1, RevisionIdV1};

pub const INFORMATION_DELTA_SCHEMA_V1: &str = "ostadix.info-delta/v1";

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct HeadCoordinateV1 {
    pub subject: EntityIdV1,
    pub predicate_schema: String,
    pub scope_identity: String,
}

impl HeadCoordinateV1 {
    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.predicate_schema.is_empty() || self.scope_identity.is_empty() {
            Err(InformationErrorV1::InvalidRecord(
                "expected-head predicate schema and scope identity must be non-empty".to_string(),
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ExpectedHeadSetV1 {
    pub coordinate: HeadCoordinateV1,
    pub heads: Vec<AtomIdV1>,
}

impl ExpectedHeadSetV1 {
    pub fn new(coordinate: HeadCoordinateV1, mut heads: Vec<AtomIdV1>) -> Self {
        heads.sort();
        heads.dedup();
        Self { coordinate, heads }
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        self.coordinate.validate()?;
        if Self::new(self.coordinate.clone(), self.heads.clone()) == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "expected-head atom set is not normalized".to_string(),
            ))
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InformationDeltaV1 {
    schema: String,
    base_revision: RevisionIdV1,
    producer: EntityIdV1,
    additions: Vec<AtomIdV1>,
    expected_heads: Vec<ExpectedHeadSetV1>,
}

impl InformationDeltaV1 {
    pub fn new(
        base_revision: RevisionIdV1,
        producer: EntityIdV1,
        mut additions: Vec<AtomIdV1>,
        mut expected_heads: Vec<ExpectedHeadSetV1>,
    ) -> Result<Self, InformationErrorV1> {
        additions.sort();
        additions.dedup();
        if additions.is_empty() {
            return Err(InformationErrorV1::InvalidRecord(
                "information delta must add at least one atom".to_string(),
            ));
        }
        for expected in &mut expected_heads {
            expected.coordinate.validate()?;
            expected.heads.sort();
            expected.heads.dedup();
        }
        expected_heads.sort();
        if expected_heads
            .windows(2)
            .any(|pair| pair[0].coordinate == pair[1].coordinate && pair[0].heads != pair[1].heads)
        {
            return Err(InformationErrorV1::InvalidRecord(
                "information delta repeats one expected-head coordinate with different head sets"
                    .to_string(),
            ));
        }
        expected_heads.dedup();
        Ok(Self {
            schema: INFORMATION_DELTA_SCHEMA_V1.to_string(),
            base_revision,
            producer,
            additions,
            expected_heads,
        })
    }

    pub fn id(&self) -> Result<DeltaIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(DeltaIdV1::digest(&canonical_bytes(self)?))
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != INFORMATION_DELTA_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported information delta schema `{}`",
                self.schema
            )));
        }
        let normalized = Self::new(
            self.base_revision.clone(),
            self.producer.clone(),
            self.additions.clone(),
            self.expected_heads.clone(),
        )?;
        if normalized == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "information delta is not in normalized canonical form".to_string(),
            ))
        }
    }

    pub fn base_revision(&self) -> &RevisionIdV1 {
        &self.base_revision
    }

    pub fn producer(&self) -> &EntityIdV1 {
        &self.producer
    }

    pub fn additions(&self) -> &[AtomIdV1] {
        &self.additions
    }

    pub fn expected_heads(&self) -> &[ExpectedHeadSetV1] {
        &self.expected_heads
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReconciliationDispositionV1 {
    CurrentEligible,
    HistoricalOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HeadConflictV1 {
    pub coordinate: HeadCoordinateV1,
    pub expected: Vec<AtomIdV1>,
    pub observed: Vec<AtomIdV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeltaReconciliationV1 {
    pub delta: DeltaIdV1,
    pub disposition: ReconciliationDispositionV1,
    pub conflicts: Vec<HeadConflictV1>,
}

impl DeltaReconciliationV1 {
    pub fn from_heads(
        delta: DeltaIdV1,
        expected: &[ExpectedHeadSetV1],
        observed: &[ExpectedHeadSetV1],
    ) -> Self {
        let observed = observed
            .iter()
            .map(|entry| (&entry.coordinate, &entry.heads))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut conflicts = Vec::new();
        for entry in expected {
            let actual = observed
                .get(&entry.coordinate)
                .map(|heads| (*heads).clone())
                .unwrap_or_default();
            if entry.heads != actual {
                conflicts.push(HeadConflictV1 {
                    coordinate: entry.coordinate.clone(),
                    expected: entry.heads.clone(),
                    observed: actual,
                });
            }
        }
        Self {
            delta,
            disposition: if conflicts.is_empty() {
                ReconciliationDispositionV1::CurrentEligible
            } else {
                ReconciliationDispositionV1::HistoricalOnly
            },
            conflicts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(byte: u8, constructor: impl FnOnce(String) -> T) -> T {
        constructor(format!("{byte:02x}").repeat(32))
    }

    #[test]
    fn changed_expected_head_preserves_delta_as_historical() {
        let coordinate = HeadCoordinateV1 {
            subject: EntityIdV1::from_sha256("11".repeat(32)).unwrap(),
            predicate_schema: "ostadix.test/head-v1".to_string(),
            scope_identity: "execution:test".to_string(),
        };
        let expected = ExpectedHeadSetV1::new(
            coordinate.clone(),
            vec![id(0x22, |value| AtomIdV1::from_sha256(value).unwrap())],
        );
        let observed = ExpectedHeadSetV1::new(
            coordinate,
            vec![id(0x33, |value| AtomIdV1::from_sha256(value).unwrap())],
        );
        let result = DeltaReconciliationV1::from_heads(
            DeltaIdV1::from_sha256("44".repeat(32)).unwrap(),
            &[expected],
            &[observed],
        );
        assert_eq!(
            result.disposition,
            ReconciliationDispositionV1::HistoricalOnly
        );
        assert_eq!(result.conflicts.len(), 1);
    }

    #[test]
    fn delta_validation_rejects_nonnormalized_inner_head_sets() {
        let coordinate = HeadCoordinateV1 {
            subject: EntityIdV1::from_sha256("11".repeat(32)).unwrap(),
            predicate_schema: "ostadix.test/head-v1".to_string(),
            scope_identity: "execution:test".to_string(),
        };
        let a = AtomIdV1::from_sha256("22".repeat(32)).unwrap();
        let b = AtomIdV1::from_sha256("33".repeat(32)).unwrap();
        let delta = InformationDeltaV1::new(
            RevisionIdV1::from_sha256("44".repeat(32)).unwrap(),
            EntityIdV1::from_sha256("55".repeat(32)).unwrap(),
            vec![AtomIdV1::from_sha256("66".repeat(32)).unwrap()],
            vec![ExpectedHeadSetV1::new(
                coordinate,
                vec![a.clone(), b.clone()],
            )],
        )
        .unwrap();
        let mut encoded = serde_json::to_value(delta).unwrap();
        encoded["expected_heads"][0]["heads"] = serde_json::json!([b, a.clone(), a]);
        let nonnormalized: InformationDeltaV1 = serde_json::from_value(encoded).unwrap();
        assert!(nonnormalized.validate().is_err());
        assert!(nonnormalized.id().is_err());
    }

    #[test]
    fn delta_rejects_empty_scope_and_conflicting_duplicate_coordinates() {
        let subject = EntityIdV1::from_sha256("11".repeat(32)).unwrap();
        let invalid_coordinate = HeadCoordinateV1 {
            subject: subject.clone(),
            predicate_schema: "ostadix.test/head-v1".to_string(),
            scope_identity: String::new(),
        };
        assert!(InformationDeltaV1::new(
            RevisionIdV1::from_sha256("22".repeat(32)).unwrap(),
            EntityIdV1::from_sha256("33".repeat(32)).unwrap(),
            vec![AtomIdV1::from_sha256("44".repeat(32)).unwrap()],
            vec![ExpectedHeadSetV1::new(
                invalid_coordinate,
                vec![AtomIdV1::from_sha256("55".repeat(32)).unwrap()],
            )],
        )
        .is_err());

        let coordinate = HeadCoordinateV1 {
            subject,
            predicate_schema: "ostadix.test/head-v1".to_string(),
            scope_identity: "execution:test".to_string(),
        };
        assert!(InformationDeltaV1::new(
            RevisionIdV1::from_sha256("22".repeat(32)).unwrap(),
            EntityIdV1::from_sha256("33".repeat(32)).unwrap(),
            vec![AtomIdV1::from_sha256("44".repeat(32)).unwrap()],
            vec![
                ExpectedHeadSetV1::new(
                    coordinate.clone(),
                    vec![AtomIdV1::from_sha256("55".repeat(32)).unwrap()],
                ),
                ExpectedHeadSetV1::new(
                    coordinate,
                    vec![AtomIdV1::from_sha256("66".repeat(32)).unwrap()],
                ),
            ],
        )
        .is_err());
    }
}
