use crate::effects::ResourceKey;
use crate::hgraph::AdmissionFactKind;
use crate::ir::PlanNodeId;

pub const EVIDENCE_SCHEMA_V2: &str = "oexec.evidence/v2";
pub const ADMISSION_SCHEMA_V2: &str = "oexec.admission/v2";
pub const ANALYZER_ID_V2: &str = "ostadix-oir-evidence-compiler/v2";

/// Strength and origin of a pre-execution fact. Declaration order is not used
/// as an authorization lattice; callers must use the explicit predicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceProvenance {
    Enforced,
    CompilerVerified,
    TrustedAdapter,
    SandboxObserved,
    UserDeclared,
    HistoricalObservation,
    Unknown,
}

impl EvidenceProvenance {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::CompilerVerified => "compiler-verified",
            Self::TrustedAdapter => "trusted-adapter",
            Self::SandboxObserved => "sandbox-observed",
            Self::UserDeclared => "user-declared",
            Self::HistoricalObservation => "historical-observation",
            Self::Unknown => "unknown",
        }
    }

    /// Only facts that are structurally enforced or derived by a trusted
    /// compiler/adapter may establish that an unknown effect is absent.
    pub const fn may_close_unknown_effect(self) -> bool {
        matches!(
            self,
            Self::Enforced | Self::CompilerVerified | Self::TrustedAdapter
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchLaneV1 {
    LocalWorker,
    Coordinator,
    Actor,
    ExternalProcess,
    AsyncIo,
    Gpu,
    RemoteProvider,
    Ocore,
}

impl DispatchLaneV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::LocalWorker => "local-worker",
            Self::Coordinator => "coordinator",
            Self::Actor => "actor",
            Self::ExternalProcess => "external-process",
            Self::AsyncIo => "async-io",
            Self::Gpu => "gpu",
            Self::RemoteProvider => "remote-provider",
            Self::Ocore => "ocore",
        }
    }
}

/// Stable preparation adapter selected by evidence analysis. The runtime may
/// validate the bound adapter against the admitted OIR, but it may not choose
/// a different adapter as a second scheduling authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchAdapterV1 {
    CoordinatorV1,
    OScopeLoadV1,
    TrustedInlineRendererV1,
}

