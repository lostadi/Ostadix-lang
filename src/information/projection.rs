use serde::{Deserialize, Serialize};

use super::{
    canonical_bytes, AtomIdV1, InformationErrorV1, LossContractV1, ProjectionReceiptIdV1,
    SnapshotRootIdV1,
};

pub const PROJECTION_RECEIPT_SCHEMA_V1: &str = "ostadix.projection-receipt/v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionDirectionV1 {
    CanonicalToView,
    ViewToCanonicalLift,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionReceiptV1 {
    schema: String,
    source_root: SnapshotRootIdV1,
    read_set: Vec<AtomIdV1>,
    recipe_schema: String,
    recipe_implementation_sha256: String,
    recipe_configuration_sha256: String,
    output_sha256: String,
    source_substrate: String,
    target_substrate: String,
    direction: ProjectionDirectionV1,
    scope_identity: String,
    freshness_preconditions: Vec<String>,
    consumer_contract: String,
    lift_schema: String,
    identity_map_sha256: String,
    loss: LossContractV1,
}

impl ProjectionReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_root: SnapshotRootIdV1,
        mut read_set: Vec<AtomIdV1>,
        recipe_schema: impl Into<String>,
        recipe_implementation_sha256: impl Into<String>,
        recipe_configuration_sha256: impl Into<String>,
        output_sha256: impl Into<String>,
        source_substrate: impl Into<String>,
        target_substrate: impl Into<String>,
        direction: ProjectionDirectionV1,
        scope_identity: impl Into<String>,
        mut freshness_preconditions: Vec<String>,
        consumer_contract: impl Into<String>,
        lift_schema: impl Into<String>,
        identity_map_sha256: impl Into<String>,
        loss: LossContractV1,
    ) -> Result<Self, InformationErrorV1> {
        read_set.sort();
        read_set.dedup();
        freshness_preconditions.sort();
        freshness_preconditions.dedup();
        loss.validate()?;

        let receipt = Self {
            schema: PROJECTION_RECEIPT_SCHEMA_V1.to_string(),
            source_root,
            read_set,
            recipe_schema: recipe_schema.into(),
            recipe_implementation_sha256: recipe_implementation_sha256.into(),
            recipe_configuration_sha256: recipe_configuration_sha256.into(),
            output_sha256: output_sha256.into(),
            source_substrate: source_substrate.into(),
            target_substrate: target_substrate.into(),
            direction,
            scope_identity: scope_identity.into(),
            freshness_preconditions,
            consumer_contract: consumer_contract.into(),
            lift_schema: lift_schema.into(),
            identity_map_sha256: identity_map_sha256.into(),
            loss,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != PROJECTION_RECEIPT_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported projection receipt schema `{}`",
                self.schema
            )));
        }
        let digests = [
            &self.recipe_implementation_sha256,
            &self.recipe_configuration_sha256,
            &self.output_sha256,
            &self.identity_map_sha256,
        ];
        if digests.iter().any(|digest| {
            digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(InformationErrorV1::InvalidRecord(
                "projection receipt digests must be lowercase sha256".to_string(),
            ));
        }
        if self.read_set.is_empty()
            || self.recipe_schema.is_empty()
            || self.source_substrate.is_empty()
            || self.target_substrate.is_empty()
            || self.scope_identity.is_empty()
            || self.consumer_contract.is_empty()
            || self.lift_schema.is_empty()
        {
            return Err(InformationErrorV1::InvalidRecord(
                "projection receipt has an empty required field or read set".to_string(),
            ));
        }
        let mut normalized_read_set = self.read_set.clone();
        normalized_read_set.sort();
        normalized_read_set.dedup();
        let mut normalized_freshness = self.freshness_preconditions.clone();
        normalized_freshness.sort();
        normalized_freshness.dedup();
        if normalized_read_set != self.read_set
            || normalized_freshness != self.freshness_preconditions
            || self.freshness_preconditions.iter().any(String::is_empty)
        {
            return Err(InformationErrorV1::InvalidRecord(
                "projection receipt sets are not normalized or contain an empty value".to_string(),
            ));
        }
        self.loss.validate()
    }

    pub fn id(&self) -> Result<ProjectionReceiptIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(ProjectionReceiptIdV1::digest(&canonical_bytes(self)?))
    }

    pub fn source_root(&self) -> &SnapshotRootIdV1 {
        &self.source_root
    }

    pub fn read_set(&self) -> &[AtomIdV1] {
        &self.read_set
    }

    pub fn loss(&self) -> &LossContractV1 {
        &self.loss
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_normalizes_read_set_and_binds_loss() {
        let atom = AtomIdV1::from_sha256("11".repeat(32)).unwrap();
        let receipt = ProjectionReceiptV1::new(
            SnapshotRootIdV1::from_sha256("22".repeat(32)).unwrap(),
            vec![atom.clone(), atom],
            "ostadix.test-projector/v1",
            "33".repeat(32),
            "44".repeat(32),
            "55".repeat(32),
            "information-snapshot",
            "scheduler-table",
            ProjectionDirectionV1::CanonicalToView,
            "execution:test",
            vec!["head:stable".to_string(), "head:stable".to_string()],
            "scheduling-only",
            "ostadix.test-lift/v1",
            "66".repeat(32),
            LossContractV1::exact(),
        )
        .unwrap();

        assert_eq!(receipt.read_set().len(), 1);
        assert_eq!(
            receipt.loss().disposition(),
            super::super::ProjectionDispositionV1::Exact
        );
        assert_eq!(receipt.id().unwrap(), receipt.id().unwrap());
    }

    #[test]
    fn receipt_validation_rejects_wrong_schema_and_noncanonical_sets() {
        let atom = AtomIdV1::from_sha256("11".repeat(32)).unwrap();
        let mut receipt = ProjectionReceiptV1::new(
            SnapshotRootIdV1::from_sha256("22".repeat(32)).unwrap(),
            vec![atom],
            "ostadix.test-projector/v1",
            "33".repeat(32),
            "44".repeat(32),
            "55".repeat(32),
            "information-snapshot",
            "scheduler-table",
            ProjectionDirectionV1::CanonicalToView,
            "execution:test",
            vec!["head:stable".to_string()],
            "scheduling-only",
            "ostadix.test-lift/v1",
            "66".repeat(32),
            LossContractV1::exact(),
        )
        .unwrap();
        receipt.schema = "ostadix.projection-receipt/v0".to_string();
        assert!(receipt.validate().is_err());
        receipt.schema = PROJECTION_RECEIPT_SCHEMA_V1.to_string();
        receipt
            .freshness_preconditions
            .push("head:stable".to_string());
        assert!(receipt.validate().is_err());
    }
}
