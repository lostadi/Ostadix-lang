//! Versioned, authority-free observations produced by the unified `o` intent
//! front door.
//!
//! These records describe what one local invocation observed.  They are not
//! admission evidence, signed receipts, or World authority.  In particular,
//! retaining a trace does not upgrade the guarantees of the engine that
//! produced it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::hosted_remote::{
    MeshExecutionTraceV1, MeshTraceEventV1, MESH_EXECUTION_TRACE_SCHEMA_V1,
};
use crate::project::model::OutputCapture;
use crate::project::{
    validated_selection_json_sha256, Artifact, ArtifactCaptureStatus, OExecutionResult,
    ProjectAttemptEvent, ProjectAttemptTrace, ProjectAttemptTraceHeader, ResultCodec,
    RouteExecutionDisposition, SelectionReuseContractV1, SelectionReuseOutputCheckV1,
    ValidatedArtifactCaptureStatusV1, ValidatedSelectionObservationV1, ValidatedSelectionReceiptV1,
};
#[cfg(test)]
use crate::project::{ValidatedSelectionCandidateV1, ValidatedSelectionDispositionV1};

pub const RUN_RECORD_SCHEMA_V1: &str = "ostadix.run-record/v1";
pub const RUN_SUMMARY_SCHEMA_V1: &str = "ostadix.run-summary/v1";
pub const PLACEMENT_PREVIEW_SCHEMA_V1: &str = "ostadix.placement-preview/v1";
pub const RUN_TRACE_ATTACHMENT_SCHEMA_V1: &str = "ostadix.run-trace/v1";
pub const RUN_RECORD_INTEGRITY_V1: &str = "unsigned_observation";
pub const PROJECT_SELECTION_REUSE_BINDING_SCHEMA_V1: &str =
    "ostadix.project-selection-reuse-binding/v1";
pub const PROJECT_SELECTION_REUSE_OBSERVATION_SCHEMA_V1: &str =
    "ostadix.project-selection-reuse-observation/v1";

fn validate_nonempty(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.contains('\0') {
        Err(format!("{label} must be nonempty and contain no NUL"))
    } else {
        Ok(())
    }
}

fn parse_canonical_duration(value: &str, label: &str) -> Result<u128, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{label} must be canonical unsigned decimal text"));
    }
    let duration = value
        .parse::<u128>()
        .map_err(|_| format!("{label} must be canonical unsigned decimal text"))?;
    if duration.to_string() != value {
        return Err(format!("{label} must be canonical unsigned decimal text"));
    }
    Ok(duration)
}

pub(crate) fn validate_lower_hex_64(value: &str, label: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!(
            "{label} must contain exactly 64 lowercase hexadecimal characters"
        ))
    }
}

/// Exact executable input kind selected during preflight.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunInputKindV1 {
    OrdinaryO,
    ProjectDirectory,
    LiftedProjectBundle,
}

/// Content-bound identity of the input.  The input bytes/tree are deliberately
/// not retained in the observation store.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunInputIdentityV1 {
    pub kind: RunInputKindV1,
    pub path: PathBuf,
    pub digest_sha256: String,
}

impl RunInputIdentityV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.path.as_os_str().is_empty() {
            return Err("input path must not be empty".to_string());
        }
        validate_lower_hex_64(&self.digest_sha256, "input digest")
    }
}

/// Effective, validated policy selected before a run identifier is allocated.
/// This stores explicit/effective values only; ambient environment variables,
/// credentials, and source material must never be copied here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionIntentObservationV1 {
    pub engine: String,
    pub target: Option<String>,
    pub selected_route: Option<String>,
    pub route_policy: Option<String>,
    pub route_declarations: Vec<String>,
    pub parallel_policy: String,
    /// Explicit local worker ceiling selected by `--workers`. `None` means
    /// the admitted runtime resolves capacity from the machine and graph.
    pub local_worker_limit: Option<u32>,
    pub mesh_mode: Option<String>,
    pub mesh_max_retries: Option<u32>,
    pub mesh_fallback: Option<String>,
    pub mesh_discovery_timeout_ms: Option<u64>,
    pub mesh_closed_registry: Option<bool>,
    pub mesh_peer_root: Option<PathBuf>,
    /// Exact durable evidence used to select one route without re-running the
    /// full benchmark set. The optional encoding preserves older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_reuse: Option<ProjectSelectionReuseBindingV1>,
}

impl ExecutionIntentObservationV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_nonempty(&self.engine, "execution engine")?;
        validate_nonempty(&self.parallel_policy, "parallel policy")?;
        for (label, value) in [
            ("target", self.target.as_deref()),
            ("selected route", self.selected_route.as_deref()),
            ("route policy", self.route_policy.as_deref()),
            ("mesh mode", self.mesh_mode.as_deref()),
            ("mesh fallback", self.mesh_fallback.as_deref()),
        ] {
            if let Some(value) = value {
                validate_nonempty(value, label)?;
            }
        }
        for declaration in &self.route_declarations {
            validate_nonempty(declaration, "route declaration")?;
        }
        if self.mesh_mode.is_none()
            && (self.mesh_max_retries.is_some()
                || self.mesh_fallback.is_some()
                || self.mesh_discovery_timeout_ms.is_some()
                || self.mesh_closed_registry.is_some()
                || self.mesh_peer_root.is_some())
        {
            return Err("mesh tuning was recorded without an effective mesh mode".to_string());
        }
        if self.mesh_discovery_timeout_ms == Some(0) {
            return Err("mesh discovery timeout must be positive".to_string());
        }
        if self.local_worker_limit == Some(0) {
            return Err("local worker limit must be positive".to_string());
        }
        if let Some(binding) = &self.selection_reuse {
            binding.validate()?;
            let expected_policy = format!("explicit:{}", binding.contract.selected_route_id);
            if self.engine != "project_compatibility"
                || self.target.as_deref() != Some(binding.contract.target.as_str())
                || self.selected_route.as_deref() != Some(binding.contract.target.as_str())
                || self.route_policy.as_deref() != Some(expected_policy.as_str())
                || self.route_declarations != binding.contract.route_declaration_sha256
                || self.mesh_mode.is_some()
            {
                return Err(
                    "selection-reuse binding disagrees with the effective project intent"
                        .to_string(),
                );
            }
        }
        Ok(())
    }
}

/// Applicable immutable planning identities.  An engine leaves identities
/// that it does not produce as `None` rather than inventing placeholders.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanIdentitiesV1 {
    pub oir_sha256: Option<String>,
    pub execution_plan_sha256: Option<String>,
    pub hgraph_sha256: Option<String>,
    pub execution_intent_sha256: Option<String>,
    pub deployment_sha256: Option<String>,
}

impl PlanIdentitiesV1 {
    pub fn validate(&self) -> Result<(), String> {
        for (label, digest) in [
            ("OIR identity", self.oir_sha256.as_deref()),
            (
                "execution-plan identity",
                self.execution_plan_sha256.as_deref(),
            ),
            ("HGraph identity", self.hgraph_sha256.as_deref()),
            (
                "execution-intent identity",
                self.execution_intent_sha256.as_deref(),
            ),
            ("deployment identity", self.deployment_sha256.as_deref()),
        ] {
            if let Some(digest) = digest {
                validate_lower_hex_64(digest, label)?;
            }
        }
        Ok(())
    }
}

/// Durable information written before computation begins.  It is sufficient
/// for a later writer to turn an abandoned lease into an interrupted record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunAttemptSeedV1 {
    pub input: RunInputIdentityV1,
    pub intent: ExecutionIntentObservationV1,
    pub plan: PlanIdentitiesV1,
    pub started_unix_nanos: u64,
}

impl RunAttemptSeedV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        self.intent.validate()?;
        self.plan.validate()?;
        if self.started_unix_nanos == 0 {
            return Err("run start observation must be nonzero".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunDispositionV1 {
    PreflightFailed,
    Succeeded,
    ExecutionFailed,
    InfrastructureFailed,
    Interrupted,
    RecordingIncomplete,
}

impl RunDispositionV1 {
    pub fn is_success(self) -> bool {
        self == Self::Succeeded
    }
}

/// Exact retained bytes plus the runtime's complete-stream observation.  No
/// additional recording-layer truncation is permitted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapturedStreamV1 {
    #[serde(with = "b64_bytes")]
    pub retained: Vec<u8>,
    pub capture: OutputCapture,
}

impl CapturedStreamV1 {
    pub fn complete(bytes: Vec<u8>) -> Self {
        let capture = OutputCapture::complete(&bytes);
        Self {
            retained: bytes,
            capture,
        }
    }

    pub fn from_runtime(retained: Vec<u8>, capture: OutputCapture) -> Result<Self, String> {
        capture.validate_for_retained(&retained)?;
        Ok(Self { retained, capture })
    }

    pub fn validate(&self) -> Result<(), String> {
        self.capture.validate_for_retained(&self.retained)
    }
}

impl Default for CapturedStreamV1 {
    fn default() -> Self {
        Self::complete(Vec::new())
    }
}

/// Lossless, canonical-CBOR-safe projection of `OExecutionResult`.
/// `duration_ns` is decimal text so the full `u128` range survives the
/// JSON-compatible canonical encoding layer.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecordedRouteResultV1 {
    pub route_id: String,
    /// The declared result codec is retained only when another durable datum
    /// (currently a validated-selection receipt) must be checked against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_codec: Option<ResultCodec>,
    pub exit_code: Option<i32>,
    pub stdout: CapturedStreamV1,
    pub stderr: CapturedStreamV1,
    pub value: Option<Value>,
    pub artifacts: Vec<Artifact>,
    pub artifact_requirements: Vec<String>,
    pub artifact_capture: ArtifactCaptureStatus,
    pub disposition: RouteExecutionDisposition,
    pub duration_ns: String,
    /// Complete-branch timing retained for validated selection. Legacy route
    /// results omit it, preserving their canonical encoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_elapsed_ns: Option<String>,
    pub provenance: RecordedExecutionProvenanceV1,
}

/// Credential-safe projection of process provenance. Raw argv, workspace, and
/// cwd values are deliberately excluded because foreign launch declarations
/// can embed credentials or ambient host paths. Engine/placement identities
/// and mesh traces retain the authority-free execution location evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordedExecutionProvenanceV1 {
    pub execution_scope: String,
    pub command_argv_retained: bool,
    pub command_argument_count: u64,
}