impl DispatchAdapterV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::CoordinatorV1 => "coordinator/v1",
            Self::OScopeLoadV1 => "o-scope-load/v1",
            Self::TrustedInlineRendererV1 => "trusted-inline-renderer/v1",
        }
    }

    pub const fn is_local_worker(self) -> bool {
        !matches!(self, Self::CoordinatorV1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClassV1 {
    Infallible,
    MayFailNoExternalEffects,
    Transactional,
    Idempotent,
    Compensatable,
    Irreversible,
    Unknown,
}

impl FailureClassV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Infallible => "infallible",
            Self::MayFailNoExternalEffects => "may-fail-no-external-effects",
            Self::Transactional => "transactional",
            Self::Idempotent => "idempotent",
            Self::Compensatable => "compensatable",
            Self::Irreversible => "irreversible",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityDispositionV1 {
    NotRequired,
    PolicyPrevalidated,
    DeferredRuntimeCheck,
}

impl CapabilityDispositionV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotRequired => "not-required",
            Self::PolicyPrevalidated => "policy-prevalidated",
            Self::DeferredRuntimeCheck => "deferred-runtime-check",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementContractV1 {
    LocalCoordinator,
    LocalWorker,
}

impl PlacementContractV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::LocalCoordinator => "local-coordinator",
            Self::LocalWorker => "local-worker",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeContractV1 {
    /// The canonical graph reached a type/representation/fidelity fixed point.
    /// The resulting flags are bounds and may remain `ANY`; this is not a
    /// claim that every operation has one concrete inferred output type.
    pub constraints_solved: bool,
    pub output_domain_bits: u16,
    pub output_representation_bits: u16,
    pub output_fidelity: String,
    pub provenance: EvidenceProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectContractV1 {
    pub reads: Vec<ResourceKey>,
    pub writes: Vec<ResourceKey>,
    pub footprint_closed: bool,
    pub provenance: EvidenceProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchContractV1 {
    pub lane: DispatchLaneV1,
    pub adapter: DispatchAdapterV1,
    /// The adapter can build a Send-only envelope after all value inputs are
    /// materialized. This is availability of a preparation contract, not a
    /// claim that the operation has already been prepared.
    pub send_only_preparation: bool,
    pub provenance: EvidenceProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureContractV1 {
    pub class: FailureClassV1,
    pub cancellation_safe: bool,
    pub provenance: EvidenceProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDemandContractV1 {
    pub cpu_units: Option<u32>,
    pub hard_memory_bytes: Option<u64>,
    pub file_descriptors: Option<u32>,
    pub process_slots: Option<u32>,
    pub provenance: EvidenceProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CostEstimateV1 {
    pub expected_duration_micros: Option<u64>,
    pub confidence_parts_per_million: Option<u32>,
    pub provenance: EvidenceProvenance,
}

impl CostEstimateV1 {
    pub const fn unknown() -> Self {
        Self {
            expected_duration_micros: None,
            confidence_parts_per_million: None,
            provenance: EvidenceProvenance::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEvidence {
    pub plan_node: PlanNodeId,
    pub type_contract: TypeContractV1,
    pub effect_contract: EffectContractV1,
    pub dispatch_contract: DispatchContractV1,
    pub capability_disposition: CapabilityDispositionV1,
    pub capability_provenance: EvidenceProvenance,
    pub placement: PlacementContractV1,
    pub placement_provenance: EvidenceProvenance,
    pub failure_contract: FailureContractV1,
    pub resource_demand: ResourceDemandContractV1,
    /// Soft evidence. Admission never derives blockers from this field.
    pub cost_estimate: CostEstimateV1,
}

impl NodeEvidence {
    pub fn provenance_for(&self, fact: AdmissionFactKind) -> EvidenceProvenance {
        match fact {
            AdmissionFactKind::Type => self.type_contract.provenance,
            AdmissionFactKind::EffectFootprint => self.effect_contract.provenance,
            AdmissionFactKind::Dispatch => self.dispatch_contract.provenance,
            AdmissionFactKind::CapabilityPolicy => self.capability_provenance,
            AdmissionFactKind::Placement => self.placement_provenance,
            AdmissionFactKind::FailurePolicy => self.failure_contract.provenance,
            AdmissionFactKind::ResourceBudget => self.resource_demand.provenance,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeSnapshotKindV1 {
    Execution,
    Inspection,
}

impl RuntimeSnapshotKindV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Inspection => "inspection",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendArtifactStateV1 {
    Hashed { sha256: String },
    Missing,
    NonRegular,
    Unreadable { error_kind: String },
}

impl BackendArtifactStateV1 {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Hashed { .. } => "hashed",
            Self::Missing => "missing",
            Self::NonRegular => "non-regular",
            Self::Unreadable { .. } => "unreadable",
        }
    }

    pub fn sha256(&self) -> Option<&str> {
        match self {
            Self::Hashed { sha256 } => Some(sha256),
            Self::Missing | Self::NonRegular | Self::Unreadable { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendArtifactV1 {
    pub canonical_backend: String,
    pub resolved_identity: String,
    pub state: BackendArtifactStateV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeBindingV1 {
    pub(crate) snapshot_kind: RuntimeSnapshotKindV1,
    pub(crate) backend_artifacts: Vec<BackendArtifactV1>,
    pub(crate) backend_set_sha256: String,
    pub(crate) environment_sha256: String,
    /// Descriptive digest of the ambient HostWorld process snapshot. This is
    /// not a governed World identity, lease, capability, or authority grant.
    pub(crate) ambient_world_sha256: String,
}

impl RuntimeBindingV1 {
    pub fn snapshot_kind(&self) -> RuntimeSnapshotKindV1 {
        self.snapshot_kind
    }

    pub fn backend_artifacts(&self) -> &[BackendArtifactV1] {
        &self.backend_artifacts
    }

    pub fn backend_set_sha256(&self) -> &str {
        &self.backend_set_sha256
    }

    pub fn environment_sha256(&self) -> &str {
        &self.environment_sha256
    }

    pub fn ambient_world_sha256(&self) -> &str {
        &self.ambient_world_sha256
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceBindingsV1 {
    /// Digest of the canonical lowered OIR text. The original source bytes are
    /// intentionally not claimed here because evaluator entry points may be
    /// handed an already-lowered `OIrProgram`.
    pub oir_sha256: String,
    pub plan_sha256: String,
    pub analyzed_graph_sha256: String,
    pub backend_set_sha256: String,
    pub environment_sha256: String,
    pub ambient_world_sha256: String,
    pub analyzer_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceBundleV2 {
    pub(crate) schema: &'static str,
    pub(crate) analyzer: &'static str,
    pub(crate) bindings: EvidenceBindingsV1,
    pub(crate) runtime: RuntimeBindingV1,
    pub(crate) nodes: Vec<NodeEvidence>,
}

impl EvidenceBundleV2 {
    pub fn schema(&self) -> &'static str {
        self.schema
    }

    pub fn analyzer(&self) -> &'static str {
        self.analyzer
    }

    pub fn bindings(&self) -> &EvidenceBindingsV1 {
        &self.bindings
    }

    pub fn runtime(&self) -> &RuntimeBindingV1 {
        &self.runtime
    }

    pub fn nodes(&self) -> &[NodeEvidence] {
        &self.nodes
    }
}
