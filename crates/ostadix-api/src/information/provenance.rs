//! Versioned, authority-free provenance claims and contextual recovery contracts.
//!
//! Information V1 atoms remain byte-for-byte unchanged.  V2 provenance is a
//! content-addressed sidecar keyed by the V1 [`AtomIdV1`].  Raw sidecars are
//! descriptive and deserializable; they do not become trusted merely because
//! they validate.  Authority-bearing analyzers live above this layer and may
//! return opaque admitted handles only after recomputing a sidecar from a
//! trusted source.

use std::collections::BTreeSet;
use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize};

use super::{
    canonical_bytes, id::domain_digest, AcquisitionModalityV1, AtomIdV1, BlobIdV1, EntityIdV1,
    InformationErrorV1, LossContractV1, NativeRecordRefV1, ObservationIdV1,
    ProjectionDispositionV1,
};

pub const INFORMATION_PROVENANCE_SCHEMA_V2: &str = "ostadix.info-provenance/v2";
pub const RECOVERY_QUESTION_SCHEMA_V1: &str = "ostadix.recovery-question/v1";
pub const CLAIM_STANDING_SCHEMA_V2: &str = "ostadix.claim-standing/v2";

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/' | b':' | b'+')
        })
}

fn validate_sha256(kind: &'static str, value: &str) -> Result<(), InformationErrorV1> {
    BlobIdV1::from_sha256(value.to_string())
        .map(|_| ())
        .map_err(|_| InformationErrorV1::InvalidDigest {
            kind,
            value: value.to_string(),
        })
}

fn validate_native_ref(value: &NativeRecordRefV1) -> Result<(), InformationErrorV1> {
    let normalized = NativeRecordRefV1::new(
        value.schema.clone(),
        value.media_type.clone(),
        value.sha256.clone(),
        value.logical_len,
    )?;
    if normalized == *value {
        Ok(())
    } else {
        Err(InformationErrorV1::InvalidRecord(
            "native record reference is not normalized".to_string(),
        ))
    }
}

/// Content identity of one canonical [`InformationProvenanceV2`] sidecar.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProvenanceRecordIdV2(String);

impl<'de> Deserialize<'de> for ProvenanceRecordIdV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_sha256(value).map_err(de::Error::custom)
    }
}

impl ProvenanceRecordIdV2 {
    pub fn from_sha256(value: impl Into<String>) -> Result<Self, InformationErrorV1> {
        let value = value.into();
        validate_sha256("information provenance", &value)?;
        Ok(Self(value))
    }

    fn digest(bytes: &[u8]) -> Self {
        Self(domain_digest(b"ostadix.info-provenance/v2", bytes))
    }

    pub fn as_sha256(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProvenanceRecordIdV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Acquisition origins are distinct from assurance mechanisms and later
/// claim standing.  In particular, enforcement is an assurance rather than an
/// origin, while contradiction and invalidation are relations over claims.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcquisitionOriginV2 {
    Assertion,
    Derivation,
    Measurement,
    Estimate,
    Counterfactual,
}

impl AcquisitionOriginV2 {
    pub const fn legacy_modality(self) -> AcquisitionModalityV1 {
        match self {
            Self::Assertion => AcquisitionModalityV1::Declared,
            Self::Derivation => AcquisitionModalityV1::Derived,
            Self::Measurement => AcquisitionModalityV1::Observed,
            Self::Estimate => AcquisitionModalityV1::Predicted,
            Self::Counterfactual => AcquisitionModalityV1::Counterfactual,
        }
    }
}

/// One ordered, role-bearing input to a derivation.
///
/// Unlike `InformationAtomV1::support`, this representation preserves order
/// and multiplicity.  The referenced V1 atom remains the content identity of
/// the input; no raw payload bytes or authority are embedded here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DerivationInputV2 {
    ordinal: u32,
    role: String,
    atom: AtomIdV1,
}