impl From<&OExecutionResult> for RecordedRouteResultV1 {
    fn from(result: &OExecutionResult) -> Self {
        Self {
            route_id: result.route_id.clone(),
            result_codec: None,
            exit_code: result.exit_code,
            stdout: CapturedStreamV1 {
                retained: result.stdout.clone(),
                capture: result.stdout_capture.clone(),
            },
            stderr: CapturedStreamV1 {
                retained: result.stderr.clone(),
                capture: result.stderr_capture.clone(),
            },
            value: result.value.clone(),
            artifacts: result.artifacts.clone(),
            artifact_requirements: result.artifact_requirements.clone(),
            artifact_capture: result.artifact_capture.clone(),
            disposition: result.disposition,
            duration_ns: result.duration_ns.to_string(),
            branch_elapsed_ns: None,
            provenance: RecordedExecutionProvenanceV1 {
                execution_scope: "isolated_project_workspace".to_string(),
                command_argv_retained: false,
                command_argument_count: u64::try_from(result.provenance.command.len())
                    .unwrap_or(u64::MAX),
            },
        }
    }
}

impl RecordedRouteResultV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_nonempty(&self.route_id, "route id")?;
        self.stdout.validate()?;
        self.stderr.validate()?;
        self.artifact_capture.validate()?;
        if self.provenance.execution_scope != "isolated_project_workspace"
            || self.provenance.command_argv_retained
        {
            return Err(
                "recorded route provenance retained an unsupported execution scope or raw argv"
                    .to_string(),
            );
        }
        if self.duration_ns.is_empty()
            || !self.duration_ns.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("route duration must be canonical unsigned decimal text".to_string());
        }
        let duration = self
            .duration_ns
            .parse::<u128>()
            .map_err(|_| "route duration must be canonical unsigned decimal text".to_string())?;
        if duration.to_string() != self.duration_ns {
            return Err("route duration must be canonical unsigned decimal text".to_string());
        }
        if let Some(branch_elapsed_ns) = &self.branch_elapsed_ns {
            let branch_duration =
                parse_canonical_duration(branch_elapsed_ns, "route complete-branch duration")?;
            if branch_duration < duration {
                return Err(
                    "route complete-branch duration is shorter than terminal duration".to_string(),
                );
            }
        }
        for artifact in &self.artifacts {
            validate_nonempty(&artifact.path, "artifact path")?;
            validate_lower_hex_64(&artifact.content_hash, "artifact digest")?;
        }
        for requirement in &self.artifact_requirements {
            validate_nonempty(requirement, "artifact requirement")?;
        }
        if self.exit_code == Some(0) && !self.artifact_capture.is_complete() {
            return Err("successful route has incomplete artifact evidence".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunFailureV1 {
    pub stage: String,
    pub message: String,
}

impl RunFailureV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_nonempty(&self.stage, "failure stage")?;
        validate_nonempty(&self.message, "failure message")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunContentKindV1 {
    Record,
    Trace,
}

impl RunContentKindV1 {
    pub(crate) fn domain(self) -> &'static [u8] {
        match self {
            Self::Record => b"ostadix.run-record/v1",
            Self::Trace => b"ostadix.run-trace/v1",
        }
    }

    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::Record => "records",
            Self::Trace => "traces",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RunContentRefV1 {
    pub kind: RunContentKindV1,
    pub sha256: String,
    pub bytes_len: u64,
}

impl RunContentRefV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_lower_hex_64(&self.sha256, "run object digest")?;
        if self.bytes_len == 0 {
            return Err("run object length must be positive".to_string());
        }
        Ok(())
    }
}

/// Pre-execution evidence binding for one bundle-bound selected-route reuse.
///
/// The source record is named by its verified content-addressed object. The
/// embedded receipt keeps a later record intelligible if retention prunes the
/// source attempt, but remains an unsigned observation rather than authority
/// on its own.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSelectionReuseBindingV1 {
    pub schema: String,
    pub source_run_id: String,
    pub source_sequence: u64,
    pub source_record: RunContentRefV1,
    pub receipt_sha256: String,
    pub contract_sha256: String,
    pub receipt: ValidatedSelectionReceiptV1,
    pub contract: SelectionReuseContractV1,
}

impl ProjectSelectionReuseBindingV1 {
    pub fn new(
        source_run_id: impl Into<String>,
        source_sequence: u64,
        source_record: RunContentRefV1,
        receipt: ValidatedSelectionReceiptV1,
        contract: SelectionReuseContractV1,
    ) -> Result<Self, String> {
        let receipt_sha256 = receipt.sha256()?;
        let contract_sha256 = contract.sha256()?;
        let binding = Self {
            schema: PROJECT_SELECTION_REUSE_BINDING_SCHEMA_V1.to_string(),
            source_run_id: source_run_id.into(),
            source_sequence,
            source_record,
            receipt_sha256,
            contract_sha256,
            receipt,
            contract,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PROJECT_SELECTION_REUSE_BINDING_SCHEMA_V1 {
            return Err("project selection-reuse binding has an unsupported schema".to_string());
        }
        validate_lower_hex_64(&self.source_run_id, "selection source run id")?;
        if self.source_sequence == 0 {
            return Err("selection source sequence must be positive".to_string());
        }
        self.source_record.validate()?;
        if self.source_record.kind != RunContentKindV1::Record {
            return Err("selection source object is not a run record".to_string());
        }
        validate_lower_hex_64(&self.receipt_sha256, "selection receipt digest")?;
        validate_lower_hex_64(&self.contract_sha256, "selection contract digest")?;
        self.receipt.validate()?;
        self.contract.validate()?;
        if self.receipt.sha256()? != self.receipt_sha256
            || self.contract.sha256()? != self.contract_sha256
        {
            return Err(
                "selection-reuse receipt or contract digest disagrees with its evidence"
                    .to_string(),
            );
        }
        let receipt_routes = self
            .receipt
            .candidates
            .iter()
            .map(|candidate| candidate.route_id.as_str())
            .collect::<Vec<_>>();
        let contract_routes = self
            .contract
            .ordered_alternatives
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let selected = self
            .receipt
            .candidates
            .iter()
            .find(|candidate| candidate.route_id == self.receipt.selected_route_id)
            .ok_or_else(|| "selection receipt lost its selected candidate".to_string())?;
        if self.receipt.project_name != self.contract.project_name
            || self.receipt.bundle_sha256 != self.contract.bundle_sha256
            || self.receipt.target != self.contract.target
            || self.receipt.policy != self.contract.evidence_policy
            || self.receipt.equivalence_contract != self.contract.equivalence_contract
            || self.receipt.selection_rule != self.contract.selection_rule
            || self.receipt.reference_route_id != self.contract.reference_route_id
            || self.receipt.selected_route_id != self.contract.selected_route_id
            || selected.declared_output_sha256 != self.contract.expected_declared_output_sha256
            || receipt_routes != contract_routes
        {
            return Err("selection receipt and reuse contract disagree".to_string());
        }
        Ok(())
    }
}

/// Terminal postcondition attached to a run that reused a selected route.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSelectionReuseObservationV1 {
    pub schema: String,
    pub source_run_id: String,
    pub source_record_sha256: String,
    pub receipt_sha256: String,
    pub contract_sha256: String,
    pub selected_route_id: String,
    pub output_check: SelectionReuseOutputCheckV1,
}

impl ProjectSelectionReuseObservationV1 {
    pub fn from_binding(
        binding: &ProjectSelectionReuseBindingV1,
        output_check: SelectionReuseOutputCheckV1,
    ) -> Result<Self, String> {
        let observation = Self {
            schema: PROJECT_SELECTION_REUSE_OBSERVATION_SCHEMA_V1.to_string(),
            source_run_id: binding.source_run_id.clone(),
            source_record_sha256: binding.source_record.sha256.clone(),
            receipt_sha256: binding.receipt_sha256.clone(),
            contract_sha256: binding.contract_sha256.clone(),
            selected_route_id: binding.contract.selected_route_id.clone(),
            output_check,
        };
        observation.validate_for(binding)?;
        Ok(observation)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PROJECT_SELECTION_REUSE_OBSERVATION_SCHEMA_V1 {
            return Err(
                "project selection-reuse observation has an unsupported schema".to_string(),
            );
        }
        validate_lower_hex_64(&self.source_run_id, "selection source run id")?;
        validate_lower_hex_64(&self.source_record_sha256, "selection source record digest")?;
        validate_lower_hex_64(&self.receipt_sha256, "selection receipt digest")?;
        validate_lower_hex_64(&self.contract_sha256, "selection contract digest")?;
        validate_nonempty(&self.selected_route_id, "selected route id")?;
        self.output_check.validate()
    }

    pub fn validate_for(&self, binding: &ProjectSelectionReuseBindingV1) -> Result<(), String> {
        self.validate()?;
        binding.validate()?;
        if self.source_run_id != binding.source_run_id
            || self.source_record_sha256 != binding.source_record.sha256
            || self.receipt_sha256 != binding.receipt_sha256
            || self.contract_sha256 != binding.contract_sha256
            || self.selected_route_id != binding.contract.selected_route_id
            || self.output_check.expected_declared_output_sha256
                != binding.contract.expected_declared_output_sha256
        {
            return Err(
                "selection-reuse observation disagrees with its admitted binding".to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum RunTraceBindingV1 {
    Attached { object: RunContentRefV1 },
    Unavailable { reason: String },
}

impl RunTraceBindingV1 {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Attached { object } => {
                object.validate()?;
                if object.kind != RunContentKindV1::Trace {
                    return Err("trace binding does not reference a trace object".to_string());
                }
                Ok(())
            }
            Self::Unavailable { reason } => validate_nonempty(reason, "trace-unavailable reason"),
        }
    }
}

/// Serializable projection of the ordinary evaluator's node lifecycle trace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OrdinaryExecutionTraceV1 {
    pub schema: String,
    pub input_sha256: String,
    pub oir_sha256: String,
    pub execution_plan_sha256: String,
    pub hgraph_sha256: String,
    pub execution_intent_sha256: String,
    pub events: Vec<OrdinaryTraceEventV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OrdinaryTraceEventV1 {
    NodeReady {
        node: u64,
    },
    NodeStarted {
        node: u64,
    },
    NodeFinished {
        node: u64,
        value_type: String,
        fingerprint: Option<String>,
    },
    NodeFailed {
        node: u64,
        message: String,
    },
    NodeDiscarded {
        node: u64,
        reason: String,
    },
}

impl OrdinaryExecutionTraceV1 {
    pub const SCHEMA: &'static str = "ostadix.ordinary-execution-trace/v1";

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != Self::SCHEMA {
            return Err(format!(
                "unsupported ordinary trace schema `{}`",
                self.schema
            ));
        }
        if self.events.len() > 1_000_000 {
            return Err("ordinary trace exceeds the 1000000-event validation bound".to_string());
        }
        for (label, digest) in [
            ("ordinary trace input", self.input_sha256.as_str()),
            ("ordinary trace OIR", self.oir_sha256.as_str()),
            (
                "ordinary trace execution plan",
                self.execution_plan_sha256.as_str(),
            ),
            ("ordinary trace HGraph", self.hgraph_sha256.as_str()),
            (
                "ordinary trace execution intent",
                self.execution_intent_sha256.as_str(),
            ),
        ] {
            validate_lower_hex_64(digest, label)?;
        }
        #[derive(Clone, Copy, Eq, PartialEq)]
        enum State {
            Ready,
            Started,
            Terminal,
        }
        let mut states = BTreeMap::new();
        for event in &self.events {
            let (node, next) = match event {
                OrdinaryTraceEventV1::NodeFinished {
                    node,
                    value_type,
                    fingerprint,
                } => {
                    validate_nonempty(value_type, "ordinary trace value type")?;
                    if let Some(fingerprint) = fingerprint {
                        validate_nonempty(fingerprint, "ordinary trace fingerprint")?;
                    }
                    (*node, State::Terminal)
                }
                OrdinaryTraceEventV1::NodeFailed { node, message } => {
                    validate_nonempty(message, "ordinary trace failure")?;
                    (*node, State::Terminal)
                }
                OrdinaryTraceEventV1::NodeDiscarded { node, reason } => {
                    validate_nonempty(reason, "ordinary trace discard reason")?;
                    (*node, State::Terminal)
                }
                OrdinaryTraceEventV1::NodeReady { node } => (*node, State::Ready),
                OrdinaryTraceEventV1::NodeStarted { node } => (*node, State::Started),
            };
            if !matches!(
                (states.get(&node).copied(), next),
                (None, State::Ready)
                    | (Some(State::Ready), State::Started)
                    | (Some(State::Started), State::Terminal)
            ) {
                return Err(format!(
                    "ordinary trace has an invalid lifecycle transition for node P{node}"
                ));
            }
            states.insert(node, next);
        }
        Ok(())
    }
}

/// Stable projection of a checked Project HGraph trace.  Deserialization is
/// followed by structural replay so skipped internal validation indexes are
/// never trusted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordedProjectTraceV1 {
    pub header: ProjectAttemptTraceHeader,
    pub events: Vec<ProjectAttemptEvent>,
}

