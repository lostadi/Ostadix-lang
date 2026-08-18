//! Context-bound image admission for Information Provenance V2.
//!
//! The lower [`crate::information`] layer defines descriptive sidecar records
//! and the contextual recovery algebra.  This layer is intentionally separate:
//! it is allowed to consume opaque execution admission and verified World
//! receipt handles, derive one provenance record, and return an opaque admitted
//! handle only when a supplied record is exactly that derivation.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::evidence::{AdmittedExecutionV6, DispatchSemanticsV1, RuntimeSnapshotKindV1};
use crate::information::{
    AcquisitionModalityV1, AcquisitionOriginV2, AcquisitionOriginWitnessV2, AssuranceWitnessV2,
    DerivationInputBindingV2, DerivationInputV2, InformationAtomV1, InformationErrorV1,
    InformationProvenanceV2, LossContractV1, LossKindV1, NativeRecordRefV1, PayloadRefV1,
    ProvenanceClaimV2, RecoveryAnalyzerV1, RecoveryAssessmentV2, RecoveryDischargeV2,
    RecoveryObjectV1, RecoveryObligationV2, RecoveryQuestionV1, RecoveryStatusV2,
};
use crate::ir::PlanNodeId;
use crate::world::{
    project_receipt_semantic_sha256_v1, receipt_v1_sha256, ObjectRoleV1, ReceiptTerminalV1,
    VerifiedExecutionReceiptV1,
};

pub const INFORMATION_PROVENANCE_ANALYZER_V2: &str = "ostadix.information-provenance-analyzer/v2";
pub const EXECUTION_PROVENANCE_RECEIPT_GATE_V2: &str = "ostadix.execution-admission/v6";

const INFORMATION_PROVENANCE_ANALYZER_SEMANTICS_V2: &[u8] = b"\
ostadix.information-provenance-analyzer/v2\0\
v1-atom-sidecar\0execution-derived-only\0strict-equivalent\0\
signed-receipt-admission-binding\0terminal-t2-output-binding\0\
contextual-recovery-with-residual-obligations";
const EXECUTION_DERIVATION_EQUIVALENCE_V2: &[u8] = b"\
ostadix.recovery-equivalence/execution-derivation-v2\0\
recover-acquisition-origin-relative-to-canonical-v1-atom-and-bound-world-receipt";

#[derive(Debug, Error)]
pub enum InformationProvenanceAdmissionErrorV2 {
    #[error(transparent)]
    Information(#[from] InformationErrorV1),
    #[error("information atom is not a legacy Derived atom")]
    LegacyModalityMismatch,
    #[error("execution provenance requires an execution runtime snapshot")]
    InspectionOnlyAdmission,
    #[error("execution admission does not contain plan node P{0}")]
    MissingPlanNode(usize),
    #[error("plan node P{0} uses autonomous unordered dispatch and cannot establish derivation provenance")]
    AutonomousPlanNode(usize),
    #[error("verified receipt does not bind the exact V6 admission digest")]
    ReceiptAdmissionMismatch,
    #[error("verified receipt did not terminate successfully")]
    ReceiptNotSuccessful,
    #[error("execution-derived provenance currently requires a T2 atom payload")]
    UnsupportedPayloadTier,
    #[error("verified receipt terminal value does not match the atom payload digest and length")]
    TerminalPayloadMismatch,
    #[error("verified receipt has no matching output object observation")]
    MissingOutputObservation,
    #[error("World receipt encoding failed while deriving provenance: {0}")]
    Receipt(String),
    #[error("World terminal value encoding failed while deriving provenance: {0}")]
    PortableValue(String),
    #[error("plan-node index does not fit the V2 provenance coordinate")]
    PlanNodeOverflow,
    #[error("verified provenance source belongs to another atom")]
    SourceAtomMismatch,
    #[error("asserted provenance record is not the trusted analyzer output")]
    AnalyzerImageMismatch,
}

/// Opaque, verified source for the first Information Provenance V2 analyzer.
///
/// It cannot be deserialized or freely constructed.  The sole constructor
/// consumes an opaque V6 admission and a signature-verified World receipt, then
/// checks their explicit digest binding and the terminal T2 output binding.
#[derive(Clone, Debug)]
pub struct VerifiedExecutionDerivationSourceV2 {
    atom_sha256: String,
    admission_sha256: String,
    evidence_analyzer_sha256: String,
    receipt_sha256: String,
    receipt_semantic_sha256: String,
    signer_key_id_sha256: String,
    plan_node: u64,
    source_sha256: String,
    procedure: NativeRecordRefV1,
    inputs: Vec<DerivationInputV2>,
}

impl VerifiedExecutionDerivationSourceV2 {
    pub fn atom_sha256(&self) -> &str {
        &self.atom_sha256
    }