impl DerivationInputV2 {
    pub fn new(
        ordinal: u32,
        role: impl Into<String>,
        atom: AtomIdV1,
    ) -> Result<Self, InformationErrorV1> {
        let role = role.into();
        if !valid_token(&role) {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "invalid derivation input role `{role}`"
            )));
        }
        Ok(Self {
            ordinal,
            role,
            atom,
        })
    }

    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn atom(&self) -> &AtomIdV1 {
        &self.atom
    }

    fn validate(&self) -> Result<(), InformationErrorV1> {
        let normalized = Self::new(self.ordinal, self.role.clone(), self.atom.clone())?;
        if normalized == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "derivation input is not normalized".to_string(),
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivationInputBindingV2 {
    /// Complete ordered inputs are bound by an independently verified source.
    OrderedComplete,
    /// Migrated from V1's sorted/deduplicated support set; order,
    /// multiplicity, and completeness were not retained.
    LegacySupportSet,
}

/// Structure from which an acquisition origin is projected.
///
/// This enum is an authority-free claim vocabulary.  Its variants are not
/// proof by themselves; an authority-bearing analyzer must derive one from a
/// trusted source and compare the complete sidecar before it can be admitted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "origin", rename_all = "kebab-case")]
pub enum AcquisitionOriginWitnessV2 {
    Assertion,
    Derivation {
        procedure: NativeRecordRefV1,
        inputs: Vec<DerivationInputV2>,
        input_binding: DerivationInputBindingV2,
    },
    Measurement {
        observation: ObservationIdV1,
    },
    Estimate {
        model: NativeRecordRefV1,
        inputs: Vec<DerivationInputV2>,
        sampling_state: Option<BlobIdV1>,
    },
    Counterfactual {
        evaluator: NativeRecordRefV1,
        baseline: AtomIdV1,
        intervention: NativeRecordRefV1,
        domain: NativeRecordRefV1,
        inputs: Vec<DerivationInputV2>,
    },
}

impl AcquisitionOriginWitnessV2 {
    pub const fn origin(&self) -> AcquisitionOriginV2 {
        match self {
            Self::Assertion => AcquisitionOriginV2::Assertion,
            Self::Derivation { .. } => AcquisitionOriginV2::Derivation,
            Self::Measurement { .. } => AcquisitionOriginV2::Measurement,
            Self::Estimate { .. } => AcquisitionOriginV2::Estimate,
            Self::Counterfactual { .. } => AcquisitionOriginV2::Counterfactual,
        }
    }

    pub const fn legacy_modality(&self) -> AcquisitionModalityV1 {
        self.origin().legacy_modality()
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        match self {
            Self::Assertion | Self::Measurement { .. } => Ok(()),
            Self::Derivation {
                procedure, inputs, ..
            }
            | Self::Estimate {
                model: procedure,
                inputs,
                ..
            } => {
                validate_native_ref(procedure)?;
                validate_inputs(inputs)
            }
            Self::Counterfactual {
                evaluator,
                intervention,
                domain,
                inputs,
                ..
            } => {
                validate_native_ref(evaluator)?;
                validate_native_ref(intervention)?;
                validate_native_ref(domain)?;
                validate_inputs(inputs)
            }
        }
    }
}

fn validate_inputs(inputs: &[DerivationInputV2]) -> Result<(), InformationErrorV1> {
    for (expected, input) in inputs.iter().enumerate() {
        input.validate()?;
        let expected = u32::try_from(expected).map_err(|_| {
            InformationErrorV1::InvalidRecord("too many derivation inputs".to_string())
        })?;
        if input.ordinal != expected {
            return Err(InformationErrorV1::InvalidRecord(
                "derivation inputs must have contiguous canonical ordinals".to_string(),
            ));
        }
    }
    Ok(())
}

/// Assurance mechanisms that may accompany any acquisition origin.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "assurance", rename_all = "kebab-case")]
pub enum AssuranceWitnessV2 {
    /// A World receipt signature verified under the caller-supplied resolver.
    /// This does not establish signer authorization or bind the signer to the
    /// atom producer.
    ReceiptSignatureVerified {
        receipt_sha256: String,
        signer_key_id_sha256: String,
    },
    /// A plan node present in a compiler-produced, pre-execution V6 admission.
    /// This does not establish that the node ran or produced the atom.
    PreExecutionAdmissionVerified {
        admission_sha256: String,
        analyzer_sha256: String,
        plan_node: u64,
    },
    TrustedAdapter {
        adapter: String,
        receipt_sha256: String,
    },
    Enforced {
        policy: NativeRecordRefV1,
        authority: EntityIdV1,
        receipt_sha256: String,
    },
}

impl AssuranceWitnessV2 {
    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        match self {
            Self::ReceiptSignatureVerified {
                receipt_sha256,
                signer_key_id_sha256,
            } => {
                validate_sha256("signature-verified receipt", receipt_sha256)?;
                validate_sha256(
                    "signature-verified receipt signer key",
                    signer_key_id_sha256,
                )
            }
            Self::PreExecutionAdmissionVerified {
                admission_sha256,
                analyzer_sha256,
                ..
            } => {
                validate_sha256("execution admission", admission_sha256)?;
                validate_sha256("execution analyzer", analyzer_sha256)
            }
            Self::TrustedAdapter {
                adapter,
                receipt_sha256,
            } => {
                if !valid_token(adapter) {
                    return Err(InformationErrorV1::InvalidRecord(format!(
                        "invalid trusted adapter identity `{adapter}`"
                    )));
                }
                validate_sha256("trusted adapter receipt", receipt_sha256)
            }
            Self::Enforced {
                policy,
                receipt_sha256,
                ..
            } => {
                validate_native_ref(policy)?;
                validate_sha256("enforcement receipt", receipt_sha256)
            }
        }
    }
}