impl From<&ProjectAttemptTrace> for RecordedProjectTraceV1 {
    fn from(trace: &ProjectAttemptTrace) -> Self {
        Self {
            header: trace.header().clone(),
            events: trace.events().to_vec(),
        }
    }
}

impl RecordedProjectTraceV1 {
    pub fn validate(&self) -> Result<(), String> {
        ProjectAttemptTrace::try_from_events(self.header.clone(), self.events.clone())
            .map(|_| ())
            .map_err(|error| format!("invalid Project HGraph trace: {error}"))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "engine", content = "trace", rename_all = "snake_case")]
pub enum RunTracePayloadV1 {
    Ordinary(OrdinaryExecutionTraceV1),
    ProjectHgraph(RecordedProjectTraceV1),
    ProjectMesh(MeshExecutionTraceV1),
}

/// One separately content-addressed trace attachment.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunTraceAttachmentV1 {
    pub schema: String,
    pub payload: RunTracePayloadV1,
}

impl RunTraceAttachmentV1 {
    pub fn ordinary(trace: OrdinaryExecutionTraceV1) -> Self {
        Self {
            schema: RUN_TRACE_ATTACHMENT_SCHEMA_V1.to_string(),
            payload: RunTracePayloadV1::Ordinary(trace),
        }
    }

    pub fn project_hgraph(trace: &ProjectAttemptTrace) -> Self {
        Self {
            schema: RUN_TRACE_ATTACHMENT_SCHEMA_V1.to_string(),
            payload: RunTracePayloadV1::ProjectHgraph(trace.into()),
        }
    }