    pub fn admission_sha256(&self) -> &str {
        &self.admission_sha256
    }

    pub fn receipt_sha256(&self) -> &str {
        &self.receipt_sha256
    }

    pub fn plan_node(&self) -> u64 {
        self.plan_node
    }

    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
}

/// Establish the bounded source that the current analyzer knows how to derive.
///
/// This does not claim universal execution fidelity.  It establishes four
/// narrower facts: the pre-execution graph was admitted, the receipt signature
/// passed the caller's resolver, the signed receipt names that exact admission
/// digest, and its terminal/output bytes match the atom's external T2 payload.
pub fn verify_execution_derivation_source_v2(
    atom: &InformationAtomV1,
    admitted: &AdmittedExecutionV6<'_>,
    receipt: &VerifiedExecutionReceiptV1,
    plan_node: PlanNodeId,
) -> Result<VerifiedExecutionDerivationSourceV2, InformationProvenanceAdmissionErrorV2> {
    atom.validate()?;
    if atom.modality() != AcquisitionModalityV1::Derived {
        return Err(InformationProvenanceAdmissionErrorV2::LegacyModalityMismatch);
    }
    if admitted.admission().runtime_snapshot_kind() != RuntimeSnapshotKindV1::Execution {
        return Err(InformationProvenanceAdmissionErrorV2::InspectionOnlyAdmission);
    }

    let operation = admitted
        .admission()
        .operations()
        .iter()
        .find(|operation| operation.plan_node == plan_node)
        .ok_or(InformationProvenanceAdmissionErrorV2::MissingPlanNode(
            plan_node.0,
        ))?;
    if operation.evidence.dispatch_contract.semantics != DispatchSemanticsV1::StrictEquivalent {
        return Err(InformationProvenanceAdmissionErrorV2::AutonomousPlanNode(
            plan_node.0,
        ));
    }

    let admission_sha256 = admitted.admission().admission_sha256().to_string();
    let receipt_record = receipt.receipt();
    if !receipt_record.evidence().is_some_and(|evidence| {
        evidence.gate() == EXECUTION_PROVENANCE_RECEIPT_GATE_V2
            && evidence.transcript().as_sha256() == admission_sha256
    }) {
        return Err(InformationProvenanceAdmissionErrorV2::ReceiptAdmissionMismatch);
    }

    let terminal = match receipt_record.terminal() {
        ReceiptTerminalV1::Success(value) => value,
        _ => return Err(InformationProvenanceAdmissionErrorV2::ReceiptNotSuccessful),
    };
    let terminal_bytes = terminal
        .encode()
        .map_err(|error| InformationProvenanceAdmissionErrorV2::PortableValue(error.to_string()))?;
    let terminal_sha256 = hex::encode(Sha256::digest(&terminal_bytes));
    let (payload_sha256, payload_len) = match atom.payload() {
        PayloadRefV1::T2(payload) => (&payload.sha256, payload.logical_len),
        PayloadRefV1::T0(_) | PayloadRefV1::T1(_) => {
            return Err(InformationProvenanceAdmissionErrorV2::UnsupportedPayloadTier)
        }
    };
    if &terminal_sha256 != payload_sha256
        || u64::try_from(terminal_bytes.len()).ok() != Some(payload_len)
    {
        return Err(InformationProvenanceAdmissionErrorV2::TerminalPayloadMismatch);
    }
    if !receipt_record.objects().iter().any(|object| {
        object.role() == ObjectRoleV1::Output
            && object.content().as_sha256() == payload_sha256
            && object.bytes_len() == payload_len
    }) {
        return Err(InformationProvenanceAdmissionErrorV2::MissingOutputObservation);
    }

    let signed_bytes = receipt.signed().bytes();
    let receipt_sha256 = hex::encode(
        receipt_v1_sha256(signed_bytes)
            .map_err(|error| InformationProvenanceAdmissionErrorV2::Receipt(error.to_string()))?,
    );
    let receipt_semantic_sha256 = hex::encode(
        project_receipt_semantic_sha256_v1(signed_bytes)
            .map_err(|error| InformationProvenanceAdmissionErrorV2::Receipt(error.to_string()))?,
    );
    let signer_key_id_sha256 = hex::encode(receipt.signed().signer_key_id());
    let plan_node_u64 = u64::try_from(plan_node.0)
        .map_err(|_| InformationProvenanceAdmissionErrorV2::PlanNodeOverflow)?;
    let atom_id = atom.id()?;
    let inputs = atom
        .support()
        .iter()
        .enumerate()
        .map(|(ordinal, atom)| {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| InformationProvenanceAdmissionErrorV2::PlanNodeOverflow)?;
            DerivationInputV2::new(ordinal, "support", atom.clone()).map_err(Into::into)
        })
        .collect::<Result<Vec<_>, InformationProvenanceAdmissionErrorV2>>()?;
    let procedure_descriptor = format!(
        "schema=ostadix.execution-plan-node/v2\nadmission_sha256={admission_sha256}\nplan_node={plan_node_u64}\n"
    );
    let procedure = native_record_ref_from_bytes(
        "ostadix.execution-plan-node/v2",
        "application/vnd.ostadix.execution-plan-node",
        procedure_descriptor.as_bytes(),
    )?;
    let evidence_analyzer_sha256 = admitted.admission().bindings().analyzer_sha256.clone();
    let source_sha256 = digest_fields(
        b"ostadix.information-provenance-source/v2\0",
        &[
            atom_id.as_sha256(),
            &admission_sha256,
            &evidence_analyzer_sha256,
            &receipt_sha256,
            &receipt_semantic_sha256,
            &signer_key_id_sha256,
            &plan_node_u64.to_string(),
            payload_sha256,
        ],
        atom.support().iter().map(AtomIdV1Ext::as_sha256),
    );