fn normalize_assurances(
    assurances: Vec<AssuranceWitnessV2>,
) -> Result<Vec<AssuranceWitnessV2>, InformationErrorV1> {
    let mut keyed = assurances
        .into_iter()
        .map(|assurance| Ok((canonical_bytes(&assurance)?, assurance)))
        .collect::<Result<Vec<_>, InformationErrorV1>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    keyed.dedup_by(|left, right| left.0 == right.0);
    Ok(keyed.into_iter().map(|(_, assurance)| assurance).collect())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvenanceClaimV2 {
    origin: AcquisitionOriginWitnessV2,
    assurances: Vec<AssuranceWitnessV2>,
}

impl ProvenanceClaimV2 {
    pub fn new(
        origin: AcquisitionOriginWitnessV2,
        assurances: Vec<AssuranceWitnessV2>,
    ) -> Result<Self, InformationErrorV1> {
        origin.validate()?;
        for assurance in &assurances {
            assurance.validate()?;
        }
        let assurances = normalize_assurances(assurances)?;
        let claim = Self { origin, assurances };
        claim.validate()?;
        Ok(claim)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        self.origin.validate()?;
        for assurance in &self.assurances {
            assurance.validate()?;
        }
        let normalized = normalize_assurances(self.assurances.clone())?;
        if normalized == self.assurances {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "provenance assurances are duplicated or not canonically ordered".to_string(),
            ))
        }
    }

    pub fn origin_witness(&self) -> &AcquisitionOriginWitnessV2 {
        &self.origin
    }

    pub const fn origin(&self) -> AcquisitionOriginV2 {
        self.origin.origin()
    }

    pub const fn legacy_modality(&self) -> AcquisitionModalityV1 {
        self.origin.legacy_modality()
    }

    pub fn assurances(&self) -> &[AssuranceWitnessV2] {
        &self.assurances
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryObjectV1 {
    AcquisitionOrigin,
    ProcedureExecution,
    SignedReport,
    PayloadValue,
    CounterfactualEvaluation,
    EnforcementMechanism,
}

/// The observation basis under which a recovery statement is made.
///
/// Recovery is not a zero-context property of a witness.  The equivalence
/// contract identifies which source distinctions matter; the domain identifies
/// the counterfactual/source space on which the statement is quantified.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryQuestionV1 {
    schema: String,
    object: RecoveryObjectV1,
    equivalence_contract: NativeRecordRefV1,
    domain: NativeRecordRefV1,
}