    pub fn project_mesh(trace: MeshExecutionTraceV1) -> Self {
        Self {
            schema: RUN_TRACE_ATTACHMENT_SCHEMA_V1.to_string(),
            payload: RunTracePayloadV1::ProjectMesh(trace),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RUN_TRACE_ATTACHMENT_SCHEMA_V1 {
            return Err(format!("unsupported run trace schema `{}`", self.schema));
        }
        match &self.payload {
            RunTracePayloadV1::Ordinary(trace) => trace.validate(),
            RunTracePayloadV1::ProjectHgraph(trace) => trace.validate(),
            RunTracePayloadV1::ProjectMesh(trace) => validate_mesh_trace(trace),
        }
    }

    /// Validate not just the attachment shape but its semantic binding to the
    /// terminal observation that names it.
    pub fn validate_for_record(&self, record: &RunRecordV1) -> Result<(), String> {
        self.validate()?;
        let RunTraceBindingV1::Attached { object } = &record.trace else {
            return Err("run trace attachment is not named by its terminal record".to_string());
        };
        let bytes = crate::canonical_cbor::encode(self)
            .map_err(|error| format!("failed to canonically encode run trace: {error:#}"))?;
        let mut hash = Sha256::new();
        let domain = RunContentKindV1::Trace.domain();
        hash.update(b"ostadix.run-object-domain/v1\0");
        hash.update((domain.len() as u64).to_be_bytes());
        hash.update(domain);
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(&bytes);
        let actual = RunContentRefV1 {
            kind: RunContentKindV1::Trace,
            sha256: hex::encode(hash.finalize()),
            bytes_len: u64::try_from(bytes.len())
                .map_err(|_| "run trace byte length does not fit u64".to_string())?,
        };
        if &actual != object {
            return Err(
                "run trace attachment bytes disagree with the record reference".to_string(),
            );
        }
        match &self.payload {
            RunTracePayloadV1::Ordinary(trace) => {
                if record.input.kind != RunInputKindV1::OrdinaryO
                    || !record.intent.engine.starts_with("local_")
                    || record.intent.mesh_mode.is_some()
                    || trace.input_sha256 != record.input.digest_sha256
                    || Some(trace.oir_sha256.as_str()) != record.plan.oir_sha256.as_deref()
                    || Some(trace.execution_plan_sha256.as_str())
                        != record.plan.execution_plan_sha256.as_deref()
                    || Some(trace.hgraph_sha256.as_str()) != record.plan.hgraph_sha256.as_deref()
                    || Some(trace.execution_intent_sha256.as_str())
                        != record.plan.execution_intent_sha256.as_deref()
                {
                    return Err(
                        "ordinary evaluator trace disagrees with the recorded input or engine"
                            .to_string(),
                    );
                }
            }
            RunTracePayloadV1::ProjectHgraph(trace) => {
                if record.input.kind == RunInputKindV1::OrdinaryO
                    || record.intent.engine != "project_hgraph"
                    || trace.header.bundle_digest != record.input.digest_sha256
                    || Some(trace.header.target.as_str()) != record.intent.target.as_deref()
                    || Some(trace.header.policy.as_str()) != record.intent.route_policy.as_deref()
                    || Some(trace.header.logical_graph_digest.as_str())
                        != record.plan.hgraph_sha256.as_deref()
                    || Some(trace.header.deployment_plan_digest.as_str())
                        != record.plan.deployment_sha256.as_deref()
                {
                    return Err(
                        "Project HGraph trace identities disagree with the terminal record"
                            .to_string(),
                    );
                }
            }
            RunTracePayloadV1::ProjectMesh(trace) => {
                if record.input.kind == RunInputKindV1::OrdinaryO
                    || !record.intent.engine.starts_with("project_mesh_")
                    || trace.bundle_sha256 != record.input.digest_sha256
                    || Some(trace.target.as_str()) != record.intent.target.as_deref()
                    || Some(trace.policy.as_str()) != record.intent.route_policy.as_deref()
                {
                    return Err(
                        "project mesh trace identities disagree with the terminal record"
                            .to_string(),
                    );
                }
                validate_mesh_trace_policy_binding(trace, &record.intent)?;
            }
        }
        Ok(())
    }
}

fn validate_mesh_trace_policy_binding(
    trace: &MeshExecutionTraceV1,
    intent: &ExecutionIntentObservationV1,
) -> Result<(), String> {
    let mode = intent
        .mesh_mode
        .as_deref()
        .ok_or_else(|| "project mesh trace is missing its recorded mesh mode".to_string())?;
    if !matches!(mode, "prefer" | "required") {
        return Err(format!("unsupported recorded mesh mode `{mode}`"));
    }
    let fallback = intent
        .mesh_fallback
        .as_deref()
        .ok_or_else(|| "project mesh trace is missing its recorded fallback policy".to_string())?;
    if !matches!(fallback, "pre_send" | "idempotent" | "never") {
        return Err(format!(
            "unsupported recorded mesh fallback policy `{fallback}`"
        ));
    }
    let maximum_generation = intent
        .mesh_max_retries
        .ok_or_else(|| "project mesh trace is missing its recorded retry bound".to_string())?
        .checked_add(1)
        .ok_or_else(|| "recorded mesh retry bound overflowed".to_string())?;
    let mut executed_failures = BTreeSet::new();
    let mut ambiguous_failures = BTreeSet::new();

    for event in &trace.events {
        let generation = match event {
            MeshTraceEventV1::Dispatched { generation, .. }
            | MeshTraceEventV1::Settled { generation, .. }
            | MeshTraceEventV1::AttemptFailed { generation, .. }
            | MeshTraceEventV1::RetryDenied { generation, .. } => *generation,
            MeshTraceEventV1::Migrated { to_generation, .. } => *to_generation,
            MeshTraceEventV1::LocalFallback {
                after_generation, ..
            } => *after_generation,
        };
        if generation > maximum_generation {
            return Err(format!(
                "mesh trace generation {generation} exceeds recorded attempt bound {maximum_generation}"
            ));
        }

        match event {
            MeshTraceEventV1::Settled {
                actor_id,
                succeeded: false,
                ..
            } => {
                executed_failures.insert(actor_id.as_str());
            }
            MeshTraceEventV1::AttemptFailed {
                actor_id, delivery, ..
            } => match delivery.as_str() {
                "ambiguous" => {
                    ambiguous_failures.insert(actor_id.as_str());
                }
                "executed" => {
                    executed_failures.insert(actor_id.as_str());
                }
                _ => {}
            },
            MeshTraceEventV1::LocalFallback {
                actor_id,
                replay_contract,
                ..
            } => {
                if !matches!(replay_contract.as_str(), "unproven" | "declared_idempotent") {
                    return Err(format!(
                        "unsupported mesh fallback replay contract `{replay_contract}`"
                    ));
                }
                if mode == "required" || fallback == "never" {
                    return Err(
                        "mesh trace claims local fallback under a policy that forbids it"
                            .to_string(),
                    );
                }
                if ambiguous_failures.contains(actor_id.as_str()) {
                    return Err(
                        "mesh trace claims local fallback after ambiguous remote delivery"
                            .to_string(),
                    );
                }
                if executed_failures.contains(actor_id.as_str())
                    && (fallback != "idempotent" || replay_contract != "declared_idempotent")
                {
                    return Err(
                        "mesh trace fallback after execution lacks recorded idempotent continuation authority"
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_mesh_trace(trace: &MeshExecutionTraceV1) -> Result<(), String> {
    trace
        .validate()
        .map_err(|error| format!("invalid project mesh trace: {error:#}"))?;
    if trace.schema != MESH_EXECUTION_TRACE_SCHEMA_V1 {
        return Err(format!(
            "unsupported project mesh trace schema `{}`",
            trace.schema
        ));
    }
    validate_nonempty(&trace.execution_id, "mesh execution id")?;
    validate_lower_hex_64(&trace.bundle_sha256, "mesh bundle digest")?;
    validate_nonempty(&trace.target, "mesh target")?;
    validate_nonempty(&trace.policy, "mesh route policy")?;
    let mut candidates = BTreeSet::new();
    for candidate in &trace.candidates {
        validate_nonempty(&candidate.node_id, "mesh candidate node id")?;
        if !candidates.insert(candidate.node_id.as_str()) {
            return Err(format!(
                "mesh trace repeats candidate node `{}`",
                candidate.node_id
            ));
        }
        if let Some(address) = &candidate.address {
            validate_nonempty(address, "mesh candidate address")?;
        }
        validate_nonempty(&candidate.detail, "mesh candidate detail")?;
    }
    for event in &trace.events {
        match event {
            MeshTraceEventV1::Dispatched {
                route_id,
                actor_id,
                node_id,
                ..
            }
            | MeshTraceEventV1::Settled {
                route_id,
                actor_id,
                node_id,
                ..
            } => {
                validate_nonempty(route_id, "mesh route id")?;
                validate_nonempty(actor_id, "mesh actor id")?;
                validate_nonempty(node_id, "mesh node id")?;
            }
            MeshTraceEventV1::AttemptFailed {
                route_id,
                actor_id,
                node_id,
                delivery,
                replay_contract,
                reason,
                ..
            } => {
                validate_nonempty(route_id, "mesh route id")?;
                validate_nonempty(actor_id, "mesh actor id")?;
                validate_nonempty(node_id, "mesh node id")?;
                validate_nonempty(delivery, "mesh failed-attempt delivery")?;
                validate_nonempty(replay_contract, "mesh failed-attempt replay contract")?;
                validate_nonempty(reason, "mesh failed-attempt reason")?;
            }
            MeshTraceEventV1::Migrated {
                route_id,
                actor_id,
                from_generation,
                to_generation,
                from_node_id,
                to_node_id,
                replay_contract,
            } => {
                validate_nonempty(route_id, "mesh route id")?;
                validate_nonempty(actor_id, "mesh actor id")?;
                validate_nonempty(from_node_id, "mesh source node id")?;
                validate_nonempty(to_node_id, "mesh destination node id")?;
                validate_nonempty(replay_contract, "mesh replay contract")?;
                if to_generation <= from_generation {
                    return Err("mesh migration generation did not advance".to_string());
                }
            }
            MeshTraceEventV1::RetryDenied {
                route_id,
                actor_id,
                reason,
                ..
            } => {
                validate_nonempty(route_id, "mesh route id")?;
                validate_nonempty(actor_id, "mesh actor id")?;
                validate_nonempty(reason, "mesh decision reason")?;
            }
            MeshTraceEventV1::LocalFallback {
                route_id,
                actor_id,
                replay_contract,
                reason,
                ..
            } => {
                validate_nonempty(route_id, "mesh route id")?;
                validate_nonempty(actor_id, "mesh actor id")?;
                validate_nonempty(replay_contract, "mesh fallback replay contract")?;
                validate_nonempty(reason, "mesh decision reason")?;
            }
        }
    }
    Ok(())
}

/// Immutable terminal observation.  The only mutable store entry is the small
/// attempt index that points at this content-addressed record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunRecordV1 {
    pub schema: String,
    pub run_id: String,
    pub sequence: u64,
    pub input: RunInputIdentityV1,
    pub intent: ExecutionIntentObservationV1,
    pub plan: PlanIdentitiesV1,
    pub started_unix_nanos: u64,
    pub finished_unix_nanos: u64,
    pub elapsed_nanos: u64,
    pub disposition: RunDispositionV1,
    pub stdout: CapturedStreamV1,
    pub stderr: CapturedStreamV1,
    pub decoded_value: Option<Value>,
    pub route_results: Vec<RecordedRouteResultV1>,
    /// Structured evidence for the new validated benchmark selector. The
    /// optional encoding preserves canonical bytes for records written before
    /// this policy existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validated_selection_receipt: Option<ValidatedSelectionReceiptV1>,
    /// Terminal declared-output check for a route chosen from a prior
    /// validated-selection run. Fresh benchmark evidence and reused evidence
    /// are intentionally separate fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_reuse: Option<ProjectSelectionReuseObservationV1>,
    pub result_references: Vec<RunResultReferenceV1>,
    pub trace: RunTraceBindingV1,
    pub failure: Option<RunFailureV1>,
    pub integrity: String,
}

impl RunRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn terminal(
        run_id: impl Into<String>,
        sequence: u64,
        seed: &RunAttemptSeedV1,
        finished_unix_nanos: u64,
        elapsed_nanos: u64,
        disposition: RunDispositionV1,
        stdout: CapturedStreamV1,
        stderr: CapturedStreamV1,
        decoded_value: Option<Value>,
        route_results: Vec<RecordedRouteResultV1>,
        result_references: Vec<RunResultReferenceV1>,
        trace: RunTraceBindingV1,
        failure: Option<RunFailureV1>,
    ) -> Self {
        Self {
            schema: RUN_RECORD_SCHEMA_V1.to_string(),
            run_id: run_id.into(),
            sequence,
            input: seed.input.clone(),
            intent: seed.intent.clone(),
            plan: seed.plan.clone(),
            started_unix_nanos: seed.started_unix_nanos,
            finished_unix_nanos,
            elapsed_nanos,
            disposition,
            stdout,
            stderr,
            decoded_value,
            route_results,
            validated_selection_receipt: None,
            selection_reuse: None,
            result_references,
            trace,
            failure,
            integrity: RUN_RECORD_INTEGRITY_V1.to_string(),
        }
    }

    pub(crate) fn interrupted(
        run_id: impl Into<String>,
        sequence: u64,
        seed: &RunAttemptSeedV1,
        finished_unix_nanos: u64,
    ) -> Self {
        Self::terminal(
            run_id,
            sequence,
            seed,
            finished_unix_nanos,
            0,
            RunDispositionV1::Interrupted,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            None,
            Vec::new(),
            Vec::new(),
            RunTraceBindingV1::unavailable(
                "the process lease was released before terminal finalization",
            ),
            Some(RunFailureV1 {
                stage: "interrupted".to_string(),
                message: "the executing process ended without publishing a terminal observation"
                    .to_string(),
            }),
        )
    }

    pub(crate) fn recording_incomplete(
        run_id: impl Into<String>,
        sequence: u64,
        seed: &RunAttemptSeedV1,
        finished_unix_nanos: u64,
        detail: impl Into<String>,
    ) -> Self {
        let detail = detail.into();
        Self::terminal(
            run_id,
            sequence,
            seed,
            finished_unix_nanos,
            0,
            RunDispositionV1::RecordingIncomplete,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            None,
            Vec::new(),
            Vec::new(),
            RunTraceBindingV1::unavailable("terminal execution evidence could not be retained"),
            Some(RunFailureV1 {
                stage: "recording".to_string(),
                message: detail,
            }),
        )
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RUN_RECORD_SCHEMA_V1 {
            return Err(format!("unsupported run-record schema `{}`", self.schema));
        }
        validate_lower_hex_64(&self.run_id, "run id")?;
        if self.sequence == 0 {
            return Err("run sequence must be positive".to_string());
        }
        if self.disposition == RunDispositionV1::PreflightFailed {
            return Err(
                "preflight failure cannot be represented by a durable run record".to_string(),
            );
        }
        self.input.validate()?;
        self.intent.validate()?;
        self.plan.validate()?;
        if self.started_unix_nanos == 0 || self.finished_unix_nanos == 0 {
            return Err("run start and finish observations must be nonzero".to_string());
        }
        if self.finished_unix_nanos < self.started_unix_nanos {
            return Err("run finish observation precedes its start".to_string());
        }
        match self.input.kind {
            RunInputKindV1::OrdinaryO => {
                if !self.intent.engine.starts_with("local_")
                    || self.intent.target.as_deref() != Some("local")
                    || self.intent.selected_route.is_some()
                    || self.intent.route_policy.is_some()
                    || self.intent.mesh_mode.is_some()
                    || self.plan.oir_sha256.is_none()
                    || self.plan.execution_plan_sha256.is_none()
                    || self.plan.hgraph_sha256.is_none()
                    || self.plan.execution_intent_sha256.is_none()
                    || self.plan.deployment_sha256.is_some()
                    || !self.route_results.is_empty()
                    || self.validated_selection_receipt.is_some()
                    || self.intent.selection_reuse.is_some()
                    || self.selection_reuse.is_some()
                {
                    return Err(
                        "ordinary run input, engine, plan, or result fields are inconsistent"
                            .to_string(),
                    );
                }
            }
            RunInputKindV1::ProjectDirectory | RunInputKindV1::LiftedProjectBundle => {
                if !self.intent.engine.starts_with("project_")
                    || self.intent.target.is_none()
                    || self.intent.route_policy.is_none()
                    || self.intent.local_worker_limit.is_some()
                    || self.plan.oir_sha256.is_some()
                    || self.plan.execution_plan_sha256.is_some()
                    || self.plan.hgraph_sha256.is_none()
                    || self.plan.execution_intent_sha256.is_some()
                    || self.plan.deployment_sha256.is_none()
                {
                    return Err(
                        "project run input, engine, or plan fields are inconsistent".to_string()
                    );
                }
                match self.intent.engine.as_str() {
                    "project_mesh_prefer" if self.intent.mesh_mode.as_deref() == Some("prefer") => {
                    }
                    "project_mesh_required"
                        if self.intent.mesh_mode.as_deref() == Some("required") => {}
                    "project_compatibility" | "project_hgraph"
                        if self.intent.mesh_mode.is_none() => {}
                    _ => {
                        return Err(
                            "project engine and effective mesh mode are inconsistent".to_string()
                        )
                    }
                }
            }
        }
        self.stdout.validate()?;
        self.stderr.validate()?;
        let mut route_ids = BTreeSet::new();
        for result in &self.route_results {
            result.validate()?;
            if !route_ids.insert(result.route_id.as_str()) {
                return Err(format!(
                    "run record repeats route result `{}`",
                    result.route_id
                ));
            }
        }
        self.validate_validated_selection_receipt()?;
        self.validate_selection_reuse()?;
        let mut result_reference_ids = BTreeSet::new();
        for reference in &self.result_references {
            reference.validate()?;
            if !result_reference_ids.insert((reference.kind.as_str(), reference.id.as_str())) {
                return Err(format!(
                    "run record repeats result reference `{}`/`{}`",
                    reference.kind, reference.id
                ));
            }
        }
        let expected_references = match self.input.kind {
            RunInputKindV1::OrdinaryO => {
                decoded_value_result_references(self.decoded_value.as_ref(), "ordinary_o")
            }
            RunInputKindV1::ProjectDirectory | RunInputKindV1::LiftedProjectBundle => {
                route_result_references(&self.route_results)
            }
        };
        if self.result_references != expected_references {
            return Err(
                "run result references disagree with the retained decoded value or route results"
                    .to_string(),
            );
        }
        self.trace.validate()?;
        if let Some(failure) = &self.failure {
            failure.validate()?;
        }
        if self.disposition == RunDispositionV1::Succeeded && self.failure.is_some() {
            return Err("successful run must not carry a failure".to_string());
        }
        if self.disposition != RunDispositionV1::Succeeded && self.failure.is_none() {
            return Err("unsuccessful run must carry a failure".to_string());
        }
        if self.disposition == RunDispositionV1::Succeeded
            && self.input.kind != RunInputKindV1::OrdinaryO
            && !self
                .route_results
                .iter()
                .any(|result| result.exit_code == Some(0) && result.artifact_capture.is_complete())
        {
            return Err("successful project run has no successful route result".to_string());
        }
        if self.integrity != RUN_RECORD_INTEGRITY_V1 {
            return Err(format!(
                "run record integrity must be `{RUN_RECORD_INTEGRITY_V1}`"
            ));
        }
        Ok(())
    }

    fn validate_selection_reuse(&self) -> Result<(), String> {
        let Some(binding) = &self.intent.selection_reuse else {
            if self.selection_reuse.is_some() {
                return Err(
                    "selection-reuse observation exists without an admitted binding".to_string(),
                );
            }
            return Ok(());
        };
        binding.validate()?;
        if matches!(self.input.kind, RunInputKindV1::OrdinaryO)
            || self.input.digest_sha256 != binding.contract.bundle_sha256
            || self.validated_selection_receipt.is_some()
        {
            return Err(
                "selection-reuse binding is attached to an incompatible run input or fresh receipt"
                    .to_string(),
            );
        }

        let Some(observation) = &self.selection_reuse else {
            if matches!(
                self.disposition,
                RunDispositionV1::Interrupted | RunDispositionV1::RecordingIncomplete
            ) {
                return Ok(());
            }
            return Err(
                "completed selection-reuse execution has no terminal output check".to_string(),
            );
        };
        observation.validate_for(binding)?;
        if self.route_results.len() > 1
            || self
                .route_results
                .first()
                .is_some_and(|result| result.route_id != binding.contract.selected_route_id)
        {
            return Err(
                "selection-reuse run contains a route other than its admitted winner".to_string(),
            );
        }

        use crate::project::SelectionReuseOutputStatusV1;
        let selected_candidate = binding
            .receipt
            .candidates
            .iter()
            .find(|candidate| candidate.route_id == binding.contract.selected_route_id)
            .ok_or_else(|| "selection-reuse binding lost its selected candidate".to_string())?;
        let selected_codec = selected_candidate.observation.result_codec;
        let recomputed_output = self
            .route_results
            .first()
            .map(|result| {
                recorded_validated_selection_observation(result, selected_codec)
                    .and_then(|evidence| evidence.declared_output_sha256())
            })
            .transpose();
        let retained_contract_mismatch = self.route_results.first().is_some_and(|result| {
            let mut normalized_requirements = result.artifact_requirements.clone();
            normalized_requirements.sort();
            normalized_requirements.dedup();
            result.result_codec != Some(selected_codec)
                || normalized_requirements != selected_candidate.observation.artifact_requirements
        });
        if retained_contract_mismatch {
            return Err(
                "selection-reuse route result does not retain its admitted codec and artifact requirements"
                    .to_string(),
            );
        }
        match observation.output_check.status {
            SelectionReuseOutputStatusV1::Matched => {
                let result = self.route_results.first().ok_or_else(|| {
                    "matched selection-reuse run has no selected route result".to_string()
                })?;
                if result.exit_code != Some(0)
                    || !result.artifact_capture.is_complete()
                    || self.decoded_value != result.value
                {
                    return Err(
                        "matched selection-reuse result is not a successful selected output"
                            .to_string(),
                    );
                }
                let recomputed = recomputed_output.map_err(|_| {
                    "matched selection-reuse result has invalid declared-output evidence"
                        .to_string()
                })?;
                if recomputed.as_deref()
                    != observation
                        .output_check
                        .observed_declared_output_sha256
                        .as_deref()
                {
                    return Err(
                        "matched selection-reuse output digest was not derived from its recorded result"
                            .to_string(),
                    );
                }
            }
            SelectionReuseOutputStatusV1::DeclaredOutputMismatch => {
                if self.disposition == RunDispositionV1::Succeeded
                    || self.failure.as_ref().map(|failure| failure.stage.as_str())
                        != Some("selection_reuse_postcondition")
                {
                    return Err(
                        "selection-reuse output mismatch was not recorded as a failed postcondition"
                            .to_string(),
                    );
                }
                let result = self.route_results.first().ok_or_else(|| {
                    "selection-reuse output mismatch has no selected route result".to_string()
                })?;
                if result.exit_code != Some(0) || !result.artifact_capture.is_complete() {
                    return Err(
                        "selection-reuse output mismatch did not follow a successful route result"
                            .to_string(),
                    );
                }
                let recomputed = recomputed_output.map_err(|_| {
                    "selection-reuse mismatch result has invalid declared-output evidence"
                        .to_string()
                })?;
                if recomputed.as_deref()
                    != observation
                        .output_check
                        .observed_declared_output_sha256
                        .as_deref()
                {
                    return Err(
                        "selection-reuse mismatch digest was not derived from its recorded result"
                            .to_string(),
                    );
                }
            }
            SelectionReuseOutputStatusV1::RouteFailed => {
                if self.disposition == RunDispositionV1::Succeeded {
                    return Err(
                        "failed selection-reuse output check cannot accompany a successful run"
                            .to_string(),
                    );
                }
                let result = self.route_results.first().ok_or_else(|| {
                    "selection-reuse route-failed status has no selected route result".to_string()
                })?;
                if result.exit_code == Some(0) && result.artifact_capture.is_complete() {
                    return Err(
                        "selection-reuse route-failed status accompanies a successful route"
                            .to_string(),
                    );
                }
            }
            SelectionReuseOutputStatusV1::ObservationInvalid => {
                if self.disposition == RunDispositionV1::Succeeded {
                    return Err(
                        "invalid selection-reuse evidence cannot accompany a successful run"
                            .to_string(),
                    );
                }
                if recomputed_output.is_ok()
                    && recomputed_output.as_ref().is_ok_and(Option::is_some)
                {
                    return Err(
                        "selection-reuse observation-invalid status has valid recorded output evidence"
                            .to_string(),
                    );
                }
            }
        }
        Ok(())
    }

    fn validate_validated_selection_receipt(&self) -> Result<(), String> {
        let policy_is_validated =
            self.intent.route_policy.as_deref() == Some("benchmark_validate_and_select");
        let Some(receipt) = &self.validated_selection_receipt else {
            let known_post_execution_failure = self.disposition
                == RunDispositionV1::InfrastructureFailed
                && self.failure.as_ref().is_some_and(|failure| {
                    matches!(
                        failure.stage.as_str(),
                        "trace_output" | "stream_observation" | "validated_selection_evidence"
                    )
                });
            if policy_is_validated
                && (self.disposition == RunDispositionV1::Succeeded
                    || !self.route_results.is_empty()
                    || known_post_execution_failure)
            {
                return Err(
                    "completed validated-selection execution has no selection receipt".to_string(),
                );
            }
            return Ok(());
        };
        if matches!(self.input.kind, RunInputKindV1::OrdinaryO) || !policy_is_validated {
            return Err(
                "validated-selection receipt is attached to an incompatible run input or policy"
                    .to_string(),
            );
        }
        if !matches!(
            self.disposition,
            RunDispositionV1::Succeeded | RunDispositionV1::InfrastructureFailed
        ) {
            return Err(
                "validated-selection receipt is attached to an incompatible run disposition"
                    .to_string(),
            );
        }
        receipt.validate()?;
        if self.input.digest_sha256 != receipt.bundle_sha256
            || self.intent.target.as_deref() != Some(receipt.target.as_str())
            || self.intent.route_policy.as_deref() != Some(receipt.policy.as_str())
        {
            return Err(
                "validated-selection receipt is not bound to the run input, target, and policy"
                    .to_string(),
            );
        }
        if receipt.candidates.len() != self.route_results.len() {
            return Err(
                "validated-selection receipt candidate count disagrees with route results"
                    .to_string(),
            );
        }
        let mut expected_result_order = receipt
            .candidates
            .iter()
            .filter(|candidate| candidate.route_id != receipt.selected_route_id)
            .map(|candidate| candidate.route_id.as_str())
            .collect::<Vec<_>>();
        expected_result_order.push(receipt.selected_route_id.as_str());
        let observed_result_order = self
            .route_results
            .iter()
            .map(|result| result.route_id.as_str())
            .collect::<Vec<_>>();
        if observed_result_order != expected_result_order {
            return Err(
                "validated-selection route results do not preserve candidate order plus winner-last"
                    .to_string(),
            );
        }
        for candidate in &receipt.candidates {
            let result = self
                .route_results
                .iter()
                .find(|result| result.route_id == candidate.route_id)
                .ok_or_else(|| {
                    format!(
                        "validated-selection candidate `{}` has no recorded route result",
                        candidate.route_id
                    )
                })?;
            let recorded_codec = result.result_codec.ok_or_else(|| {
                format!(
                    "validated-selection candidate `{}` has no recorded result codec",
                    candidate.route_id
                )
            })?;
            if recorded_codec != candidate.observation.result_codec {
                return Err(format!(
                    "validated-selection candidate `{}` codec disagrees with its recorded route result",
                    candidate.route_id
                ));
            }
            if result.branch_elapsed_ns.as_deref() != Some(candidate.branch_elapsed_ns.as_str())
                || result.duration_ns != candidate.terminal_elapsed_ns
            {
                return Err(format!(
                    "validated-selection candidate `{}` timing disagrees with its recorded route result",
                    candidate.route_id
                ));
            }
            let branch_duration = parse_canonical_duration(
                &candidate.branch_elapsed_ns,
                "validated-selection complete-branch duration",
            )?;
            let terminal_duration =
                parse_canonical_duration(&result.duration_ns, "route terminal duration")?;
            if branch_duration < terminal_duration
                || branch_duration > u128::from(self.elapsed_nanos)
            {
                return Err(format!(
                    "validated-selection candidate `{}` timing falls outside terminal and whole-run bounds",
                    candidate.route_id
                ));
            }
            let observation = recorded_validated_selection_observation(result, recorded_codec)?;
            if observation != candidate.observation {
                return Err(format!(
                    "validated-selection candidate `{}` evidence disagrees with its recorded route result",
                    candidate.route_id
                ));
            }
        }
        let selected = self.route_results.last().ok_or_else(|| {
            "validated-selection receipt has no selected route result".to_string()
        })?;
        if selected.route_id != receipt.selected_route_id
            || selected.exit_code != Some(0)
            || !selected.artifact_capture.is_complete()
            || self.decoded_value != selected.value
        {
            return Err(
                "validated-selection selected result or decoded value is inconsistent".to_string(),
            );
        }
        Ok(())
    }
}

fn recorded_validated_selection_observation(
    result: &RecordedRouteResultV1,
    result_codec: ResultCodec,
) -> Result<ValidatedSelectionObservationV1, String> {
    let json_value_sha256 = match result_codec {
        ResultCodec::Json => {
            let decoded = if result.stdout.capture.truncated {
                None
            } else {
                serde_json::from_slice::<Value>(&result.stdout.retained).ok()
            };
            if decoded != result.value {
                return Err(format!(
                    "recorded route `{}` JSON value disagrees with stdout",
                    result.route_id
                ));
            }
            result
                .value
                .as_ref()
                .map(validated_selection_json_sha256)
                .transpose()?
        }
        ResultCodec::Text | ResultCodec::Bytes => {
            if result.value.is_some() {
                return Err(format!(
                    "recorded non-JSON route `{}` carries a decoded JSON value",
                    result.route_id
                ));
            }
            None
        }
    };
    let mut artifacts = result.artifacts.clone();
    artifacts.sort_unstable_by(|left, right| {
        (&left.path, left.bytes_len, &left.content_hash).cmp(&(
            &right.path,
            right.bytes_len,
            &right.content_hash,
        ))
    });
    let mut artifact_requirements = result.artifact_requirements.clone();
    artifact_requirements.sort_unstable();
    artifact_requirements.dedup();
    let artifact_capture = ValidatedArtifactCaptureStatusV1::from_capture(
        &result.artifact_capture,
        &artifact_requirements,
    )?;
    let observation = ValidatedSelectionObservationV1 {
        result_codec,
        exit_code: result.exit_code,
        stdout_capture: result.stdout.capture.clone(),
        stderr_capture: result.stderr.capture.clone(),
        json_value_sha256,
        artifacts,
        artifact_requirements,
        artifact_capture,
        execution_disposition: result.disposition,
    };
    observation.validate()?;
    Ok(observation)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunResultReferenceV1 {
    pub kind: String,
    pub id: String,
    pub sha256: Option<String>,
    pub bytes_len: Option<u64>,
    pub complete: bool,
}

impl RunResultReferenceV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_nonempty(&self.kind, "result-reference kind")?;
        validate_nonempty(&self.id, "result-reference id")?;
        match (&self.sha256, self.bytes_len) {
            (Some(digest), Some(_)) => validate_lower_hex_64(digest, "result-reference digest"),
            (None, None) => Ok(()),
            _ => Err("result reference digest and byte length must appear together".to_string()),
        }
    }
}

/// Deterministically bind one decoded top-level value without depending on
/// `serde_json` map insertion order.
pub fn decoded_value_result_references(
    value: Option<&Value>,
    id: &str,
) -> Vec<RunResultReferenceV1> {
    value
        .map(canonical_json_bytes)
        .map(|bytes| vec![result_reference("decoded_value", id, &bytes, true)])
        .unwrap_or_default()
}

/// Recompute the only valid route-output/artifact reference inventory for a
/// retained project result list.
pub fn route_result_references(results: &[RecordedRouteResultV1]) -> Vec<RunResultReferenceV1> {
    let mut references = Vec::new();
    for result in results {
        references.push(RunResultReferenceV1 {
            kind: "route_stdout".to_string(),
            id: result.route_id.clone(),
            sha256: Some(result.stdout.capture.sha256.clone()),
            bytes_len: Some(result.stdout.capture.total_observed_bytes),
            complete: !result.stdout.capture.truncated,
        });
        references.push(RunResultReferenceV1 {
            kind: "route_stderr".to_string(),
            id: result.route_id.clone(),
            sha256: Some(result.stderr.capture.sha256.clone()),
            bytes_len: Some(result.stderr.capture.total_observed_bytes),
            complete: !result.stderr.capture.truncated,
        });
        references.extend(
            result
                .artifacts
                .iter()
                .map(|artifact| RunResultReferenceV1 {
                    kind: "artifact".to_string(),
                    id: format!("{}:{}", result.route_id, artifact.path),
                    sha256: Some(artifact.content_hash.clone()),
                    bytes_len: Some(artifact.bytes_len),
                    complete: true,
                }),
        );
    }
    references
}

fn result_reference(kind: &str, id: &str, bytes: &[u8], complete: bool) -> RunResultReferenceV1 {
    RunResultReferenceV1 {
        kind: kind.to_string(),
        id: id.to_string(),
        sha256: Some(hex::encode(Sha256::digest(bytes))),
        bytes_len: Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
        complete,
    }
}

fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    fn ordered(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(ordered).collect()),
            Value::Object(values) => {
                let mut keys = values.keys().collect::<Vec<_>>();
                keys.sort();
                let mut canonical = serde_json::Map::new();
                for key in keys {
                    canonical.insert(key.clone(), ordered(&values[key]));
                }
                Value::Object(canonical)
            }
            value => value.clone(),
        }
    }
    serde_json::to_vec(&ordered(value)).expect("serde_json::Value serialization cannot fail")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunRecordingStatusV1 {
    Recorded {
        sequence: u64,
        record_sha256: String,
    },
    Disabled,
    NotStarted {
        reason: String,
    },
    Incomplete {
        detail: String,
    },
}

/// Single JSON envelope emitted by `o run --json`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunSummaryV1 {
    pub schema: String,
    pub run_id: Option<String>,
    /// Exact identities exist only after preflight has produced an executable
    /// intent. A preflight-failure envelope leaves both fields absent rather
    /// than inventing source or plan evidence.
    pub input: Option<RunInputIdentityV1>,
    pub plan: Option<PlanIdentitiesV1>,
    pub disposition: RunDispositionV1,
    pub result_references: Vec<RunResultReferenceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_reuse: Option<ProjectSelectionReuseObservationV1>,
    pub recording: RunRecordingStatusV1,
    pub failure: Option<RunFailureV1>,
}

impl RunSummaryV1 {
    pub fn from_record(record: &RunRecordV1, recording: RunRecordingStatusV1) -> Self {
        Self {
            schema: RUN_SUMMARY_SCHEMA_V1.to_string(),
            run_id: Some(record.run_id.clone()),
            input: Some(record.input.clone()),
            plan: Some(record.plan.clone()),
            disposition: record.disposition,
            result_references: record.result_references.clone(),
            selection_reuse: record.selection_reuse.clone(),
            recording,
            failure: record.failure.clone(),
        }
    }

