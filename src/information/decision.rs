use serde::{Deserialize, Serialize};

use super::{
    canonical_bytes, AtomIdV1, DecisionIdV1, EntityIdV1, InformationErrorV1, PayloadRefV1,
    ProjectionReceiptIdV1, ScopeV1, SnapshotRootIdV1,
};

pub const DECISION_RECEIPT_SCHEMA_V1: &str = "ostadix.info-decision/v1";
pub const OBSERVATION_RECORD_SCHEMA_V1: &str = "ostadix.info-observation/v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecisionCandidateV1 {
    pub strategy_identity: String,
    pub applicable: bool,
    pub upper_bound_cost_us: Option<u64>,
    pub explanation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecisionReceiptV1 {
    schema: String,
    source_root: SnapshotRootIdV1,
    read_set: Vec<AtomIdV1>,
    objective_identity: String,
    ruleset_sha256: String,
    candidates: Vec<DecisionCandidateV1>,
    selected_strategy: Option<String>,
    uncertainty: Vec<String>,
}

impl DecisionReceiptV1 {
    pub fn new(
        source_root: SnapshotRootIdV1,
        mut read_set: Vec<AtomIdV1>,
        objective_identity: impl Into<String>,
        ruleset_sha256: impl Into<String>,
        mut candidates: Vec<DecisionCandidateV1>,
        selected_strategy: Option<String>,
        mut uncertainty: Vec<String>,
    ) -> Result<Self, InformationErrorV1> {
        read_set.sort();
        read_set.dedup();
        candidates.sort_by(|left, right| left.strategy_identity.cmp(&right.strategy_identity));
        uncertainty.sort();
        uncertainty.dedup();
        let objective_identity = objective_identity.into();
        let ruleset_sha256 = ruleset_sha256.into();
        if read_set.is_empty() || objective_identity.is_empty() || candidates.is_empty() {
            return Err(InformationErrorV1::InvalidRecord(
                "decision receipt requires a read set, objective, and candidates".to_string(),
            ));
        }
        if ruleset_sha256.len() != 64
            || !ruleset_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InformationErrorV1::InvalidRecord(
                "decision ruleset digest must be lowercase sha256".to_string(),
            ));
        }
        if let Some(selected) = &selected_strategy {
            if !candidates
                .iter()
                .any(|candidate| candidate.applicable && candidate.strategy_identity == *selected)
            {
                return Err(InformationErrorV1::InvalidRecord(
                    "selected decision strategy is not an applicable candidate".to_string(),
                ));
            }
        }
        let receipt = Self {
            schema: DECISION_RECEIPT_SCHEMA_V1.to_string(),
            source_root,
            read_set,
            objective_identity,
            ruleset_sha256,
            candidates,
            selected_strategy,
            uncertainty,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != DECISION_RECEIPT_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported decision receipt schema `{}`",
                self.schema
            )));
        }
        if self.read_set.is_empty()
            || self.objective_identity.is_empty()
            || self.candidates.is_empty()
        {
            return Err(InformationErrorV1::InvalidRecord(
                "decision receipt requires a read set, objective, and candidates".to_string(),
            ));
        }
        if self.ruleset_sha256.len() != 64
            || !self
                .ruleset_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InformationErrorV1::InvalidRecord(
                "decision ruleset digest must be lowercase sha256".to_string(),
            ));
        }
        let mut normalized_read_set = self.read_set.clone();
        normalized_read_set.sort();
        normalized_read_set.dedup();
        let mut normalized_uncertainty = self.uncertainty.clone();
        normalized_uncertainty.sort();
        normalized_uncertainty.dedup();
        if normalized_read_set != self.read_set
            || normalized_uncertainty != self.uncertainty
            || self.uncertainty.iter().any(String::is_empty)
            || self.candidates.iter().any(|candidate| {
                candidate.strategy_identity.is_empty() || candidate.explanation.is_empty()
            })
            || self.candidates.windows(2).any(|pair| {
                pair[0].strategy_identity.as_str() >= pair[1].strategy_identity.as_str()
            })
        {
            return Err(InformationErrorV1::InvalidRecord(
                "decision receipt collections are empty, duplicated, or not normalized".to_string(),
            ));
        }
        if let Some(selected) = &self.selected_strategy {
            if !self
                .candidates
                .iter()
                .any(|candidate| candidate.applicable && candidate.strategy_identity == *selected)
            {
                return Err(InformationErrorV1::InvalidRecord(
                    "selected decision strategy is not an applicable candidate".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn id(&self) -> Result<DecisionIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(DecisionIdV1::digest(&canonical_bytes(self)?))
    }

    pub fn read_set(&self) -> &[AtomIdV1] {
        &self.read_set
    }

    pub fn selected_strategy(&self) -> Option<&str> {
        self.selected_strategy.as_deref()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationRecordV1 {
    schema: String,
    subject: EntityIdV1,
    measurement_schema: String,
    payload: PayloadRefV1,
    scope: ScopeV1,
    producer: EntityIdV1,
    projection_receipt: Option<ProjectionReceiptIdV1>,
    support: Vec<AtomIdV1>,
}

impl ObservationRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        subject: EntityIdV1,
        measurement_schema: impl Into<String>,
        payload: PayloadRefV1,
        scope: ScopeV1,
        producer: EntityIdV1,
        projection_receipt: Option<ProjectionReceiptIdV1>,
        mut support: Vec<AtomIdV1>,
    ) -> Result<Self, InformationErrorV1> {
        let measurement_schema = measurement_schema.into();
        if measurement_schema.is_empty() {
            return Err(InformationErrorV1::InvalidRecord(
                "observation measurement schema must be non-empty".to_string(),
            ));
        }
        scope.validate()?;
        support.sort();
        support.dedup();
        let record = Self {
            schema: OBSERVATION_RECORD_SCHEMA_V1.to_string(),
            subject,
            measurement_schema,
            payload,
            scope,
            producer,
            projection_receipt,
            support,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != OBSERVATION_RECORD_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported observation record schema `{}`",
                self.schema
            )));
        }
        if self.measurement_schema.is_empty() {
            return Err(InformationErrorV1::InvalidRecord(
                "observation measurement schema must be non-empty".to_string(),
            ));
        }
        self.payload.validate()?;
        self.scope.validate()?;
        let mut normalized_support = self.support.clone();
        normalized_support.sort();
        normalized_support.dedup();
        if normalized_support != self.support {
            return Err(InformationErrorV1::InvalidRecord(
                "observation support set is not normalized".to_string(),
            ));
        }
        Ok(())
    }

    pub fn id(&self) -> Result<super::ObservationIdV1, InformationErrorV1> {
        self.validate()?;
        Ok(super::ObservationIdV1::digest(&canonical_bytes(self)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::information::PublicScalarV1;

    #[test]
    fn selected_strategy_must_come_from_applicable_candidates() {
        let atom = AtomIdV1::from_sha256("11".repeat(32)).unwrap();
        let candidate = DecisionCandidateV1 {
            strategy_identity: "serial".to_string(),
            applicable: false,
            upper_bound_cost_us: Some(10),
            explanation: "blocked".to_string(),
        };
        assert!(DecisionReceiptV1::new(
            SnapshotRootIdV1::from_sha256("22".repeat(32)).unwrap(),
            vec![atom],
            "objective:test",
            "33".repeat(32),
            vec![candidate],
            Some("serial".to_string()),
            vec![],
        )
        .is_err());
    }

    #[test]
    fn observation_identity_binds_projection_and_scope() {
        let entity = EntityIdV1::from_sha256("11".repeat(32)).unwrap();
        let observation = ObservationRecordV1::new(
            entity.clone(),
            "ostadix.measurement/duration-us-v1",
            PayloadRefV1::public(PublicScalarV1::U64(53)).unwrap(),
            ScopeV1 {
                attempt_id: Some("attempt:1".to_string()),
                ..ScopeV1::default()
            },
            entity,
            Some(ProjectionReceiptIdV1::from_sha256("22".repeat(32)).unwrap()),
            vec![],
        )
        .unwrap();
        assert_eq!(observation.id().unwrap(), observation.id().unwrap());
    }

    #[test]
    fn decision_and_observation_reject_wrong_schemas() {
        let atom = AtomIdV1::from_sha256("11".repeat(32)).unwrap();
        let mut decision = DecisionReceiptV1::new(
            SnapshotRootIdV1::from_sha256("22".repeat(32)).unwrap(),
            vec![atom],
            "objective:test",
            "33".repeat(32),
            vec![DecisionCandidateV1 {
                strategy_identity: "serial".to_string(),
                applicable: true,
                upper_bound_cost_us: Some(10),
                explanation: "bounded".to_string(),
            }],
            Some("serial".to_string()),
            vec![],
        )
        .unwrap();
        decision.schema = "ostadix.info-decision/v0".to_string();
        assert!(decision.validate().is_err());

        let entity = EntityIdV1::from_sha256("44".repeat(32)).unwrap();
        let mut observation = ObservationRecordV1::new(
            entity.clone(),
            "ostadix.measurement/duration-us-v1",
            PayloadRefV1::public(PublicScalarV1::U64(1)).unwrap(),
            ScopeV1::default(),
            entity,
            None,
            vec![],
        )
        .unwrap();
        observation.schema = "ostadix.info-observation/v0".to_string();
        assert!(observation.validate().is_err());
    }
}