impl RecoveryQuestionV1 {
    pub fn new(
        object: RecoveryObjectV1,
        equivalence_contract: NativeRecordRefV1,
        domain: NativeRecordRefV1,
    ) -> Result<Self, InformationErrorV1> {
        let question = Self {
            schema: RECOVERY_QUESTION_SCHEMA_V1.to_string(),
            object,
            equivalence_contract,
            domain,
        };
        question.validate()?;
        Ok(question)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != RECOVERY_QUESTION_SCHEMA_V1 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported recovery question schema `{}`",
                self.schema
            )));
        }
        validate_native_ref(&self.equivalence_contract)?;
        validate_native_ref(&self.domain)
    }

    pub fn object(&self) -> RecoveryObjectV1 {
        self.object
    }

    pub fn equivalence_contract(&self) -> &NativeRecordRefV1 {
        &self.equivalence_contract
    }

    pub fn domain(&self) -> &NativeRecordRefV1 {
        &self.domain
    }
}

/// An independently dischargeable condition for a recovery claim.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "obligation", rename_all = "kebab-case")]
pub enum RecoveryObligationV2 {
    ProducerAuthentication,
    ProcedureResolution {
        procedure_sha256: String,
    },
    TypedInputCompleteness,
    SupportRecovery,
    ExecutionAdmissionBinding {
        admission_sha256: String,
    },
    ReceiptSignatureVerification {
        receipt_sha256: String,
    },
    SignerAuthorization {
        signer_key_id_sha256: String,
    },
    ReceiptCurrentness {
        receipt_sha256: String,
    },
    TerminalOutputBinding {
        receipt_sha256: String,
    },
    PlanNodeReceiptBinding {
        plan_node: u64,
        receipt_sha256: String,
    },
    ExecutionFidelity {
        admission_sha256: String,
    },
    DeterministicEffectClosure {
        plan_node: u64,
    },
    MorphismFidelity {
        plan_node: u64,
    },
    ObservationEventBinding {
        observation: ObservationIdV1,
    },
    SamplingStateBinding,
    EnforcementReceiptBinding {
        receipt_sha256: String,
    },
    CounterfactualDomainBinding {
        domain_sha256: String,
    },
}

impl RecoveryObligationV2 {
    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        match self {
            Self::ProducerAuthentication
            | Self::TypedInputCompleteness
            | Self::SupportRecovery
            | Self::DeterministicEffectClosure { .. }
            | Self::MorphismFidelity { .. }
            | Self::ObservationEventBinding { .. }
            | Self::SamplingStateBinding => Ok(()),
            Self::ProcedureResolution { procedure_sha256 } => {
                validate_sha256("procedure obligation", procedure_sha256)
            }
            Self::ExecutionAdmissionBinding { admission_sha256 }
            | Self::ExecutionFidelity { admission_sha256 } => {
                validate_sha256("execution admission obligation", admission_sha256)
            }
            Self::ReceiptSignatureVerification { receipt_sha256 }
            | Self::ReceiptCurrentness { receipt_sha256 }
            | Self::TerminalOutputBinding { receipt_sha256 }
            | Self::EnforcementReceiptBinding { receipt_sha256 } => {
                validate_sha256("receipt obligation", receipt_sha256)
            }
            Self::SignerAuthorization {
                signer_key_id_sha256,
            } => validate_sha256("signer authorization obligation", signer_key_id_sha256),
            Self::PlanNodeReceiptBinding { receipt_sha256, .. } => {
                validate_sha256("plan-node receipt obligation", receipt_sha256)
            }
            Self::CounterfactualDomainBinding { domain_sha256 } => {
                validate_sha256("counterfactual domain obligation", domain_sha256)
            }
        }
    }
}