    pub fn preflight_failed(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            schema: RUN_SUMMARY_SCHEMA_V1.to_string(),
            run_id: None,
            input: None,
            plan: None,
            disposition: RunDispositionV1::PreflightFailed,
            result_references: Vec::new(),
            selection_reuse: None,
            recording: RunRecordingStatusV1::NotStarted {
                reason: "preflight did not produce an executable intent".to_string(),
            },
            failure: Some(RunFailureV1 {
                stage: "preflight".to_string(),
                message,
            }),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RUN_SUMMARY_SCHEMA_V1 {
            return Err(format!("unsupported run-summary schema `{}`", self.schema));
        }
        if let Some(run_id) = &self.run_id {
            validate_lower_hex_64(run_id, "run-summary id")?;
        }
        match (&self.input, &self.plan) {
            (Some(input), Some(plan)) => {
                input.validate()?;
                plan.validate()?;
            }
            (None, None) if self.disposition == RunDispositionV1::PreflightFailed => {}
            _ => {
                return Err(
                    "run-summary input and plan identities must be present or absent together"
                        .to_string(),
                )
            }
        }
        for reference in &self.result_references {
            reference.validate()?;
        }
        if let Some(observation) = &self.selection_reuse {
            observation.validate()?;
            if self.run_id.is_none() || self.input.is_none() || self.plan.is_none() {
                return Err("selection-reuse summary lacks an executed run identity".to_string());
            }
        }
        match &self.recording {
            RunRecordingStatusV1::Recorded {
                sequence,
                record_sha256,
            } => {
                if *sequence == 0 {
                    return Err("recorded run sequence must be positive".to_string());
                }
                validate_lower_hex_64(record_sha256, "recorded run object digest")?;
                if self.run_id.is_none() {
                    return Err("recorded run summary is missing its run id".to_string());
                }
            }
            RunRecordingStatusV1::Disabled => {
                if self.run_id.is_some() {
                    return Err("unrecorded run summary must not allocate a run id".to_string());
                }
            }
            RunRecordingStatusV1::NotStarted { reason } => {
                validate_nonempty(reason, "recording-not-started reason")?;
                if self.run_id.is_some()
                    || self.input.is_some()
                    || self.plan.is_some()
                    || self.disposition != RunDispositionV1::PreflightFailed
                {
                    return Err(
                        "recording-not-started is reserved for preflight failure".to_string()
                    );
                }
            }
            RunRecordingStatusV1::Incomplete { detail } => {
                validate_nonempty(detail, "incomplete-recording detail")?;
            }
        }
        if self.disposition == RunDispositionV1::PreflightFailed {
            if self.run_id.is_some()
                || !self.result_references.is_empty()
                || !matches!(&self.recording, RunRecordingStatusV1::NotStarted { .. })
            {
                return Err("preflight-failure summary contains post-preflight state".to_string());
            }
        } else if self.input.is_none() || self.plan.is_none() {
            return Err("post-preflight summary is missing exact identities".to_string());
        }
        if self.disposition == RunDispositionV1::Succeeded && self.failure.is_some() {
            return Err("successful run summary must not carry a failure".to_string());
        }
        if self.disposition != RunDispositionV1::Succeeded && self.failure.is_none() {
            return Err("unsuccessful run summary must carry a failure".to_string());
        }
        if let Some(failure) = &self.failure {
            failure.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementPlanningModeV1 {
    Static,
    LiveReadOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalReadinessPreviewV1 {
    pub runtime_ready: bool,
    pub worker_count: Option<u32>,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlacementCandidatePreviewV1 {
    pub node_id: String,
    pub endpoint_hint: Option<String>,
    pub available_slots: Option<u32>,
    pub observed_latency_micros: Option<u64>,
    pub authenticated: bool,
    pub eligible: bool,
    pub detail: String,
}

/// Read-only planner output.  A live preview is an observation only and must
/// never be interpreted as reservation, admission, or execution authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlacementPreviewV1 {
    pub schema: String,
    pub input: RunInputIdentityV1,
    pub plan: PlanIdentitiesV1,
    pub mode: PlacementPlanningModeV1,
    pub local: LocalReadinessPreviewV1,
    pub candidates: Vec<PlacementCandidatePreviewV1>,
    pub selected_node_id: Option<String>,
    pub explanation: Vec<String>,
    pub integrity: String,
}

impl PlacementPreviewV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PLACEMENT_PREVIEW_SCHEMA_V1 {
            return Err(format!(
                "unsupported placement-preview schema `{}`",
                self.schema
            ));
        }
        self.input.validate()?;
        self.plan.validate()?;
        validate_nonempty(&self.local.detail, "local readiness detail")?;
        let mut nodes = BTreeSet::new();
        for candidate in &self.candidates {
            validate_nonempty(&candidate.node_id, "placement candidate node id")?;
            validate_nonempty(&candidate.detail, "placement candidate detail")?;
            if !nodes.insert(candidate.node_id.as_str()) {
                return Err(format!(
                    "placement preview repeats node `{}`",
                    candidate.node_id
                ));
            }
            if candidate.eligible && !candidate.authenticated {
                return Err(format!(
                    "placement candidate `{}` is eligible without authenticated identity",
                    candidate.node_id
                ));
            }
        }
        if let Some(selected) = &self.selected_node_id {
            validate_nonempty(selected, "selected placement node")?;
            if !self
                .candidates
                .iter()
                .any(|candidate| candidate.node_id == *selected && candidate.eligible)
            {
                return Err("selected placement node is not an eligible candidate".to_string());
            }
        }
        for line in &self.explanation {
            validate_nonempty(line, "placement explanation")?;
        }
        if self.integrity != RUN_RECORD_INTEGRITY_V1 {
            return Err(format!(
                "placement preview integrity must be `{RUN_RECORD_INTEGRITY_V1}`"
            ));
        }
        Ok(())
    }
}

mod b64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{
        RoutePolicy, SelectionReuseOutputStatusV1, SELECTION_REUSE_CONTRACT_SCHEMA_V1,
        SELECTION_REUSE_EFFECT_BOUNDARY_V1, SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1,
        VALIDATED_SELECTION_EQUIVALENCE_V1, VALIDATED_SELECTION_RULE_V1,
    };
    use sha2::{Digest, Sha256};

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn seed() -> RunAttemptSeedV1 {
        RunAttemptSeedV1 {
            input: RunInputIdentityV1 {
                kind: RunInputKindV1::OrdinaryO,
                path: PathBuf::from("example.O"),
                digest_sha256: digest(b"example"),
            },
            intent: ExecutionIntentObservationV1 {
                engine: "local_hgraph".to_string(),
                target: Some("local".to_string()),
                selected_route: None,
                route_policy: None,
                route_declarations: Vec::new(),
                parallel_policy: "local".to_string(),
                local_worker_limit: None,
                mesh_mode: None,
                mesh_max_retries: None,
                mesh_fallback: None,
                mesh_discovery_timeout_ms: None,
                mesh_closed_registry: None,
                mesh_peer_root: None,
                selection_reuse: None,
            },
            plan: PlanIdentitiesV1 {
                oir_sha256: Some(digest(b"oir")),
                execution_plan_sha256: Some(digest(b"plan")),
                hgraph_sha256: Some(digest(b"hgraph")),
                execution_intent_sha256: Some(digest(b"intent")),
                ..PlanIdentitiesV1::default()
            },
            started_unix_nanos: 1,
        }
    }

    fn validated_project_seed() -> RunAttemptSeedV1 {
        RunAttemptSeedV1 {
            input: RunInputIdentityV1 {
                kind: RunInputKindV1::ProjectDirectory,
                path: PathBuf::from("project"),
                digest_sha256: "ab".repeat(32),
            },
            intent: ExecutionIntentObservationV1 {
                engine: "project_compatibility".to_string(),
                target: Some("main".to_string()),
                selected_route: None,
                route_policy: Some("benchmark_validate_and_select".to_string()),
                route_declarations: Vec::new(),
                parallel_policy: "project_policy".to_string(),
                local_worker_limit: None,
                mesh_mode: None,
                mesh_max_retries: None,
                mesh_fallback: None,
                mesh_discovery_timeout_ms: None,
                mesh_closed_registry: None,
                mesh_peer_root: None,
                selection_reuse: None,
            },
            plan: PlanIdentitiesV1 {
                hgraph_sha256: Some(digest(b"project-hgraph")),
                deployment_sha256: Some(digest(b"project-deployment")),
                ..PlanIdentitiesV1::default()
            },
            started_unix_nanos: 1,
        }
    }

    fn recorded_text_result(route_id: &str, stdout: &[u8]) -> RecordedRouteResultV1 {
        RecordedRouteResultV1 {
            route_id: route_id.to_string(),
            result_codec: Some(ResultCodec::Text),
            exit_code: Some(0),
            stdout: CapturedStreamV1::complete(stdout.to_vec()),
            stderr: CapturedStreamV1::default(),
            value: None,
            artifacts: Vec::new(),
            artifact_requirements: Vec::new(),
            artifact_capture: ArtifactCaptureStatus::Complete,
            disposition: RouteExecutionDisposition::Executed,
            duration_ns: "1".to_string(),
            branch_elapsed_ns: None,
            provenance: RecordedExecutionProvenanceV1 {
                execution_scope: "isolated_project_workspace".to_string(),
                command_argv_retained: false,
                command_argument_count: 3,
            },
        }
    }

    fn receipt_candidate(
        result: &RecordedRouteResultV1,
        branch_elapsed_ns: &str,
    ) -> ValidatedSelectionCandidateV1 {
        let observation =
            recorded_validated_selection_observation(result, ResultCodec::Text).unwrap();
        ValidatedSelectionCandidateV1 {
            route_id: result.route_id.clone(),
            terminal_elapsed_ns: result.duration_ns.clone(),
            branch_elapsed_ns: branch_elapsed_ns.to_string(),
            observation_sha256: observation.sha256().unwrap(),
            declared_output_sha256: observation.declared_output_sha256().unwrap(),
            observation,
            disposition: ValidatedSelectionDispositionV1::Eligible,
        }
    }

    fn matched_selection_reuse_record() -> RunRecordV1 {
        let mut reference = recorded_text_result("reference", b"same");
        reference.artifacts = vec![Artifact {
            path: "out.txt".to_string(),
            content_hash: digest(b"artifact"),
            bytes_len: 8,
        }];
        reference.artifact_requirements = vec!["out.txt".to_string()];
        let mut fast = reference.clone();
        fast.route_id = "fast".to_string();

        let receipt = ValidatedSelectionReceiptV1::new(
            "fixture",
            "ab".repeat(32),
            "main",
            "reference",
            vec![
                receipt_candidate(&reference, "20"),
                receipt_candidate(&fast, "10"),
            ],
            "fast",
        )
        .unwrap();
        let expected_declared_output_sha256 = receipt
            .candidates
            .iter()
            .find(|candidate| candidate.route_id == "fast")
            .unwrap()
            .declared_output_sha256
            .clone();
        let contract = SelectionReuseContractV1 {
            schema: SELECTION_REUSE_CONTRACT_SCHEMA_V1.to_string(),
            project_name: "fixture".to_string(),
            bundle_sha256: receipt.bundle_sha256.clone(),
            target: "main".to_string(),
            ordered_alternatives: vec!["reference".to_string(), "fast".to_string()],
            evidence_policy: RoutePolicy::BenchmarkValidateAndSelect.token(),
            equivalence_contract: VALIDATED_SELECTION_EQUIVALENCE_V1.to_string(),
            selection_rule: VALIDATED_SELECTION_RULE_V1.to_string(),
            effect_boundary: SELECTION_REUSE_EFFECT_BOUNDARY_V1.to_string(),
            reference_route_id: "reference".to_string(),
            selected_route_id: "fast".to_string(),
            expected_declared_output_sha256: expected_declared_output_sha256.clone(),
            benchmark_hgraph_sha256: digest(b"benchmark-hgraph"),
            benchmark_deployment_sha256: digest(b"benchmark-deployment"),
            route_declaration_sha256: Vec::new(),
        };
        let binding = ProjectSelectionReuseBindingV1::new(
            "cc".repeat(32),
            7,
            RunContentRefV1 {
                kind: RunContentKindV1::Record,
                sha256: "dd".repeat(32),
                bytes_len: 1,
            },
            receipt,
            contract,
        )
        .unwrap();
        let seed = RunAttemptSeedV1 {
            input: RunInputIdentityV1 {
                kind: RunInputKindV1::ProjectDirectory,
                path: PathBuf::from("project"),
                digest_sha256: binding.contract.bundle_sha256.clone(),
            },
            intent: ExecutionIntentObservationV1 {
                engine: "project_compatibility".to_string(),
                target: Some(binding.contract.target.clone()),
                selected_route: Some(binding.contract.target.clone()),
                route_policy: Some(
                    RoutePolicy::Explicit(binding.contract.selected_route_id.clone()).token(),
                ),
                route_declarations: binding.contract.route_declaration_sha256.clone(),
                parallel_policy: "project_policy".to_string(),
                local_worker_limit: None,
                mesh_mode: None,
                mesh_max_retries: None,
                mesh_fallback: None,
                mesh_discovery_timeout_ms: None,
                mesh_closed_registry: None,
                mesh_peer_root: None,
                selection_reuse: Some(binding.clone()),
            },
            plan: PlanIdentitiesV1 {
                hgraph_sha256: Some(digest(b"selected-hgraph")),
                deployment_sha256: Some(digest(b"selected-deployment")),
                ..PlanIdentitiesV1::default()
            },
            started_unix_nanos: 1,
        };
        let route_results = vec![fast];
        let mut record = RunRecordV1::terminal(
            "ee".repeat(32),
            8,
            &seed,
            21,
            20,
            RunDispositionV1::Succeeded,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            None,
            route_results.clone(),
            route_result_references(&route_results),
            RunTraceBindingV1::unavailable("compatibility engine has no checked trace"),
            None,
        );
        record.selection_reuse = Some(
            ProjectSelectionReuseObservationV1::from_binding(
                &binding,
                SelectionReuseOutputCheckV1 {
                    schema: SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1.to_string(),
                    status: SelectionReuseOutputStatusV1::Matched,
                    expected_declared_output_sha256: expected_declared_output_sha256.clone(),
                    observed_declared_output_sha256: Some(expected_declared_output_sha256),
                },
            )
            .unwrap(),
        );
        record.validate().unwrap();
        record
    }

    fn route_failed_selection_reuse_record() -> RunRecordV1 {
        let mut record = matched_selection_reuse_record();
        let binding = record.intent.selection_reuse.as_ref().unwrap().clone();
        record.disposition = RunDispositionV1::ExecutionFailed;
        record.failure = Some(RunFailureV1 {
            stage: "execution".to_string(),
            message: "selected route failed".to_string(),
        });
        record.route_results[0].exit_code = Some(1);
        record.result_references = route_result_references(&record.route_results);
        record.selection_reuse = Some(
            ProjectSelectionReuseObservationV1::from_binding(
                &binding,
                SelectionReuseOutputCheckV1 {
                    schema: SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1.to_string(),
                    status: SelectionReuseOutputStatusV1::RouteFailed,
                    expected_declared_output_sha256: binding
                        .contract
                        .expected_declared_output_sha256
                        .clone(),
                    observed_declared_output_sha256: None,
                },
            )
            .unwrap(),
        );
        record.validate().unwrap();
        record
    }

    #[test]
    fn selection_reuse_record_recomputes_retained_output_evidence() {
        let record = matched_selection_reuse_record();

        let mut forged_observed_digest = record.clone();
        forged_observed_digest
            .selection_reuse
            .as_mut()
            .unwrap()
            .output_check
            .observed_declared_output_sha256 = Some(digest(b"forged"));
        assert!(forged_observed_digest.validate().is_err());

        let mut changed_stdout = record.clone();
        changed_stdout.route_results[0].stdout = CapturedStreamV1::complete(b"changed".to_vec());
        assert!(changed_stdout
            .validate()
            .unwrap_err()
            .contains("digest was not derived from its recorded result"));

        let mut changed_artifact = record.clone();
        changed_artifact.route_results[0].artifacts[0].content_hash = digest(b"changed");
        assert!(changed_artifact
            .validate()
            .unwrap_err()
            .contains("digest was not derived from its recorded result"));

        let mut changed_codec = record.clone();
        changed_codec.route_results[0].result_codec = Some(ResultCodec::Bytes);
        assert!(changed_codec
            .validate()
            .unwrap_err()
            .contains("admitted codec and artifact requirements"));

        let mut extra_route = record.clone();
        let mut reference = extra_route.route_results[0].clone();
        reference.route_id = "reference".to_string();
        extra_route.route_results.push(reference);
        extra_route.result_references = route_result_references(&extra_route.route_results);
        assert!(extra_route
            .validate()
            .unwrap_err()
            .contains("route other than its admitted winner"));

        let mut contradictory_status = record;
        let check = &mut contradictory_status
            .selection_reuse
            .as_mut()
            .unwrap()
            .output_check;
        check.status = SelectionReuseOutputStatusV1::RouteFailed;
        check.observed_declared_output_sha256 = None;
        assert!(contradictory_status
            .validate()
            .unwrap_err()
            .contains("cannot accompany a successful run"));
    }

    #[test]
    fn selection_reuse_route_failed_status_requires_the_selected_result() {
        let mut record = route_failed_selection_reuse_record();
        record.route_results.clear();
        record.result_references.clear();
        assert!(record
            .validate()
            .unwrap_err()
            .contains("route-failed status has no selected route result"));
    }

    #[test]
    fn selection_reuse_result_must_retain_admitted_artifact_requirements() {
        let mut record = matched_selection_reuse_record();
        record.route_results[0].artifact_requirements = vec!["other.txt".to_string()];
        assert!(record
            .validate()
            .unwrap_err()
            .contains("admitted codec and artifact requirements"));
    }

    #[test]
    fn captured_stream_preserves_arbitrary_bytes() {
        let stream = CapturedStreamV1::complete(vec![0, 0xff, b'\n']);
        let json = serde_json::to_vec(&stream).unwrap();
        let decoded: CapturedStreamV1 = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded, stream);
        decoded.validate().unwrap();
    }

