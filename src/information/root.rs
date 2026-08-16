use serde::{Deserialize, Serialize};

use super::{canonical_bytes, AtomIdV1, InformationErrorV1, RevisionIdV1, SnapshotRootIdV1};

pub const INFORMATION_SNAPSHOT_SCHEMA_V1: &str = "ostadix.info-snapshot/v1";
pub const INFORMATION_REVISION_SCHEMA_V1: &str = "ostadix.info-revision/v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InformationSnapshotV1 {
    schema: String,
    facts: Vec<AtomIdV1>,
}

impl InformationSnapshotV1 {
    pub fn new(mut facts: Vec<AtomIdV1>) -> Self {
        facts.sort();
        facts.dedup();
        Self {
            schema: INFORMATION_SNAPSHOT_SCHEMA_V1.to_string(),
            facts,
        }
    }

    pub fn facts(&self) -> &[AtomIdV1] {
        &self.facts
    }

    pub fn id(&self) -> Result<SnapshotRootIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(SnapshotRootIdV1::digest(&canonical_bytes(self)?))
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != INFORMATION_SNAPSHOT_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported information snapshot schema `{}`",
                self.schema
            )));
        }
        if Self::new(self.facts.clone()) == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "information snapshot facts are not normalized".to_string(),
            ))
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InformationRevisionV1 {
    schema: String,
    snapshot: SnapshotRootIdV1,
    parents: Vec<RevisionIdV1>,
    reconciliation_identity: Option<String>,
}

impl InformationRevisionV1 {
    pub fn new(
        snapshot: SnapshotRootIdV1,
        mut parents: Vec<RevisionIdV1>,
        reconciliation_identity: Option<String>,
    ) -> Result<Self, InformationErrorV1> {
        parents.sort();
        parents.dedup();
        if parents.len() > 2 {
            return Err(InformationErrorV1::TooManyParents);
        }
        Ok(Self {
            schema: INFORMATION_REVISION_SCHEMA_V1.to_string(),
            snapshot,
            parents,
            reconciliation_identity,
        })
    }

    pub fn snapshot(&self) -> &SnapshotRootIdV1 {
        &self.snapshot
    }

    pub fn parents(&self) -> &[RevisionIdV1] {
        &self.parents
    }

    pub fn id(&self) -> Result<RevisionIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(RevisionIdV1::digest(&canonical_bytes(self)?))
    }

    pub fn reconciliation_identity(&self) -> Option<&str> {
        self.reconciliation_identity.as_deref()
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != INFORMATION_REVISION_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported information revision schema `{}`",
                self.schema
            )));
        }
        if self
            .reconciliation_identity
            .as_ref()
            .is_some_and(|identity| identity.is_empty())
        {
            return Err(InformationErrorV1::InvalidRecord(
                "revision reconciliation identity must be non-empty".to_string(),
            ));
        }
        let normalized = Self::new(
            self.snapshot.clone(),
            self.parents.clone(),
            self.reconciliation_identity.clone(),
        )?;
        if normalized == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "information revision parents are not normalized".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(byte: u8) -> AtomIdV1 {
        AtomIdV1::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
    }

    #[test]
    fn snapshot_identity_depends_on_fact_set_not_insertion_order() {
        let left = InformationSnapshotV1::new(vec![atom(2), atom(1), atom(2)]);
        let right = InformationSnapshotV1::new(vec![atom(1), atom(2)]);
        assert_eq!(left.id().unwrap(), right.id().unwrap());
    }

    #[test]
    fn revision_lineage_is_separate_from_snapshot_identity() {
        let snapshot = InformationSnapshotV1::new(vec![atom(1)]).id().unwrap();
        let genesis = InformationRevisionV1::new(snapshot.clone(), vec![], None).unwrap();
        let parent = genesis.id().unwrap();
        let append = InformationRevisionV1::new(snapshot.clone(), vec![parent], None).unwrap();
        assert_eq!(genesis.snapshot(), append.snapshot());
        assert_ne!(genesis.id().unwrap(), append.id().unwrap());
    }
}