    Ok(VerifiedExecutionDerivationSourceV2 {
        atom_sha256: atom_id.as_sha256().to_string(),
        admission_sha256,
        evidence_analyzer_sha256,
        receipt_sha256,
        receipt_semantic_sha256,
        signer_key_id_sha256,
        plan_node: plan_node_u64,
        source_sha256,
        procedure,
        inputs,
    })
}

// A local trait avoids an unstable fully-qualified function pointer in the
// iterator above while keeping the digest surface explicit.
trait AtomIdV1Ext {
    fn as_sha256(&self) -> &str;
}

impl AtomIdV1Ext for crate::information::AtomIdV1 {
    fn as_sha256(&self) -> &str {
        crate::information::AtomIdV1::as_sha256(self)
    }
}

fn digest_fields<'a>(
    domain: &[u8],
    fixed: &[&str],
    trailing: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    for field in fixed {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    for field in trailing {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn analyzer_sha256_v2() -> String {
    hex::encode(Sha256::digest(INFORMATION_PROVENANCE_ANALYZER_SEMANTICS_V2))
}

fn native_record_ref_from_bytes(
    schema: &str,
    media_type: &str,
    bytes: &[u8],
) -> Result<NativeRecordRefV1, InformationErrorV1> {
    let logical_len = u64::try_from(bytes.len()).map_err(|_| {
        InformationErrorV1::InvalidRecord(
            "native record length does not fit the V1 coordinate".to_string(),
        )
    })?;
    NativeRecordRefV1::new(
        schema,
        media_type,
        hex::encode(Sha256::digest(bytes)),
        logical_len,
    )
}

/// The one recovery question currently implemented by the execution adapter.
///
/// The domain is the exact verified-source digest; the equivalence contract is
/// the analyzer's frozen semantic observer. Arbitrary caller-supplied bases are
/// returned as `Unsupported` rather than receiving the same assessment.
pub fn execution_derivation_recovery_question_v2(
    source: &VerifiedExecutionDerivationSourceV2,
) -> Result<RecoveryQuestionV1, InformationProvenanceAdmissionErrorV2> {
    let domain_descriptor = format!(
        "schema=ostadix.information-provenance-source/v2\nsource_sha256={}\n",
        source.source_sha256
    );
    Ok(RecoveryQuestionV1::new(
        RecoveryObjectV1::AcquisitionOrigin,
        native_record_ref_from_bytes(
            "ostadix.recovery-equivalence/v2",
            "application/vnd.ostadix.recovery-equivalence",
            EXECUTION_DERIVATION_EQUIVALENCE_V2,
        )?,
        native_record_ref_from_bytes(
            "ostadix.recovery-domain/v2",
            "application/vnd.ostadix.recovery-domain",
            domain_descriptor.as_bytes(),
        )?,
    )?)
}

fn legacy_support_loss() -> Result<LossContractV1, InformationErrorV1> {
    let kind = LossKindV1::new("derivation.v1-support-order-and-multiplicity-not-recorded")?;
    LossContractV1::new([kind.clone()], [kind], false)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InformationProvenanceAnalyzerV2;

impl RecoveryAnalyzerV1 for InformationProvenanceAnalyzerV2 {
    type Subject = InformationAtomV1;
    type Context = VerifiedExecutionDerivationSourceV2;
    type Error = InformationProvenanceAdmissionErrorV2;

    fn assess(
        &self,
        subject: &Self::Subject,
        question: &RecoveryQuestionV1,
        context: &Self::Context,
    ) -> Result<RecoveryAssessmentV2, Self::Error> {
        subject.validate()?;
        question.validate()?;
        if subject.id()?.as_sha256() != context.atom_sha256 {
            return Err(InformationProvenanceAdmissionErrorV2::SourceAtomMismatch);
        }
        if question != &execution_derivation_recovery_question_v2(context)? {
            return Ok(RecoveryAssessmentV2::unsupported(
                "execution-derivation analyzer does not implement this recovery basis",
            )?);
        }

        let outstanding = BTreeSet::from([
            RecoveryObligationV2::ProducerAuthentication,
            RecoveryObligationV2::ProcedureResolution {
                procedure_sha256: context.procedure.sha256.clone(),
            },
            RecoveryObligationV2::TypedInputCompleteness,
            RecoveryObligationV2::SupportRecovery,
            RecoveryObligationV2::ExecutionFidelity {
                admission_sha256: context.admission_sha256.clone(),
            },
            RecoveryObligationV2::PlanNodeReceiptBinding {
                plan_node: context.plan_node,
                receipt_sha256: context.receipt_sha256.clone(),
            },
            RecoveryObligationV2::ReceiptCurrentness {
                receipt_sha256: context.receipt_sha256.clone(),
            },
            RecoveryObligationV2::SignerAuthorization {
                signer_key_id_sha256: context.signer_key_id_sha256.clone(),
            },
            RecoveryObligationV2::DeterministicEffectClosure {
                plan_node: context.plan_node,
            },
            RecoveryObligationV2::MorphismFidelity {
                plan_node: context.plan_node,
            },
        ]);
        let discharges = [
            RecoveryDischargeV2::new(
                RecoveryObligationV2::ExecutionAdmissionBinding {
                    admission_sha256: context.admission_sha256.clone(),
                },
                context.admission_sha256.clone(),
            )?,
            RecoveryDischargeV2::new(
                RecoveryObligationV2::ReceiptSignatureVerification {
                    receipt_sha256: context.receipt_sha256.clone(),
                },
                context.receipt_sha256.clone(),
            )?,
            RecoveryDischargeV2::new(
                RecoveryObligationV2::TerminalOutputBinding {
                    receipt_sha256: context.receipt_sha256.clone(),
                },
                context.receipt_semantic_sha256.clone(),
            )?,
        ];
        Ok(RecoveryAssessmentV2::assessed(
            question.clone(),
            legacy_support_loss()?,
            outstanding,
            discharges,
        )?)
    }
}

impl InformationProvenanceAnalyzerV2 {
    pub fn analyze(
        &self,
        atom: &InformationAtomV1,
        source: &VerifiedExecutionDerivationSourceV2,
        question: &RecoveryQuestionV1,
    ) -> Result<InformationProvenanceV2, InformationProvenanceAdmissionErrorV2> {
        if atom.id()?.as_sha256() != source.atom_sha256 {
            return Err(InformationProvenanceAdmissionErrorV2::SourceAtomMismatch);
        }
        let claim = ProvenanceClaimV2::new(
            AcquisitionOriginWitnessV2::Derivation {
                procedure: source.procedure.clone(),
                inputs: source.inputs.clone(),
                input_binding: DerivationInputBindingV2::LegacySupportSet,
            },
            vec![
                AssuranceWitnessV2::PreExecutionAdmissionVerified {
                    admission_sha256: source.admission_sha256.clone(),
                    analyzer_sha256: source.evidence_analyzer_sha256.clone(),
                    plan_node: source.plan_node,
                },
                AssuranceWitnessV2::ReceiptSignatureVerified {
                    receipt_sha256: source.receipt_sha256.clone(),
                    signer_key_id_sha256: source.signer_key_id_sha256.clone(),
                },
            ],
        )?;
        debug_assert_eq!(claim.origin(), AcquisitionOriginV2::Derivation);
        let recovery = self.assess(atom, question, source)?;
        Ok(InformationProvenanceV2::new(
            atom.id()?,
            analyzer_sha256_v2(),
            source.source_sha256.clone(),
            claim,
            recovery,
        )?)
    }

    pub fn admit(
        &self,
        atom: &InformationAtomV1,
        source: &VerifiedExecutionDerivationSourceV2,
        question: &RecoveryQuestionV1,
        asserted: InformationProvenanceV2,
    ) -> Result<AdmittedInformationProvenanceV2, InformationProvenanceAdmissionErrorV2> {
        let expected = self.analyze(atom, source, question)?;
        if asserted != expected {
            return Err(InformationProvenanceAdmissionErrorV2::AnalyzerImageMismatch);
        }
        Ok(AdmittedInformationProvenanceV2 { record: expected })
    }
}

/// Provenance that is known to be in the current analyzer image.
///
/// No public constructor or deserializer exists.  Raw sidecars and V1 atoms
/// cannot be relabeled into this authority handle.
///
/// ```compile_fail
/// use ostadix_api::information_provenance::AdmittedInformationProvenanceV2;
///
/// let _: AdmittedInformationProvenanceV2 = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Clone, Debug)]
pub struct AdmittedInformationProvenanceV2 {
    record: InformationProvenanceV2,
}

impl AdmittedInformationProvenanceV2 {
    pub fn record(&self) -> &InformationProvenanceV2 {
        &self.record
    }

    pub fn atom(&self) -> &crate::information::AtomIdV1 {
        self.record.atom()
    }

    /// The origin class emitted by the analyzer.
    ///
    /// This is an image-membership statement, not an assertion that recovery
    /// has established the origin. Use [`Self::established_origin`] for that
    /// stronger question.
    pub fn analyzer_classification(&self) -> AcquisitionOriginV2 {
        self.record.claim().origin()
    }

    /// The origin only when the contextual recovery question is exact.
    pub fn established_origin(&self) -> Option<AcquisitionOriginV2> {
        (self.recovery_status() == RecoveryStatusV2::Exact).then(|| self.analyzer_classification())
    }

    pub fn recovery_status(&self) -> RecoveryStatusV2 {
        self.record.recovery().status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{
        admit_execution_v6, analyze_execution_v6, runtime_binding_from_directory,
    };
    use crate::execution_contract::Policy;
    use crate::hgraph::solve::solve_types;
    use crate::information::{
        EntityDescriptorV1, ExternalPayloadRefV1, ObservationIdV1, ParticipantV1, PublicScalarV1,
        ScopeV1,
    };
    use crate::ir::OIrProgram;
    use crate::parser::Parser;
    use crate::world::*;
    use std::collections::{BTreeMap, HashSet};
    use std::path::Path;

    struct ExactResolver {
        key_id: [u8; 32],
        public: [u8; 32],
    }

    impl ReceiptKeyResolver for ExactResolver {
        fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]> {
            (key_id == &self.key_id).then_some(self.public)
        }
    }

    fn artifact_sha(bytes: &[u8]) -> ArtifactId {
        ArtifactId::from_sha256(hex::encode(Sha256::digest(bytes))).unwrap()
    }

    fn admitted_fixture() -> (OIrProgram, crate::ir::ExecutionPlan, crate::hgraph::HGraph) {
        let backends = HashSet::from(["python".to_string()]);
        let parsed = Parser::new("python^(40 + 2)_python", &backends)
            .parse()
            .unwrap();
        let program = OIrProgram::lower(&parsed);
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).unwrap();
        solve_types(&mut graph).unwrap();
        (program, plan, graph)
    }

    fn receipt_for(
        admission_sha256: &str,
        terminal: PortableValueRecord,
    ) -> VerifiedExecutionReceiptV1 {
        let world_id = WorldId::new("provenance-world").unwrap();
        let world = WorldIdentity::new(world_id.clone(), WorldEpoch::new(1).unwrap());
        let governor = GovernorIdentity::new(
            world.clone(),
            GovernorTerm::new(1).unwrap(),
            GovernorLogIndex::new(1).unwrap(),
        );
        let node = NodeIdentity::new(
            world_id.clone(),
            NodeId::new("node").unwrap(),
            NodeGeneration::new(1).unwrap(),
        );
        let domain = DomainIdentity::new(
            node.clone(),
            DomainId::new("domain").unwrap(),
            DomainGeneration::new(1).unwrap(),
        );
        let process = ProcessIdentity::new(
            domain.clone(),
            ProcessId::new("process").unwrap(),
            ProcessGeneration::new(1).unwrap(),
        );
        let attempt = AttemptIdentity::new(
            world_id.clone(),
            TaskId::new("task").unwrap(),
            AttemptGeneration::new(1).unwrap(),
        );
        let output = ObjectIdentity::new(
            world_id.clone(),
            ObjectId::new("output").unwrap(),
            ObjectVersion::new(1).unwrap(),
        );
        let placement =
            ReceiptPlacementV1::new(node.clone(), domain.clone(), Some(process.clone()), vec![])
                .unwrap();
        let context = ReceiptContextV1::new(
            ReceiptIdentity::new(world_id, ReceiptId::new("receipt").unwrap()),
            world.clone(),
            governor.clone(),
            attempt.clone(),
            placement,
        )
        .unwrap();
        let terminal_bytes = terminal.encode().unwrap();
        let output_digest = artifact_sha(&terminal_bytes);
        let receipt = ExecutionReceiptV1::new(
            context,
            ReceiptSubjectV1::new(
                None,
                None,
                None,
                Some(artifact_sha(b"logical-graph")),
                Some(artifact_sha(b"effects")),
            )
            .unwrap(),
            vec![],
            vec![],
            vec![ObjectObservationV1::new(
                output.clone(),
                ObjectRoleV1::Output,
                output_digest,
                terminal_bytes.len() as u64,
            )
            .unwrap()],
            vec![],
            vec![],
            vec![],
            ReceiptTerminalV1::Success(terminal),
            ReceiptCommitFenceV1::Governed(governor.clone()),
            Some(
                EvidenceObservationV1::new(
                    EXECUTION_PROVENANCE_RECEIPT_GATE_V2,
                    ArtifactId::from_sha256(admission_sha256).unwrap(),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let current = ReceiptCurrentStateV1::new(
            world,
            governor,
            node,
            domain,
            Some(process),
            attempt,
            vec![output],
        )
        .unwrap();
        let signer = Ed25519ReceiptSigner::from_secret_bytes([0x51; 32]);
        let bytes = encode_signed_receipt_v1(&receipt, &current, &signer).unwrap();
        let resolver = ExactResolver {
            key_id: signer.key_id(),
            public: signer.public_key_bytes(),
        };
        verify_signed_receipt_v1(&bytes, &resolver).unwrap()
    }

    #[test]
    fn admitted_sidecar_is_exactly_the_execution_analyzer_image() {
        let (program, plan, graph) = admitted_fixture();
        let runtime = runtime_binding_from_directory(
            &plan,
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("backends")
                .as_path(),
            &[("provenance-test", "v2")],
        )
        .unwrap();
        let evidence = analyze_execution_v6(&program, &plan, &graph, runtime.clone()).unwrap();
        let admitted =
            admit_execution_v6(&program, &plan, graph, Policy::Eager, runtime, evidence).unwrap();
        let plan_node = admitted.admission().operations()[0].plan_node;
        let terminal = PortableValueRecord::Core(PortableOValue::integer(42).unwrap());
        let terminal_bytes = terminal.encode().unwrap();
        let producer = EntityDescriptorV1::new(
            "ostadix",
            "execution",
            BTreeMap::from([("id".to_string(), "fixture".to_string())]),
        )
        .unwrap()
        .id()
        .unwrap();
        let atom = InformationAtomV1::new(
            vec![ParticipantV1::new("subject", producer.clone()).unwrap()],
            "ostadix.execution-result/v1",
            PayloadRefV1::T2(
                ExternalPayloadRefV1::new(
                    "ostadix.execution-result/v1",
                    "application/octet-stream",
                    hex::encode(Sha256::digest(&terminal_bytes)),
                    terminal_bytes.len() as u64,
                )
                .unwrap(),
            ),
            AcquisitionModalityV1::Derived,
            ScopeV1::default(),
            producer.clone(),
            vec![],
        )
        .unwrap();
        let receipt = receipt_for(admitted.admission().admission_sha256(), terminal);
        let wrong_receipt = receipt_for(
            &"88".repeat(32),
            PortableValueRecord::Core(PortableOValue::integer(42).unwrap()),
        );
        assert!(matches!(
            verify_execution_derivation_source_v2(&atom, &admitted, &wrong_receipt, plan_node),
            Err(InformationProvenanceAdmissionErrorV2::ReceiptAdmissionMismatch)
        ));
        let inline_atom = InformationAtomV1::new(
            vec![ParticipantV1::new("subject", producer.clone()).unwrap()],
            "ostadix.execution-result/v1",
            PayloadRefV1::public(PublicScalarV1::U64(42)).unwrap(),
            AcquisitionModalityV1::Derived,
            ScopeV1::default(),
            producer,
            vec![],
        )
        .unwrap();
        assert!(matches!(
            verify_execution_derivation_source_v2(&inline_atom, &admitted, &receipt, plan_node),
            Err(InformationProvenanceAdmissionErrorV2::UnsupportedPayloadTier)
        ));
        let source =
            verify_execution_derivation_source_v2(&atom, &admitted, &receipt, plan_node).unwrap();
        let question = execution_derivation_recovery_question_v2(&source).unwrap();
        let analyzer = InformationProvenanceAnalyzerV2;
        let expected = analyzer.analyze(&atom, &source, &question).unwrap();
        let admitted_sidecar = analyzer
            .admit(&atom, &source, &question, expected.clone())
            .unwrap();
        assert_eq!(
            admitted_sidecar.analyzer_classification(),
            AcquisitionOriginV2::Derivation
        );
        assert_eq!(
            admitted_sidecar.recovery_status(),
            RecoveryStatusV2::Unestablished
        );
        assert_eq!(admitted_sidecar.established_origin(), None);
        let outstanding = expected.recovery().outstanding().unwrap();
        assert!(outstanding.contains(&RecoveryObligationV2::ProducerAuthentication));
        assert!(
            outstanding.contains(&RecoveryObligationV2::SignerAuthorization {
                signer_key_id_sha256: source.signer_key_id_sha256.clone(),
            })
        );
        assert!(
            outstanding.contains(&RecoveryObligationV2::ReceiptCurrentness {
                receipt_sha256: source.receipt_sha256.clone(),
            })
        );
        assert!(
            !outstanding.contains(&RecoveryObligationV2::ReceiptSignatureVerification {
                receipt_sha256: source.receipt_sha256.clone(),
            })
        );

        let procedure_descriptor = format!(
            "schema=ostadix.execution-plan-node/v2\nadmission_sha256={}\nplan_node={}\n",
            source.admission_sha256, source.plan_node
        );
        assert_eq!(
            source.procedure,
            native_record_ref_from_bytes(
                "ostadix.execution-plan-node/v2",
                "application/vnd.ostadix.execution-plan-node",
                procedure_descriptor.as_bytes(),
            )
            .unwrap()
        );
        let other_procedure_descriptor = format!(
            "schema=ostadix.execution-plan-node/v2\nadmission_sha256={}\nplan_node={}\n",
            source.admission_sha256,
            source.plan_node + 1
        );
        assert_ne!(
            source.procedure,
            native_record_ref_from_bytes(
                "ostadix.execution-plan-node/v2",
                "application/vnd.ostadix.execution-plan-node",
                other_procedure_descriptor.as_bytes(),
            )
            .unwrap()
        );

        let forged = InformationProvenanceV2::new(
            atom.id().unwrap(),
            expected.analyzer_sha256(),
            "99".repeat(32),
            expected.claim().clone(),
            expected.recovery().clone(),
        )
        .unwrap();
        assert!(matches!(
            analyzer.admit(&atom, &source, &question, forged),
            Err(InformationProvenanceAdmissionErrorV2::AnalyzerImageMismatch)
        ));

        let fabricated_measurement = InformationProvenanceV2::new(
            atom.id().unwrap(),
            expected.analyzer_sha256(),
            expected.source_sha256(),
            ProvenanceClaimV2::new(
                AcquisitionOriginWitnessV2::Measurement {
                    observation: ObservationIdV1::from_sha256("77".repeat(32)).unwrap(),
                },
                vec![],
            )
            .unwrap(),
            expected.recovery().clone(),
        )
        .unwrap();
        assert!(matches!(
            analyzer.admit(&atom, &source, &question, fabricated_measurement),
            Err(InformationProvenanceAdmissionErrorV2::AnalyzerImageMismatch)
        ));
    }
}