    #[test]
    fn terminal_record_identifies_itself_as_unsigned_observation() {
        let seed = seed();
        let decoded = Value::Bool(true);
        let record = RunRecordV1::terminal(
            "11".repeat(32),
            1,
            &seed,
            2,
            1,
            RunDispositionV1::Succeeded,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            Some(decoded.clone()),
            Vec::new(),
            decoded_value_result_references(Some(&decoded), "ordinary_o"),
            RunTraceBindingV1::unavailable("compatibility engine has no checked trace"),
            None,
        );
        record.validate().unwrap();
        assert_eq!(record.integrity, RUN_RECORD_INTEGRITY_V1);
        assert!(serde_json::to_value(&record)
            .unwrap()
            .get("validated_selection_receipt")
            .is_none());
    }

    #[test]
    fn terminal_record_binds_validated_selection_receipt_to_results() {
        let seed = validated_project_seed();
        let reference = recorded_text_result("reference", b"same");
        let fast = recorded_text_result("fast", b"same");
        let receipt = ValidatedSelectionReceiptV1::new(
            "fixture",
            seed.input.digest_sha256.clone(),
            "main",
            "reference",
            vec![
                receipt_candidate(&reference, "20"),
                receipt_candidate(&fast, "10"),
            ],
            "fast",
        )
        .unwrap();
        let mut route_results = vec![reference, fast];
        for (result, candidate) in route_results.iter_mut().zip(&receipt.candidates) {
            result.branch_elapsed_ns = Some(candidate.branch_elapsed_ns.clone());
        }
        let mut record = RunRecordV1::terminal(
            "44".repeat(32),
            1,
            &seed,
            31,
            30,
            RunDispositionV1::Succeeded,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            None,
            route_results.clone(),
            route_result_references(&route_results),
            RunTraceBindingV1::unavailable("compatibility engine has no checked trace"),
            None,
        );
        record.validated_selection_receipt = Some(receipt);
        record.validate().unwrap();

        let mut wrong_result = record.clone();
        wrong_result.route_results[0].stdout = CapturedStreamV1::complete(b"changed".to_vec());
        assert!(wrong_result.validate().is_err());

        let mut wrong_binding = record.clone();
        wrong_binding.input.digest_sha256 = digest(b"different-bundle");
        assert!(wrong_binding.validate().is_err());

        let mut wrong_codec = record.clone();
        wrong_codec.route_results[0].result_codec = Some(ResultCodec::Bytes);
        assert!(wrong_codec.validate().is_err());

        let mut shorter_than_terminal = record.clone();
        shorter_than_terminal.route_results[1].duration_ns = "11".to_string();
        assert!(shorter_than_terminal.validate().is_err());

        let mut longer_than_run = record.clone();
        longer_than_run.elapsed_nanos = 9;
        assert!(longer_than_run.validate().is_err());

        let mut missing_receipt = record;
        missing_receipt.validated_selection_receipt = None;
        assert!(missing_receipt.validate().is_err());

        let mut infrastructure_failed = validated_project_seed();
        infrastructure_failed.started_unix_nanos = 10;
        let reference = recorded_text_result("reference", b"same");
        let fast = recorded_text_result("fast", b"same");
        let receipt = ValidatedSelectionReceiptV1::new(
            "fixture",
            infrastructure_failed.input.digest_sha256.clone(),
            "main",
            "reference",
            vec![
                receipt_candidate(&reference, "20"),
                receipt_candidate(&fast, "10"),
            ],
            "fast",
        )
        .unwrap();
        let mut route_results = vec![reference, fast];
        for (result, candidate) in route_results.iter_mut().zip(&receipt.candidates) {
            result.branch_elapsed_ns = Some(candidate.branch_elapsed_ns.clone());
        }
        let mut failed_record = RunRecordV1::terminal(
            "55".repeat(32),
            2,
            &infrastructure_failed,
            40,
            30,
            RunDispositionV1::InfrastructureFailed,
            CapturedStreamV1::default(),
            CapturedStreamV1::complete(b"sidecar failed".to_vec()),
            None,
            route_results.clone(),
            route_result_references(&route_results),
            RunTraceBindingV1::unavailable("compatibility engine has no checked trace"),
            Some(RunFailureV1 {
                stage: "trace_output".to_string(),
                message: "sidecar failed".to_string(),
            }),
        );
        failed_record.validated_selection_receipt = Some(receipt);
        failed_record.validate().unwrap();
        failed_record.validated_selection_receipt = None;
        assert!(failed_record.validate().is_err());

        let post_execution_without_results = RunRecordV1::terminal(
            "66".repeat(32),
            3,
            &infrastructure_failed,
            40,
            30,
            RunDispositionV1::InfrastructureFailed,
            CapturedStreamV1::default(),
            CapturedStreamV1::complete(b"sidecar failed".to_vec()),
            None,
            Vec::new(),
            Vec::new(),
            RunTraceBindingV1::unavailable("compatibility engine has no checked trace"),
            Some(RunFailureV1 {
                stage: "trace_output".to_string(),
                message: "sidecar failed".to_string(),
            }),
        );
        assert_eq!(
            post_execution_without_results.validate().unwrap_err(),
            "completed validated-selection execution has no selection receipt"
        );

        let setup_failure = RunRecordV1::terminal(
            "77".repeat(32),
            4,
            &infrastructure_failed,
            40,
            30,
            RunDispositionV1::InfrastructureFailed,
            CapturedStreamV1::default(),
            CapturedStreamV1::complete(b"capture setup failed".to_vec()),
            None,
            Vec::new(),
            Vec::new(),
            RunTraceBindingV1::unavailable("execution never began"),
            Some(RunFailureV1 {
                stage: "stream_observation_setup".to_string(),
                message: "capture setup failed".to_string(),
            }),
        );
        setup_failure.validate().unwrap();
    }