/// Evidence that discharged one recovery obligation.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RecoveryDischargeV2 {
    obligation: RecoveryObligationV2,
    evidence_sha256: String,
}

impl RecoveryDischargeV2 {
    pub fn new(
        obligation: RecoveryObligationV2,
        evidence_sha256: impl Into<String>,
    ) -> Result<Self, InformationErrorV1> {
        let discharge = Self {
            obligation,
            evidence_sha256: evidence_sha256.into(),
        };
        discharge.validate()?;
        Ok(discharge)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        self.obligation.validate()?;
        validate_sha256("recovery discharge evidence", &self.evidence_sha256)
    }

    pub fn obligation(&self) -> &RecoveryObligationV2 {
        &self.obligation
    }

    pub fn evidence_sha256(&self) -> &str {
        &self.evidence_sha256
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryStatusV2 {
    Exact,
    Lossy,
    Opaque,
    Unestablished,
    Unsupported,
}

/// Contextual result of a recovery analyzer.
///
/// Intrinsic loss and outstanding obligations are deliberately separate.  A
/// later evidence layer may discharge an obligation; it cannot erase a
/// distinction that the representation did not retain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "assessment", rename_all = "kebab-case")]
pub enum RecoveryAssessmentV2 {
    Unsupported {
        reason: String,
    },
    Assessed {
        question: Box<RecoveryQuestionV1>,
        intrinsic_loss: Box<LossContractV1>,
        outstanding: BTreeSet<RecoveryObligationV2>,
        discharges: Vec<RecoveryDischargeV2>,
    },
}

impl RecoveryAssessmentV2 {
    pub fn assessed(
        question: RecoveryQuestionV1,
        intrinsic_loss: LossContractV1,
        outstanding: impl IntoIterator<Item = RecoveryObligationV2>,
        discharges: impl IntoIterator<Item = RecoveryDischargeV2>,
    ) -> Result<Self, InformationErrorV1> {
        let mut discharges = discharges.into_iter().collect::<Vec<_>>();
        discharges.sort();
        discharges.dedup();
        let assessment = Self::Assessed {
            question: Box::new(question),
            intrinsic_loss: Box::new(intrinsic_loss),
            outstanding: outstanding.into_iter().collect(),
            discharges,
        };
        assessment.validate()?;
        Ok(assessment)
    }

    pub fn unsupported(reason: impl Into<String>) -> Result<Self, InformationErrorV1> {
        let assessment = Self::Unsupported {
            reason: reason.into(),
        };
        assessment.validate()?;
        Ok(assessment)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        match self {
            Self::Unsupported { reason } => {
                if reason.is_empty() {
                    Err(InformationErrorV1::InvalidRecord(
                        "unsupported recovery assessment requires a reason".to_string(),
                    ))
                } else {
                    Ok(())
                }
            }
            Self::Assessed {
                question,
                intrinsic_loss,
                outstanding,
                discharges,
            } => {
                question.validate()?;
                intrinsic_loss.validate()?;
                for obligation in outstanding {
                    obligation.validate()?;
                }
                let mut normalized = discharges.clone();
                normalized.sort();
                normalized.dedup();
                if normalized != *discharges {
                    return Err(InformationErrorV1::InvalidRecord(
                        "recovery discharges are duplicated or not canonically ordered".to_string(),
                    ));
                }
                for discharge in discharges {
                    discharge.validate()?;
                    if outstanding.contains(discharge.obligation()) {
                        return Err(InformationErrorV1::InvalidRecord(
                            "a recovery obligation cannot be both outstanding and discharged"
                                .to_string(),
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    pub fn status(&self) -> RecoveryStatusV2 {
        match self {
            Self::Unsupported { .. } => RecoveryStatusV2::Unsupported,
            Self::Assessed {
                outstanding,
                intrinsic_loss,
                ..
            } if !outstanding.is_empty() => RecoveryStatusV2::Unestablished,
            Self::Assessed { intrinsic_loss, .. } => match intrinsic_loss.disposition() {
                ProjectionDispositionV1::Exact => RecoveryStatusV2::Exact,
                ProjectionDispositionV1::Lossy => RecoveryStatusV2::Lossy,
                ProjectionDispositionV1::Opaque => RecoveryStatusV2::Opaque,
            },
        }
    }

    pub fn outstanding(&self) -> Option<&BTreeSet<RecoveryObligationV2>> {
        match self {
            Self::Unsupported { .. } => None,
            Self::Assessed { outstanding, .. } => Some(outstanding),
        }
    }
}

/// Shared contextual recovery interface.  Implement this trait on analyzers,
/// not on raw records: the context and recovery question are part of the
/// proposition being assessed.
pub trait RecoveryAnalyzerV1 {
    type Subject;
    type Context;
    type Error;

    fn assess(
        &self,
        subject: &Self::Subject,
        question: &RecoveryQuestionV1,
        context: &Self::Context,
    ) -> Result<RecoveryAssessmentV2, Self::Error>;
}

/// Authority-free V2 provenance sidecar for an immutable V1 atom.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InformationProvenanceV2 {
    schema: String,
    atom: AtomIdV1,
    analyzer_sha256: String,
    source_sha256: String,
    claim: ProvenanceClaimV2,
    recovery: RecoveryAssessmentV2,
}

impl InformationProvenanceV2 {
    pub fn new(
        atom: AtomIdV1,
        analyzer_sha256: impl Into<String>,
        source_sha256: impl Into<String>,
        claim: ProvenanceClaimV2,
        recovery: RecoveryAssessmentV2,
    ) -> Result<Self, InformationErrorV1> {
        let record = Self {
            schema: INFORMATION_PROVENANCE_SCHEMA_V2.to_string(),
            atom,
            analyzer_sha256: analyzer_sha256.into(),
            source_sha256: source_sha256.into(),
            claim,
            recovery,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        if self.schema != INFORMATION_PROVENANCE_SCHEMA_V2 {
            return Err(InformationErrorV1::InvalidRecord(format!(
                "unsupported information provenance schema `{}`",
                self.schema
            )));
        }
        validate_sha256("provenance analyzer", &self.analyzer_sha256)?;
        validate_sha256("provenance source", &self.source_sha256)?;
        self.claim.validate()?;
        self.recovery.validate()
    }

    pub fn id(&self) -> Result<ProvenanceRecordIdV2, InformationErrorV1> {
        self.validate()?;
        Ok(ProvenanceRecordIdV2::digest(&canonical_bytes(self)?))
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, InformationErrorV1> {
        self.validate()?;
        canonical_bytes(self)
    }

    pub fn atom(&self) -> &AtomIdV1 {
        &self.atom
    }

    pub fn analyzer_sha256(&self) -> &str {
        &self.analyzer_sha256
    }

    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    pub fn claim(&self) -> &ProvenanceClaimV2 {
        &self.claim
    }

    pub fn recovery(&self) -> &RecoveryAssessmentV2 {
        &self.recovery
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimStandingDispositionV2 {
    Contradicted,
    Invalidated,
}

/// A ruling about a claim, kept separate from the immutable claim it concerns.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClaimStandingV2 {
    schema: String,
    subject: AtomIdV1,
    ruling: AtomIdV1,
    disposition: ClaimStandingDispositionV2,
}

impl ClaimStandingV2 {
    pub fn new(
        subject: AtomIdV1,
        ruling: AtomIdV1,
        disposition: ClaimStandingDispositionV2,
    ) -> Result<Self, InformationErrorV1> {
        if subject == ruling {
            return Err(InformationErrorV1::InvalidRecord(
                "claim standing ruling must be distinct from its subject".to_string(),
            ));
        }
        Ok(Self {
            schema: CLAIM_STANDING_SCHEMA_V2.to_string(),
            subject,
            ruling,
            disposition,
        })
    }

    pub fn validate(&self) -> Result<(), InformationErrorV1> {
        let normalized = Self::new(self.subject.clone(), self.ruling.clone(), self.disposition)?;
        if normalized == *self {
            Ok(())
        } else {
            Err(InformationErrorV1::InvalidRecord(
                "claim standing record is not normalized".to_string(),
            ))
        }
    }

    pub fn subject(&self) -> &AtomIdV1 {
        &self.subject
    }

    pub fn ruling(&self) -> &AtomIdV1 {
        &self.ruling
    }

    pub fn disposition(&self) -> ClaimStandingDispositionV2 {
        self.disposition
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::information::{EntityDescriptorV1, PublicScalarV1};
    use sha2::Digest;
    use std::collections::BTreeMap;

    fn atom(label: &str) -> AtomIdV1 {
        AtomIdV1::from_sha256(hex::encode(sha2::Sha256::digest(label.as_bytes()))).unwrap()
    }

    fn native(schema: &str, label: &str) -> NativeRecordRefV1 {
        NativeRecordRefV1::new(
            schema,
            "application/octet-stream",
            hex::encode(sha2::Sha256::digest(label.as_bytes())),
            label.len() as u64,
        )
        .unwrap()
    }

    #[test]
    fn origin_is_projected_and_enforcement_is_an_orthogonal_assurance() {
        let claim = ProvenanceClaimV2::new(
            AcquisitionOriginWitnessV2::Derivation {
                procedure: native("ostadix.procedure/v2", "procedure"),
                inputs: vec![DerivationInputV2::new(0, "argument", atom("input")).unwrap()],
                input_binding: DerivationInputBindingV2::OrderedComplete,
            },
            vec![AssuranceWitnessV2::Enforced {
                policy: native("ostadix.policy/v1", "policy"),
                authority: EntityDescriptorV1::new(
                    "ostadix",
                    "authority",
                    BTreeMap::from([("name".to_string(), "kernel".to_string())]),
                )
                .unwrap()
                .id()
                .unwrap(),
                receipt_sha256: "11".repeat(32),
            }],
        )
        .unwrap();
        assert_eq!(claim.origin(), AcquisitionOriginV2::Derivation);
        assert_eq!(claim.legacy_modality(), AcquisitionModalityV1::Derived);
        assert!(matches!(
            claim.assurances(),
            [AssuranceWitnessV2::Enforced { .. }]
        ));
    }

    #[test]
    fn recovery_is_unestablished_until_obligations_are_discharged() {
        let question = RecoveryQuestionV1::new(
            RecoveryObjectV1::ProcedureExecution,
            native("ostadix.equivalence/v1", "equivalence"),
            native("ostadix.domain/v1", "domain"),
        )
        .unwrap();
        let obligation = RecoveryObligationV2::ExecutionFidelity {
            admission_sha256: "22".repeat(32),
        };
        let pending = RecoveryAssessmentV2::assessed(
            question.clone(),
            LossContractV1::exact(),
            [obligation.clone()],
            [],
        )
        .unwrap();
        assert_eq!(pending.status(), RecoveryStatusV2::Unestablished);

        let established = RecoveryAssessmentV2::assessed(
            question,
            LossContractV1::exact(),
            [],
            [RecoveryDischargeV2::new(obligation, "33".repeat(32)).unwrap()],
        )
        .unwrap();
        assert_eq!(established.status(), RecoveryStatusV2::Exact);
    }

    #[test]
    fn v1_standing_modalities_are_not_v2_acquisition_origins() {
        let standing = ClaimStandingV2::new(
            atom("claim"),
            atom("ruling"),
            ClaimStandingDispositionV2::Contradicted,
        )
        .unwrap();
        standing.validate().unwrap();
        assert_eq!(
            standing.disposition(),
            ClaimStandingDispositionV2::Contradicted
        );

        // Keep an otherwise-unused V1 scalar in this test module so the V2
        // sidecar remains visibly independent of V1 payload construction.
        let _ = PublicScalarV1::Null;
    }

    #[test]
    fn derivation_inputs_preserve_multiplicity_and_require_canonical_ordinals() {
        let repeated = atom("same-input");
        let witness = AcquisitionOriginWitnessV2::Derivation {
            procedure: native("ostadix.procedure/v2", "procedure"),
            inputs: vec![
                DerivationInputV2::new(0, "left", repeated.clone()).unwrap(),
                DerivationInputV2::new(1, "right", repeated).unwrap(),
            ],
            input_binding: DerivationInputBindingV2::OrderedComplete,
        };
        witness.validate().unwrap();

        let malformed = AcquisitionOriginWitnessV2::Derivation {
            procedure: native("ostadix.procedure/v2", "procedure"),
            inputs: vec![DerivationInputV2::new(1, "first", atom("input")).unwrap()],
            input_binding: DerivationInputBindingV2::OrderedComplete,
        };
        assert!(malformed.validate().is_err());
    }

    #[test]
    fn v1_atom_bytes_and_identity_remain_frozen_beside_the_v2_sidecar() {
        let subject = EntityDescriptorV1::new(
            "ostadix",
            "subject",
            BTreeMap::from([("name".to_string(), "v1-golden".to_string())]),
        )
        .unwrap();
        let producer = EntityDescriptorV1::new(
            "ostadix",
            "producer",
            BTreeMap::from([("name".to_string(), "v1-golden".to_string())]),
        )
        .unwrap();
        let value = crate::information::InformationAtomV1::new(
            vec![crate::information::ParticipantV1::new("subject", subject.id().unwrap()).unwrap()],
            "ostadix.v1-golden/fact-v1",
            crate::information::PayloadRefV1::public(PublicScalarV1::U64(7)).unwrap(),
            AcquisitionModalityV1::Declared,
            crate::information::ScopeV1::default(),
            producer.id().unwrap(),
            vec![],
        )
        .unwrap();
        let packed = crate::information::PackedInformationObjectV1::from_atom(&value).unwrap();
        assert_eq!(
            hex::encode(&packed.canonical_bytes),
            "ac6573636f7065a8676e6f64655f6964f66a617474656d70745f6964f66a67656e65726174696f6ef66c657865637574696f6e5f6964f66f61727469666163745f736861323536f672656e7669726f6e6d656e745f736861323536f67276616c69645f66726f6d5f756e69785f6d73f67376616c69645f756e74696c5f756e69785f6d73f666736368656d61746f7374616469782e696e666f2d61746f6d2f7631677061796c6f6164a26474696572627430677061796c6f6164a2646b696e64637536346576616c75650767737570706f727480686d6f64616c697479686465636c617265646870726f64756365727840336334366333396263326537633063633038356462323636303135393131633636653537393934353836333639386334653234313861663062366133616331386b6166666f7264616e636573806c7061727469636970616e747381a264726f6c65677375626a65637466656e746974797840633466643765656634303538653066383938663666323165323031633536333364663138633265613737396161636339303561363734366462623632343833386e636f6e666964656e63655f70706df6707072656469636174655f736368656d6178196f7374616469782e76312d676f6c64656e2f666163742d76317364657269766174696f6e5f6964656e74697479f678197472616e73706172656e63795f636f6e73657175656e63657380"
        );
        assert_eq!(
            value.id().unwrap().as_sha256(),
            "eeade6a53003d79138d1b752ba27406a9ee3322b6acd28de350f0eca74f67ed0"
        );
    }
}