    #[test]
    fn terminal_record_recomputes_result_references_from_retained_value() {
        let seed = seed();
        let decoded = Value::String("retained".to_string());
        let mut record = RunRecordV1::terminal(
            "22".repeat(32),
            1,
            &seed,
            2,
            1,
            RunDispositionV1::Succeeded,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            Some(decoded.clone()),
            Vec::new(),
            decoded_value_result_references(Some(&decoded), "ordinary_o"),
            RunTraceBindingV1::unavailable("test trace unavailable"),
            None,
        );
        record.validate().unwrap();
        record.result_references[0].sha256 = Some(digest(b"substituted"));
        assert!(record
            .validate()
            .unwrap_err()
            .contains("disagree with the retained decoded value"));
    }

    #[test]
    fn preflight_failure_cannot_validate_as_a_terminal_record() {
        let seed = seed();
        let mut record = RunRecordV1::terminal(
            "33".repeat(32),
            1,
            &seed,
            2,
            1,
            RunDispositionV1::ExecutionFailed,
            CapturedStreamV1::default(),
            CapturedStreamV1::default(),
            None,
            Vec::new(),
            Vec::new(),
            RunTraceBindingV1::unavailable("execution did not produce a trace"),
            Some(RunFailureV1 {
                stage: "execution".to_string(),
                message: "failed".to_string(),
            }),
        );
        record.disposition = RunDispositionV1::PreflightFailed;
        assert!(record
            .validate()
            .unwrap_err()
            .contains("cannot be represented by a durable run record"));
    }

    #[test]
    fn mesh_tuning_without_mesh_mode_is_rejected() {
        let mut seed = seed();
        seed.intent.mesh_max_retries = Some(1);
        assert!(seed.validate().unwrap_err().contains("mesh tuning"));
    }
}
