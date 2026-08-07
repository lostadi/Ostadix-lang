//! Deterministic attempt tracing for the hosted project coordinator.
//!
//! A [`ProjectAttemptEvent`] records a coordinator-assigned event ordinal and
//! its planner-local [`PlanNodeId`]. The ordinal is the event's position in the
//! committed lifecycle sequence: ordinals are unique and contiguous from zero.
//! It is not a wall-clock timestamp or an operation identity; [`PlanNodeId`]
//! identifies the operation.
//!
//! A non-route operation's successful commit (linearization) point is one
//! indivisible local coordinator transition containing all three of:
//!
//! 1. the terminal `Finished` event;
//! 2. the stored successful operation value; and
//! 3. atomic materialization/publication of the operation's declared outputs.
//!
//! Route execution has more precise terminal states. `SettledSuccess`,
//! `SettledFailure`, and `Skipped` all bind a valid [`ProjectRouteOutcome`]; an
//! unsuccessful process settlement is therefore not confused with an
//! infrastructure `Aborted` event that may have produced no route result.
//! Which graph outputs become materialized for each settlement remains a
//! coordinator responsibility. Appending a terminal event before its
//! corresponding coordinator state transition is not a commit.
//!
//! This local commit discipline does **not** imply exactly-once external
//! effects: a command may have changed a host file, contacted a service, or
//! performed another effect before a crash, cancellation, or retry. Such
//! effects require their own idempotency or recovery protocol.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hgraph::{ExecutableOp, HNodeKind, NodeId, ReadyInputPolicy, ReadySchedule, ValueState};
use crate::ir::PlanNodeId;

use super::model::{
    Artifact, ArtifactCaptureStatus, OExecutionResult, RouteFailureContinuation, RoutePolicy,
};
use super::plan::{ProjectDependency, ProjectHGraph, ProjectPlanOperation};

/// Version of the deterministic project-attempt event vocabulary.
///
/// Version 2 added an execution-context header and distinguished route
/// settlement from coordinator aborts. Version 3 added checked ordered-branch
/// decision evidence. Version 4 binds the trace to canonical
/// `LogicalHGraphV1` schema bytes instead of human inspection text. Version 5
/// also binds the canonical hosted-unbound `DeploymentPlanV1`; trusted replay
/// rejects substitution of that artifact. Version 6 distinguishes retained
/// child-output prefixes from the complete drained streams by binding observed
/// and retained lengths, truncation, full-stream digests, and declared-artifact
/// completeness. It does not bind or execute a snapshot-derived provider
/// proposal or attach World identity.
pub const PROJECT_ATTEMPT_TRACE_VERSION: u32 = 6;

pub(crate) fn project_logical_graph_digest(
    project: &ProjectHGraph,
) -> Result<String, ProjectTraceError> {
    let logical = project.logical_v1().map_err(|error| {
        ProjectTraceError::InvalidMetadata(format!(
            "failed to construct canonical LogicalHGraphV1: {error}"
        ))
    })?;
    let digest = logical.digest().map_err(|error| {
        ProjectTraceError::InvalidMetadata(format!(
            "failed to digest canonical LogicalHGraphV1: {error}"
        ))
    })?;
    Ok(digest.as_sha256().to_string())
}

pub(crate) fn project_hosted_deployment_digest(
    project: &ProjectHGraph,
) -> Result<String, ProjectTraceError> {
    let logical = project.logical_v1().map_err(|error| {
        ProjectTraceError::InvalidMetadata(format!(
            "failed to construct canonical LogicalHGraphV1: {error}"
        ))
    })?;
    let deployment = super::deployment::DeploymentPlanV1::hosted(&logical).map_err(|error| {
        ProjectTraceError::InvalidMetadata(format!(
            "failed to construct canonical hosted DeploymentPlanV1: {error}"
        ))
    })?;
    project_deployment_digest(&deployment)
}

pub(crate) fn project_deployment_digest(
    deployment: &super::deployment::DeploymentPlanV1,
) -> Result<String, ProjectTraceError> {
    let digest = deployment.digest().map_err(|error| {
        ProjectTraceError::InvalidMetadata(format!(
            "failed to digest canonical DeploymentPlanV1: {error}"
        ))
    })?;
    Ok(digest.as_sha256().to_string())
}

/// Context binding shared by every event in one project execution attempt.
///
/// The logical graph digest is stable for the same validated graph. The
/// execution attempt identifier is deliberately separate and must identify
/// this particular invocation, so repeated executions of one graph need not
/// reuse an identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAttemptTraceHeader {
    pub project_name: String,
    pub bundle_digest: String,
    pub target: String,
    pub policy: String,
    pub logical_graph_schema: u16,
    pub logical_graph_digest: String,
    pub deployment_plan_schema: u16,
    pub deployment_plan_digest: String,
    pub execution_attempt_id: String,
}

impl ProjectAttemptTraceHeader {
    // These fields form one fixed execution-context header. Keeping a single
    // constructor prevents callers from assembling partially bound metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_name: impl Into<String>,
        bundle_digest: impl Into<String>,
        target: impl Into<String>,
        policy: impl Into<String>,
        logical_graph_schema: u16,
        logical_graph_digest: impl Into<String>,
        deployment_plan_schema: u16,
        deployment_plan_digest: impl Into<String>,
        execution_attempt_id: impl Into<String>,
    ) -> Self {
        Self {
            project_name: project_name.into(),
            bundle_digest: bundle_digest.into(),
            target: target.into(),
            policy: policy.into(),
            logical_graph_schema,
            logical_graph_digest: logical_graph_digest.into(),
            deployment_plan_schema,
            deployment_plan_digest: deployment_plan_digest.into(),
            execution_attempt_id: execution_attempt_id.into(),
        }
    }

    fn validate(&self) -> Result<(), ProjectTraceError> {
        validate_label(&self.project_name, "project name")?;
        validate_metadata_sha256(&self.bundle_digest, "bundle digest")?;
        validate_label(&self.target, "selection target")?;
        validate_label(&self.policy, "route policy")?;
        if self.logical_graph_schema != super::logical::LOGICAL_HGRAPH_SCHEMA_V1 {
            return Err(ProjectTraceError::InvalidMetadata(format!(
                "logical graph schema must be {}, got {}",
                super::logical::LOGICAL_HGRAPH_SCHEMA_V1,
                self.logical_graph_schema
            )));
        }
        validate_metadata_sha256(&self.logical_graph_digest, "logical graph digest")?;
        if self.deployment_plan_schema != super::deployment::DEPLOYMENT_PLAN_SCHEMA_V1 {
            return Err(ProjectTraceError::InvalidMetadata(format!(
                "deployment plan schema must be {}, got {}",
                super::deployment::DEPLOYMENT_PLAN_SCHEMA_V1,
                self.deployment_plan_schema
            )));
        }
        validate_metadata_sha256(&self.deployment_plan_digest, "deployment plan digest")?;
        validate_label(&self.execution_attempt_id, "execution attempt id")?;
        Ok(())
    }
}

/// One operation's stable identity as seen by the project coordinator.
///
/// Repeating this identity in every lifecycle event makes a standalone event
/// self-describing. [`ProjectAttemptTrace`] verifies that the identity remains
/// byte-for-byte stable for the whole attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAttemptIdentity {
    /// Planner-local operation identity.
    #[serde(with = "plan_node_id_serde")]
    pub plan_node: PlanNodeId,
    /// Canonical operation label, such as `run-route:check`.
    pub operation_label: String,
    /// Selected alternative/workspace branch, when the operation has one.
    pub branch: Option<usize>,
    /// Route identifier for route-specific build/run operations.
    pub route_id: Option<String>,
}

impl ProjectAttemptIdentity {
    /// Construct an identity from explicit coordinator metadata.
    ///
    /// Labels are checked when the event is recorded, so construction remains
    /// convenient for coordinator code that already owns validated plan data.
    pub fn new(
        plan_node: PlanNodeId,
        operation_label: impl Into<String>,
        branch: Option<usize>,
        route_id: Option<String>,
    ) -> Self {
        Self {
            plan_node,
            operation_label: operation_label.into(),
            branch,
            route_id,
        }
    }

    /// Derive the canonical trace identity for a validated project operation.
    pub fn from_operation(operation: &ProjectPlanOperation) -> Result<Self, ProjectTraceError> {
        let (operation_label, route_id) = match &operation.op {
            ExecutableOp::MaterializeProject => ("materialize-project".to_string(), None),
            ExecutableOp::BuildRoute { route_id } => {
                (format!("build-route:{route_id}"), Some(route_id.clone()))
            }
            ExecutableOp::RunRoute { route_id } => {
                (format!("run-route:{route_id}"), Some(route_id.clone()))
            }
            ExecutableOp::SelectRoute { policy } => (format!("select-route:{policy}"), None),
            ExecutableOp::CompareRouteResults => ("compare-route-results".to_string(), None),
            other => {
                return Err(ProjectTraceError::InvalidMetadata(format!(
                    "plan node {} is not a project operation: {other:?}",
                    operation.id.0
                )))
            }
        };
        let identity = Self::new(operation.id, operation_label, operation.branch, route_id);
        identity.validate()?;
        Ok(identity)
    }

    fn validate(&self) -> Result<(), ProjectTraceError> {
        validate_label(&self.operation_label, "operation label")?;
        if let Some(route_id) = &self.route_id {
            validate_label(route_id, "route id")?;
        }
        Ok(())
    }
}

/// SHA-256 identity for one materialized route artifact.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectArtifactFingerprint {
    /// Path relative to the route's isolated execution workspace.
    pub path: String,
    /// Lowercase hexadecimal SHA-256 of the artifact bytes.
    pub sha256: String,
    /// Exact artifact length, retained to distinguish metadata disagreements.
    pub bytes_len: u64,
}

impl ProjectArtifactFingerprint {
    fn from_artifact(artifact: &Artifact) -> Result<Self, ProjectTraceError> {
        validate_artifact_path(&artifact.path)?;
        validate_sha256(&artifact.content_hash, "artifact content hash")?;
        Ok(Self {
            path: artifact.path.clone(),
            sha256: artifact.content_hash.clone(),
            bytes_len: artifact.bytes_len,
        })
    }
}

/// Content-normalized route result retained in a terminal attempt event.
///
/// Volatile provenance (temporary workspace paths, process duration, and raw
/// output bytes) is deliberately absent. Artifacts are sorted by
/// `(path, sha256, bytes_len)` so filesystem/glob discovery order cannot alter
/// the trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRouteOutcome {
    /// Process exit code, or `None` when no process status was available.
    pub exit_code: Option<i32>,
    /// Lowercase hexadecimal SHA-256 of the complete drained stdout stream.
    pub stdout_sha256: String,
    /// Total stdout bytes observed, including any discarded suffix.
    pub stdout_total_observed_bytes: u64,
    /// Number of stdout bytes retained on the execution result.
    pub stdout_retained_bytes: u64,
    /// Whether the retained stdout is only a bounded prefix.
    pub stdout_truncated: bool,
    /// Lowercase hexadecimal SHA-256 of the complete drained stderr stream.
    pub stderr_sha256: String,
    /// Total stderr bytes observed, including any discarded suffix.
    pub stderr_total_observed_bytes: u64,
    /// Number of stderr bytes retained on the execution result.
    pub stderr_retained_bytes: u64,
    /// Whether the retained stderr is only a bounded prefix.
    pub stderr_truncated: bool,
    /// Deterministically ordered fingerprints of declared output artifacts.
    pub artifacts: Vec<ProjectArtifactFingerprint>,
    /// Declared output patterns bound to this capture attempt.
    pub artifact_requirements: Vec<String>,
    /// Whether every declared output artifact was captured completely.
    pub artifact_capture: ArtifactCaptureStatus,
}

impl ProjectRouteOutcome {
    /// Normalize an execution result into deterministic, content-addressed
    /// trace data.
    pub fn from_result(result: &OExecutionResult) -> Result<Self, ProjectTraceError> {
        result
            .stdout_capture
            .validate_for_retained(&result.stdout)
            .map_err(ProjectTraceError::InvalidOutcome)?;
        result
            .stderr_capture
            .validate_for_retained(&result.stderr)
            .map_err(ProjectTraceError::InvalidOutcome)?;
        result
            .artifact_capture
            .validate()
            .map_err(ProjectTraceError::InvalidOutcome)?;
        if result.exit_code == Some(0) && !result.artifact_capture.is_complete() {
            return Err(ProjectTraceError::InvalidOutcome(
                "exit-zero route outcome has incomplete artifact evidence".to_string(),
            ));
        }
        if !result.artifact_capture.is_complete() && !result.artifacts.is_empty() {
            return Err(ProjectTraceError::InvalidOutcome(
                "incomplete artifact evidence retains apparently complete artifacts".to_string(),
            ));
        }
        let mut artifacts = result
            .artifacts
            .iter()
            .map(ProjectArtifactFingerprint::from_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        artifacts.sort();
        if artifacts
            .windows(2)
            .any(|pair| pair[0].path == pair[1].path)
        {
            return Err(ProjectTraceError::InvalidOutcome(
                "route outcome contains a duplicate artifact path".to_string(),
            ));
        }
        let outcome = Self {
            exit_code: result.exit_code,
            stdout_sha256: result.stdout_capture.sha256.clone(),
            stdout_total_observed_bytes: result.stdout_capture.total_observed_bytes,
            stdout_retained_bytes: result.stdout_capture.retained_bytes,
            stdout_truncated: result.stdout_capture.truncated,
            stderr_sha256: result.stderr_capture.sha256.clone(),
            stderr_total_observed_bytes: result.stderr_capture.total_observed_bytes,
            stderr_retained_bytes: result.stderr_capture.retained_bytes,
            stderr_truncated: result.stderr_capture.truncated,
            artifacts,
            artifact_requirements: result.artifact_requirements.clone(),
            artifact_capture: result.artifact_capture.clone(),
        };
        outcome.validate()?;
        Ok(outcome)
    }

    fn validate(&self) -> Result<(), ProjectTraceError> {
        validate_stream_fingerprint(
            &self.stdout_sha256,
            self.stdout_total_observed_bytes,
            self.stdout_retained_bytes,
            self.stdout_truncated,
            "stdout",
        )?;
        validate_stream_fingerprint(
            &self.stderr_sha256,
            self.stderr_total_observed_bytes,
            self.stderr_retained_bytes,
            self.stderr_truncated,
            "stderr",
        )?;
        self.artifact_capture
            .validate()
            .map_err(ProjectTraceError::InvalidOutcome)?;
        if self.exit_code == Some(0) && !self.artifact_capture.is_complete() {
            return Err(ProjectTraceError::InvalidOutcome(
                "exit-zero route outcome has incomplete artifact evidence".to_string(),
            ));
        }
        if !self.artifact_capture.is_complete() && !self.artifacts.is_empty() {
            return Err(ProjectTraceError::InvalidOutcome(
                "incomplete artifact evidence retains apparently complete artifacts".to_string(),
            ));
        }
        for requirement in &self.artifact_requirements {
            if requirement.is_empty() || requirement.contains('\0') {
                return Err(ProjectTraceError::InvalidOutcome(
                    "artifact requirement must be nonempty and contain no NUL".to_string(),
                ));
            }
        }
        let mut prior: Option<&ProjectArtifactFingerprint> = None;
        for artifact in &self.artifacts {
            validate_artifact_path(&artifact.path)?;
            validate_sha256(&artifact.sha256, "artifact fingerprint")?;
            if let Some(previous) = prior {
                if previous >= artifact {
                    let detail = if previous.path == artifact.path {
                        "duplicate artifact path"
                    } else {
                        "artifacts are not in canonical order"
                    };
                    return Err(ProjectTraceError::InvalidOutcome(detail.to_string()));
                }
            }
            prior = Some(artifact);
        }
        if self.artifact_capture.is_complete() {
            for requirement in &self.artifact_requirements {
                if !self
                    .artifacts
                    .iter()
                    .any(|artifact| super::runtime::glob_match(requirement, &artifact.path))
                {
                    return Err(ProjectTraceError::InvalidOutcome(format!(
                        "complete artifact evidence has no fingerprint matching `{requirement}`"
                    )));
                }
            }
            for artifact in &self.artifacts {
                if !self
                    .artifact_requirements
                    .iter()
                    .any(|requirement| super::runtime::glob_match(requirement, &artifact.path))
                {
                    return Err(ProjectTraceError::InvalidOutcome(format!(
                        "artifact fingerprint `{}` matches no declared requirement",
                        artifact.path
                    )));
                }
            }
        }
        Ok(())
    }
}

fn validate_artifact_path(path: &str) -> Result<(), ProjectTraceError> {
    if path.is_empty() || path.contains('\0') {
        return Err(ProjectTraceError::InvalidOutcome(
            "artifact path must be nonempty and contain no NUL".to_string(),
        ));
    }
    if path.starts_with('/')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(ProjectTraceError::InvalidOutcome(
            "artifact path must be a canonical relative workspace path".to_string(),
        ));
    }
    Ok(())
}

fn validate_stream_fingerprint(
    sha256: &str,
    total_observed_bytes: u64,
    retained_bytes: u64,
    truncated: bool,
    stream: &str,
) -> Result<(), ProjectTraceError> {
    validate_sha256(sha256, &format!("{stream} fingerprint"))?;
    if total_observed_bytes < retained_bytes {
        return Err(ProjectTraceError::InvalidOutcome(format!(
            "{stream} observed fewer bytes than it retained"
        )));
    }
    if truncated != (total_observed_bytes > retained_bytes) {
        return Err(ProjectTraceError::InvalidOutcome(format!(
            "{stream} truncation flag disagrees with observed and retained lengths"
        )));
    }
    if total_observed_bytes == 0 && sha256 != sha256_hex(&[]) {
        return Err(ProjectTraceError::InvalidOutcome(format!(
            "empty {stream} stream has a nonempty-content fingerprint"
        )));
    }
    Ok(())
}

impl TryFrom<&OExecutionResult> for ProjectRouteOutcome {
    type Error = ProjectTraceError;

    fn try_from(result: &OExecutionResult) -> Result<Self, Self::Error> {
        Self::from_result(result)
    }
}

/// Evidence class used by the hosted coordinator to decide whether another
/// ordered alternative may start after an unsuccessful branch.
///
/// `DeclaredIdempotent` remains an author declaration bound by the bundle
/// digest. It is not an independently verified sandbox, fence, or effect log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectContinuationEvidence {
    /// Every assessed route was guard-skipped, so no child process started.
    NoExecution,
    /// Every child process that started carried a bundle-bound
    /// `declared_idempotent` continuation contract.
    DeclaredIdempotent,
    /// At least one child process started without a safe continuation contract.
    UnprovenEffects,
}

/// One checked admission/denial for the next ordered route alternative.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectContinuationDecision {
    /// Route that would become the next selected alternative.
    pub next_route_id: String,
    /// Routes whose completed executions were considered, in coordinator order.
    pub assessed_route_ids: Vec<String>,
    pub evidence: ProjectContinuationEvidence,
    pub admitted: bool,
}

impl ProjectContinuationDecision {
    pub fn new(
        next_route_id: impl Into<String>,
        assessed_route_ids: Vec<String>,
        evidence: ProjectContinuationEvidence,
    ) -> Result<Self, ProjectTraceError> {
        let decision = Self {
            next_route_id: next_route_id.into(),
            assessed_route_ids,
            admitted: !matches!(evidence, ProjectContinuationEvidence::UnprovenEffects),
            evidence,
        };
        decision.validate()?;
        Ok(decision)
    }

    fn validate(&self) -> Result<(), ProjectTraceError> {
        validate_label(&self.next_route_id, "next route id")?;
        if self.assessed_route_ids.is_empty() {
            return Err(ProjectTraceError::InvalidEvent(
                "continuation decision has no assessed routes".to_string(),
            ));
        }
        let mut prior = BTreeMap::<&str, ()>::new();
        for route_id in &self.assessed_route_ids {
            validate_label(route_id, "assessed route id")?;
            if prior.insert(route_id.as_str(), ()).is_some() {
                return Err(ProjectTraceError::InvalidEvent(
                    "continuation decision repeats an assessed route".to_string(),
                ));
            }
        }
        let expected_admitted =
            !matches!(self.evidence, ProjectContinuationEvidence::UnprovenEffects);
        if self.admitted != expected_admitted {
            return Err(ProjectTraceError::InvalidEvent(
                "continuation admission disagrees with its evidence class".to_string(),
            ));
        }
        Ok(())
    }
}

/// Lifecycle state of one project-plan operation attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAttemptState {
    Ready,
    Started,
    /// Successful terminal state for a non-route project operation.
    Finished,
    /// A route produced a successful execution result.
    SettledSuccess,
    /// A route produced a valid execution result with an unsuccessful status.
    SettledFailure,
    /// A route guard produced a valid skip result.
    Skipped,
    /// The coordinator could not complete the operation. A route outcome is
    /// absent unless execution had genuinely produced one before the abort.
    Aborted,
}

impl ProjectAttemptState {
    /// True only for the states that participate in the coordinator's local
    /// commit/linearization point.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished
                | Self::SettledSuccess
                | Self::SettledFailure
                | Self::Skipped
                | Self::Aborted
        )
    }
}

/// One self-describing deterministic coordinator lifecycle event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAttemptEvent {
    /// Unique, contiguous lifecycle-event sequence number assigned by the
    /// coordinator trace, beginning at zero.
    pub coordinator_ordinal: u64,
    #[serde(with = "plan_node_id_serde")]
    pub plan_node: PlanNodeId,
    pub operation_label: String,
    pub branch: Option<usize>,
    pub route_id: Option<String>,
    pub state: ProjectAttemptState,
    /// Present for settled/skipped `RunRoute` events and only for an `Aborted`
    /// event when a valid route result was genuinely available. Absent for
    /// other project operations and all nonterminal lifecycle events.
    pub outcome: Option<ProjectRouteOutcome>,
    /// SHA-256 of the coordinator's normalized failure description. Raw error
    /// text is not retained because host paths and tool diagnostics are often
    /// nondeterministic.
    pub failure_sha256: Option<String>,
    /// Present only on the unsuccessful terminal route event that decides
    /// whether the next ordered alternative may start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ProjectContinuationDecision>,
}

impl ProjectAttemptEvent {
    fn new(
        coordinator_ordinal: u64,
        identity: &ProjectAttemptIdentity,
        state: ProjectAttemptState,
        outcome: Option<ProjectRouteOutcome>,
        failure_sha256: Option<String>,
        continuation: Option<ProjectContinuationDecision>,
    ) -> Self {
        Self {
            coordinator_ordinal,
            plan_node: identity.plan_node,
            operation_label: identity.operation_label.clone(),
            branch: identity.branch,
            route_id: identity.route_id.clone(),
            state,
            outcome,
            failure_sha256,
            continuation,
        }
    }

    /// Recover the identity repeated by this event.
    pub fn identity(&self) -> ProjectAttemptIdentity {
        ProjectAttemptIdentity::new(
            self.plan_node,
            self.operation_label.clone(),
            self.branch,
            self.route_id.clone(),
        )
    }

    fn validate(&self) -> Result<(), ProjectTraceError> {
        self.identity().validate()?;
        if let Some(outcome) = &self.outcome {
            outcome.validate()?;
        }
        if let Some(continuation) = &self.continuation {
            continuation.validate()?;
        }
        match self.state {
            ProjectAttemptState::Ready | ProjectAttemptState::Started => {
                if self.outcome.is_some()
                    || self.failure_sha256.is_some()
                    || self.continuation.is_some()
                {
                    return Err(ProjectTraceError::InvalidEvent(
                        "nonterminal attempt event carries terminal data".to_string(),
                    ));
                }
            }
            ProjectAttemptState::Finished => {
                if self.is_run_route() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "route attempt uses the non-route Finished state".to_string(),
                    ));
                }
                if self.outcome.is_some()
                    || self.failure_sha256.is_some()
                    || self.continuation.is_some()
                {
                    return Err(ProjectTraceError::InvalidEvent(
                        "finished non-route event carries route or failure data".to_string(),
                    ));
                }
            }
            ProjectAttemptState::SettledSuccess
            | ProjectAttemptState::SettledFailure
            | ProjectAttemptState::Skipped => {
                if !self.is_run_route() || self.route_id.is_none() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "route settlement event does not identify a RunRoute operation".to_string(),
                    ));
                }
                if self.outcome.is_none() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "route settlement event lacks a route outcome".to_string(),
                    ));
                }
                if self.failure_sha256.is_some() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "route settlement event carries an abort fingerprint".to_string(),
                    ));
                }
                if self.state == ProjectAttemptState::SettledSuccess && self.continuation.is_some()
                {
                    return Err(ProjectTraceError::InvalidEvent(
                        "successful route settlement carries a continuation decision".to_string(),
                    ));
                }
                if self.continuation.is_some() && self.branch.is_none() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "continuation decision is not bound to an alternative branch".to_string(),
                    ));
                }
                let exit_code = self
                    .outcome
                    .as_ref()
                    .expect("route settlement outcome presence was checked")
                    .exit_code;
                match self.state {
                    ProjectAttemptState::SettledSuccess if exit_code != Some(0) => {
                        return Err(ProjectTraceError::InvalidEvent(
                            "successful route settlement does not have exit code 0".to_string(),
                        ));
                    }
                    ProjectAttemptState::SettledFailure if exit_code == Some(0) => {
                        return Err(ProjectTraceError::InvalidEvent(
                            "unsuccessful route settlement has exit code 0".to_string(),
                        ));
                    }
                    ProjectAttemptState::Skipped if exit_code.is_some() => {
                        return Err(ProjectTraceError::InvalidEvent(
                            "skipped route settlement carries a process exit code".to_string(),
                        ));
                    }
                    _ => {}
                }
                if self.state == ProjectAttemptState::Skipped {
                    let outcome = self
                        .outcome
                        .as_ref()
                        .expect("route settlement outcome presence was checked");
                    if !outcome.artifacts.is_empty() {
                        return Err(ProjectTraceError::InvalidEvent(
                            "skipped route settlement carries captured artifacts".to_string(),
                        ));
                    }
                    if outcome.stdout_total_observed_bytes != 0
                        || outcome.stdout_retained_bytes != 0
                        || outcome.stdout_truncated
                        || outcome.stdout_sha256 != sha256_hex(&[])
                    {
                        return Err(ProjectTraceError::InvalidEvent(
                            "skipped route settlement must have an empty complete stdout stream"
                                .to_string(),
                        ));
                    }
                    if outcome.stderr_total_observed_bytes == 0
                        || outcome.stderr_retained_bytes != outcome.stderr_total_observed_bytes
                        || outcome.stderr_truncated
                    {
                        return Err(ProjectTraceError::InvalidEvent(
                            "skipped route settlement must have a nonempty complete stderr marker"
                                .to_string(),
                        ));
                    }
                    if outcome.artifact_requirements.is_empty() {
                        if !outcome.artifact_capture.is_complete() {
                            return Err(ProjectTraceError::InvalidEvent(
                                "skipped route without output requirements has incomplete artifact evidence"
                                    .to_string(),
                            ));
                        }
                    } else if !matches!(
                        &outcome.artifact_capture,
                        ArtifactCaptureStatus::Incomplete { failure }
                            if matches!(
                                failure.as_ref(),
                                super::model::ArtifactCaptureFailure::NotAttempted { reason }
                                    if reason == "route_guard_skipped"
                            )
                    ) {
                        return Err(ProjectTraceError::InvalidEvent(
                            "skipped route with output requirements must record route_guard_skipped incomplete evidence"
                                .to_string(),
                        ));
                    }
                }
            }
            ProjectAttemptState::Aborted => {
                let digest = self.failure_sha256.as_deref().ok_or_else(|| {
                    ProjectTraceError::InvalidEvent(
                        "aborted attempt event lacks a failure fingerprint".to_string(),
                    )
                })?;
                validate_sha256(digest, "failure fingerprint")?;
                if self.outcome.is_some() && (!self.is_run_route() || self.route_id.is_none()) {
                    return Err(ProjectTraceError::InvalidEvent(
                        "non-route abort carries a route outcome".to_string(),
                    ));
                }
                if self.continuation.is_some() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "aborted attempt carries a continuation decision".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn is_run_route(&self) -> bool {
        self.operation_label.starts_with("run-route:")
    }
}

/// Checked deterministic lifecycle history for project-plan attempts.
#[derive(Clone, Debug, Serialize)]
pub struct ProjectAttemptTrace {
    format_version: u32,
    header: ProjectAttemptTraceHeader,
    events: Vec<ProjectAttemptEvent>,
    #[serde(skip)]
    attempts: BTreeMap<PlanNodeId, (ProjectAttemptIdentity, ProjectAttemptState)>,
}

impl ProjectAttemptTrace {
    /// Start an empty checked trace bound to one validated execution context.
    pub fn new(header: ProjectAttemptTraceHeader) -> Result<Self, ProjectTraceError> {
        header.validate()?;
        Ok(Self {
            format_version: PROJECT_ATTEMPT_TRACE_VERSION,
            header,
            events: Vec::new(),
            attempts: BTreeMap::new(),
        })
    }

    /// Rebuild a trace while checking event-local metadata, ordinals, lifecycle
    /// transitions, and self-contained continuation inventory.
    ///
    /// This structural replay has no trusted project plan, so it cannot prove
    /// that a continuation names the planned next branch or that its evidence
    /// matches bundle-bound route contracts. Use [`Self::try_from_project_events`]
    /// when those semantic checks are required.
    pub fn try_from_events(
        header: ProjectAttemptTraceHeader,
        events: impl IntoIterator<Item = ProjectAttemptEvent>,
    ) -> Result<Self, ProjectTraceError> {
        let mut trace = Self::new(header)?;
        for event in events {
            trace.record(event)?;
        }
        Ok(trace)
    }

    /// Rebuild a trace and validate it against one trusted Project HGraph.
    ///
    /// In addition to structural replay, this checks the trace header, exact
    /// plan-operation identities, ordered-branch admission, the exact next
    /// alternative, and continuation evidence recomputed from RoutePlanFacts.
    pub fn try_from_project_events(
        project: &ProjectHGraph,
        header: ProjectAttemptTraceHeader,
        events: impl IntoIterator<Item = ProjectAttemptEvent>,
    ) -> Result<Self, ProjectTraceError> {
        let canonical_graph = project.plan.to_hgraph().map_err(|error| {
            ProjectTraceError::InvalidMetadata(format!("trusted project plan is invalid: {error}"))
        })?;
        if canonical_graph != project.graph {
            return Err(ProjectTraceError::InvalidMetadata(
                "trusted Project HGraph differs from its canonical plan projection".to_string(),
            ));
        }
        let expected_deployment_digest = project_hosted_deployment_digest(project)?;
        validate_project_header(project, &header, &expected_deployment_digest)?;
        let trace = Self::try_from_events(header, events)?;
        trace.validate_project_semantics(project)?;
        Ok(trace)
    }

    /// Rebuild a trace against one trusted Project HGraph and one exact
    /// deployment artifact already admitted by the caller.
    ///
    /// This variant is used by the bounded World-hosted reference path. It
    /// checks the snapshot-derived deployment digest rather than silently
    /// reconstructing the unbound hosted profile. It does not itself establish
    /// deployment authority, snapshot freshness, or provider admission.
    pub fn try_from_project_events_with_deployment(
        project: &ProjectHGraph,
        deployment: &super::deployment::DeploymentPlanV1,
        header: ProjectAttemptTraceHeader,
        events: impl IntoIterator<Item = ProjectAttemptEvent>,
    ) -> Result<Self, ProjectTraceError> {
        let canonical_graph = project.plan.to_hgraph().map_err(|error| {
            ProjectTraceError::InvalidMetadata(format!("trusted project plan is invalid: {error}"))
        })?;
        if canonical_graph != project.graph {
            return Err(ProjectTraceError::InvalidMetadata(
                "trusted Project HGraph differs from its canonical plan projection".to_string(),
            ));
        }
        let logical = project.logical_v1().map_err(|error| {
            ProjectTraceError::InvalidMetadata(format!(
                "failed to derive trusted logical HGraph for deployment replay: {error}"
            ))
        })?;
        let logical_digest = logical.digest().map_err(|error| {
            ProjectTraceError::InvalidMetadata(format!(
                "failed to digest trusted logical HGraph for deployment replay: {error}"
            ))
        })?;
        if deployment.logical_hgraph_schema != super::logical::LOGICAL_HGRAPH_SCHEMA_V1
            || deployment.logical_hgraph != logical_digest
        {
            return Err(ProjectTraceError::InvalidMetadata(
                "deployment replay artifact differs from the trusted logical HGraph".to_string(),
            ));
        }
        let expected_deployment_digest = project_deployment_digest(deployment)?;
        validate_project_header(project, &header, &expected_deployment_digest)?;
        let trace = Self::try_from_events(header, events)?;
        trace.validate_project_semantics(project)?;
        Ok(trace)
    }

    /// Execution context to which every event in this trace is bound.
    pub fn header(&self) -> &ProjectAttemptTraceHeader {
        &self.header
    }

    /// Immutable persisted format version selected by the checked constructor.
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Read-only event order emitted by the coordinator.
    pub fn events(&self) -> &[ProjectAttemptEvent] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Last recorded lifecycle state for a plan node.
    pub fn state(&self, plan_node: PlanNodeId) -> Option<ProjectAttemptState> {
        self.attempts.get(&plan_node).map(|(_, state)| *state)
    }

    /// Consume the trace without exposing its mutable validation indexes.
    pub fn into_events(self) -> Vec<ProjectAttemptEvent> {
        self.events
    }

    pub fn record_ready(
        &mut self,
        identity: &ProjectAttemptIdentity,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(identity, ProjectAttemptState::Ready, None, None, None)
    }

    pub fn record_started(
        &mut self,
        identity: &ProjectAttemptIdentity,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(identity, ProjectAttemptState::Started, None, None, None)
    }

    /// Record successful completion of a non-route project operation.
    pub fn record_finished(
        &mut self,
        identity: &ProjectAttemptIdentity,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(identity, ProjectAttemptState::Finished, None, None, None)
    }

    /// Record a route result whose process status is successful.
    pub fn record_settled_success(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: ProjectRouteOutcome,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(
            identity,
            ProjectAttemptState::SettledSuccess,
            Some(outcome),
            None,
            None,
        )
    }

    /// Record a route result whose process status is unsuccessful.
    pub fn record_settled_failure(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: ProjectRouteOutcome,
    ) -> Result<(), ProjectTraceError> {
        self.record_settled_failure_with_continuation(identity, outcome, None)
    }

    /// Record an unsuccessful route settlement together with the checked
    /// admission/denial governing the next ordered alternative.
    pub fn record_settled_failure_with_continuation(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: ProjectRouteOutcome,
        continuation: Option<ProjectContinuationDecision>,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(
            identity,
            ProjectAttemptState::SettledFailure,
            Some(outcome),
            None,
            continuation,
        )
    }

    /// Record a valid route result produced by guard-skip semantics.
    pub fn record_skipped(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: ProjectRouteOutcome,
    ) -> Result<(), ProjectTraceError> {
        self.record_skipped_with_continuation(identity, outcome, None)
    }

    /// Record a guard-skipped route result and any ordered continuation that
    /// this no-execution settlement admits.
    pub fn record_skipped_with_continuation(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: ProjectRouteOutcome,
        continuation: Option<ProjectContinuationDecision>,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(
            identity,
            ProjectAttemptState::Skipped,
            Some(outcome),
            None,
            continuation,
        )
    }

    /// Record an infrastructure/coordinator abort.
    ///
    /// Raw diagnostics are not retained; their normalized bytes are
    /// SHA-256-bound. `outcome` must be `None` unless route execution genuinely
    /// produced a valid result before the abort.
    pub fn record_aborted(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: Option<ProjectRouteOutcome>,
        normalized_failure: impl AsRef<[u8]>,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(
            identity,
            ProjectAttemptState::Aborted,
            outcome,
            Some(sha256_hex(normalized_failure.as_ref())),
            None,
        )
    }

    /// Replay one externally constructed event after checking its metadata,
    /// contiguous ordinal, and exact `Ready -> Started -> terminal` lifecycle.
    ///
    /// Normal coordinator code should use the `record_*` methods, which assign
    /// the ordinal internally.
    pub fn record(&mut self, event: ProjectAttemptEvent) -> Result<(), ProjectTraceError> {
        event.validate()?;
        let expected_ordinal = self.next_ordinal()?;
        if event.coordinator_ordinal != expected_ordinal {
            return Err(ProjectTraceError::NoncontiguousOrdinal {
                expected: expected_ordinal,
                actual: event.coordinator_ordinal,
            });
        }
        self.validate_continuation_history(&event)?;
        let identity = event.identity();

        match self.attempts.get(&event.plan_node) {
            None => {
                if event.state != ProjectAttemptState::Ready {
                    return Err(ProjectTraceError::InvalidTransition {
                        plan_node: event.plan_node,
                        from: None,
                        to: event.state,
                    });
                }
            }
            Some((existing_identity, prior_state)) => {
                if existing_identity != &identity {
                    return Err(ProjectTraceError::MetadataChanged(event.plan_node));
                }
                let valid = matches!(
                    (*prior_state, event.state),
                    (ProjectAttemptState::Ready, ProjectAttemptState::Started)
                        | (
                            ProjectAttemptState::Started,
                            ProjectAttemptState::Finished
                                | ProjectAttemptState::SettledSuccess
                                | ProjectAttemptState::SettledFailure
                                | ProjectAttemptState::Skipped
                                | ProjectAttemptState::Aborted
                        )
                );
                if !valid {
                    return Err(ProjectTraceError::InvalidTransition {
                        plan_node: event.plan_node,
                        from: Some(*prior_state),
                        to: event.state,
                    });
                }
            }
        }

        self.attempts
            .insert(event.plan_node, (identity, event.state));
        self.events.push(event);
        Ok(())
    }

    fn validate_continuation_history(
        &self,
        event: &ProjectAttemptEvent,
    ) -> Result<(), ProjectTraceError> {
        let Some(decision) = &event.continuation else {
            return Ok(());
        };
        let branch = event.branch.ok_or_else(|| {
            ProjectTraceError::InvalidEvent(
                "continuation decision is not bound to an alternative branch".to_string(),
            )
        })?;
        let mut terminal_events = self
            .events
            .iter()
            .filter(|prior| {
                prior.branch == Some(branch)
                    && prior.route_id.is_some()
                    && matches!(
                        prior.state,
                        ProjectAttemptState::SettledSuccess
                            | ProjectAttemptState::SettledFailure
                            | ProjectAttemptState::Skipped
                    )
            })
            .collect::<Vec<_>>();
        terminal_events.push(event);
        let observed = terminal_events
            .iter()
            .map(|terminal| {
                terminal
                    .route_id
                    .clone()
                    .expect("filtered continuation event identifies a route")
            })
            .collect::<Vec<_>>();
        if observed != decision.assessed_route_ids {
            return Err(ProjectTraceError::InvalidEvent(
                "continuation decision route inventory disagrees with terminal branch events"
                    .to_string(),
            ));
        }
        let any_executed = terminal_events
            .iter()
            .any(|terminal| terminal.state != ProjectAttemptState::Skipped);
        match decision.evidence {
            ProjectContinuationEvidence::NoExecution if any_executed => {
                return Err(ProjectTraceError::InvalidEvent(
                    "no-execution continuation includes an executed route".to_string(),
                ));
            }
            ProjectContinuationEvidence::DeclaredIdempotent
            | ProjectContinuationEvidence::UnprovenEffects
                if !any_executed =>
            {
                return Err(ProjectTraceError::InvalidEvent(
                    "executed-route continuation evidence has no executed route".to_string(),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_project_semantics(&self, project: &ProjectHGraph) -> Result<(), ProjectTraceError> {
        validate_observed_readiness(project, &self.events)?;
        for event in &self.events {
            let operation = project
                .plan
                .operations
                .get(event.plan_node.0)
                .filter(|operation| operation.id == event.plan_node)
                .ok_or_else(|| {
                    ProjectTraceError::InvalidMetadata(format!(
                        "trace references missing project plan node {}",
                        event.plan_node.0
                    ))
                })?;
            let expected = ProjectAttemptIdentity::from_operation(operation)?;
            if event.identity() != expected {
                return Err(ProjectTraceError::InvalidMetadata(format!(
                    "trace identity for plan node {} differs from the trusted project plan",
                    event.plan_node.0
                )));
            }
            if matches!(operation.op, ExecutableOp::RunRoute { .. }) {
                if let Some(outcome) = &event.outcome {
                    let route_facts = operation.route_facts.as_ref().ok_or_else(|| {
                        ProjectTraceError::InvalidMetadata(format!(
                            "RunRoute plan node {} has no trusted RoutePlanFacts",
                            event.plan_node.0
                        ))
                    })?;
                    if outcome.artifact_requirements != route_facts.outputs {
                        return Err(ProjectTraceError::InvalidEvent(format!(
                            "route outcome artifact requirements for plan node {} differ from trusted RoutePlanFacts",
                            event.plan_node.0
                        )));
                    }
                }
            }
        }

        let ordered = matches!(
            project.plan.policy,
            RoutePolicy::Fallback | RoutePolicy::AnySuccess
        );
        if !ordered {
            if self.events.iter().any(|event| event.continuation.is_some()) {
                return Err(ProjectTraceError::InvalidEvent(
                    "non-ordered project trace carries a continuation decision".to_string(),
                ));
            }
            return validate_completed_selection(project, &self.events);
        }

        let mut admitted_branches = BTreeSet::from([0_usize]);
        let mut highest_observed_branch = 0_usize;
        for (event_index, event) in self.events.iter().enumerate() {
            let Some(branch) = event.branch else {
                continue;
            };
            if branch >= project.plan.alternatives.len() {
                return Err(ProjectTraceError::InvalidEvent(format!(
                    "trace references out-of-range alternative branch {branch}"
                )));
            }
            if !admitted_branches.contains(&branch) {
                return Err(ProjectTraceError::InvalidEvent(format!(
                    "alternative branch {branch} emitted an event before its continuation was admitted"
                )));
            }
            if branch < highest_observed_branch {
                return Err(ProjectTraceError::InvalidEvent(format!(
                    "alternative branch {branch} emitted an event after branch {highest_observed_branch} started"
                )));
            }
            highest_observed_branch = highest_observed_branch.max(branch);

            let is_terminal_alternative = event
                .route_id
                .as_ref()
                .is_some_and(|route_id| project.plan.alternatives.get(branch) == Some(route_id));
            let settled_unsuccessfully = matches!(
                event.state,
                ProjectAttemptState::SettledFailure | ProjectAttemptState::Skipped
            );
            let has_next_alternative = branch + 1 < project.plan.alternatives.len();
            let decision_required =
                is_terminal_alternative && settled_unsuccessfully && has_next_alternative;

            match (&event.continuation, decision_required) {
                (None, true) => {
                    return Err(ProjectTraceError::InvalidEvent(format!(
                        "unsuccessful terminal alternative on branch {branch} lacks a continuation decision"
                    )));
                }
                (Some(_), false) => {
                    return Err(ProjectTraceError::InvalidEvent(format!(
                        "continuation decision is not attached to an unsuccessful non-final terminal alternative on branch {branch}"
                    )));
                }
                (None, false) => continue,
                (Some(decision), true) => {
                    validate_transitive_prerequisite_coverage(
                        project,
                        &self.events,
                        event.plan_node,
                        &mut BTreeSet::new(),
                    )?;
                    let expected_next = &project.plan.alternatives[branch + 1];
                    if &decision.next_route_id != expected_next {
                        return Err(ProjectTraceError::InvalidEvent(format!(
                            "continuation from branch {branch} names `{}` instead of planned alternative `{expected_next}`",
                            decision.next_route_id
                        )));
                    }
                    let expected_evidence = recompute_continuation_evidence(
                        project,
                        &self.events[..=event_index],
                        branch,
                    )?;
                    if decision.evidence != expected_evidence {
                        return Err(ProjectTraceError::InvalidEvent(format!(
                            "continuation evidence on branch {branch} differs from trusted RoutePlanFacts"
                        )));
                    }
                    if decision.admitted {
                        admitted_branches.insert(branch + 1);
                    }
                }
            }
        }
        validate_completed_selection(project, &self.events)
    }

    fn record_state(
        &mut self,
        identity: &ProjectAttemptIdentity,
        state: ProjectAttemptState,
        outcome: Option<ProjectRouteOutcome>,
        failure_sha256: Option<String>,
        continuation: Option<ProjectContinuationDecision>,
    ) -> Result<(), ProjectTraceError> {
        let coordinator_ordinal = self.next_ordinal()?;
        self.record(ProjectAttemptEvent::new(
            coordinator_ordinal,
            identity,
            state,
            outcome,
            failure_sha256,
            continuation,
        ))
    }

    fn next_ordinal(&self) -> Result<u64, ProjectTraceError> {
        u64::try_from(self.events.len()).map_err(|_| ProjectTraceError::OrdinalOverflow)
    }
}

/// Replay the exact coordinator readiness rule over the observed prefix.
///
/// This checks every graph input, including value, completion, resource-state,
/// and ordered-branch control nodes. A partial coordinator-failure trace need
/// not contain work that never became ready, but every observed `Ready` event
/// must be justified by outputs published by earlier terminal events (or by an
/// initially materialized graph node). This is what lets semantic replay accept
/// honest prefixes without accepting causally impossible ones.
fn validate_observed_readiness(
    project: &ProjectHGraph,
    events: &[ProjectAttemptEvent],
) -> Result<(), ProjectTraceError> {
    let schedule = ReadySchedule::derive(&project.graph).map_err(|error| {
        ProjectTraceError::InvalidMetadata(format!(
            "failed to derive trusted project ReadySchedule: {error}"
        ))
    })?;
    let ready_by_plan = schedule
        .ops
        .iter()
        .map(|ready| (ready.plan_node, ready))
        .collect::<BTreeMap<_, _>>();
    let output_producers = schedule
        .ops
        .iter()
        .flat_map(|ready| {
            ready
                .outputs
                .iter()
                .copied()
                .map(move |output| (output, ready.plan_node))
        })
        .collect::<BTreeMap<_, _>>();
    let mut materialized = project
        .graph
        .nodes
        .iter()
        .filter_map(|(node, value)| (value.state == ValueState::Materialized).then_some(*node))
        .collect::<BTreeSet<_>>();
    let mut terminal_states = BTreeMap::<PlanNodeId, ProjectAttemptState>::new();
    let mut continuation_denied = false;

    for event in events {
        let ready = ready_by_plan
            .get(&event.plan_node)
            .copied()
            .ok_or_else(|| {
                ProjectTraceError::InvalidMetadata(format!(
                    "trace references plan node {} outside the trusted ReadySchedule",
                    event.plan_node.0
                ))
            })?;
        if event.state == ProjectAttemptState::Ready {
            let input_policy = ready.input_policy(&project.graph).map_err(|error| {
                ProjectTraceError::InvalidMetadata(format!(
                    "failed to derive input policy for plan node {}: {error}",
                    event.plan_node.0
                ))
            })?;
            let justified = match input_policy {
                ReadyInputPolicy::All => ready
                    .inputs
                    .iter()
                    .all(|input| materialized.contains(input)),
                ReadyInputPolicy::OrderedFirstSuccess => {
                    continuation_denied
                        || ordered_selection_inputs_ready(
                            ready.inputs.as_slice(),
                            &materialized,
                            &output_producers,
                            &terminal_states,
                        )?
                }
            };
            if !justified {
                return Err(ProjectTraceError::InvalidEvent(format!(
                    "plan node {} became Ready before its trusted graph inputs were published",
                    event.plan_node.0
                )));
            }
        }

        if event.state.is_terminal() {
            for output in &ready.outputs {
                let node = project.graph.node(*output).ok_or_else(|| {
                    ProjectTraceError::InvalidMetadata(format!(
                        "plan node {} names missing graph output N{}",
                        event.plan_node.0, output.0
                    ))
                })?;
                if terminal_publishes(&node.kind, event.state) {
                    materialized.insert(*output);
                }
            }
            terminal_states.insert(event.plan_node, event.state);
            if event
                .continuation
                .as_ref()
                .is_some_and(|decision| !decision.admitted)
            {
                continuation_denied = true;
            }
        }
    }
    Ok(())
}

fn ordered_selection_inputs_ready(
    inputs: &[NodeId],
    materialized: &BTreeSet<NodeId>,
    output_producers: &BTreeMap<NodeId, PlanNodeId>,
    terminal_states: &BTreeMap<PlanNodeId, ProjectAttemptState>,
) -> Result<bool, ProjectTraceError> {
    let mut saw_result = false;
    for input in inputs {
        if !materialized.contains(input) {
            return Ok(false);
        }
        saw_result = true;
        let producer = output_producers.get(input).ok_or_else(|| {
            ProjectTraceError::InvalidMetadata(format!(
                "ordered selection input N{} has no trusted operation producer",
                input.0
            ))
        })?;
        if terminal_states.get(producer) == Some(&ProjectAttemptState::SettledSuccess) {
            return Ok(true);
        }
    }
    Ok(saw_result)
}

fn terminal_publishes(kind: &HNodeKind, state: ProjectAttemptState) -> bool {
    match state {
        ProjectAttemptState::Finished
        | ProjectAttemptState::SettledSuccess
        | ProjectAttemptState::Skipped => true,
        ProjectAttemptState::SettledFailure => {
            matches!(kind, HNodeKind::Value | HNodeKind::ResourceState { .. })
        }
        ProjectAttemptState::Ready
        | ProjectAttemptState::Started
        | ProjectAttemptState::Aborted => false,
    }
}

fn validate_project_header(
    project: &ProjectHGraph,
    header: &ProjectAttemptTraceHeader,
    expected_deployment_digest: &str,
) -> Result<(), ProjectTraceError> {
    header.validate()?;
    let expected_policy = project.plan.policy.token();
    let expected = [
        (
            "project name",
            header.project_name.as_str(),
            project.plan.project_name.as_str(),
        ),
        (
            "bundle digest",
            header.bundle_digest.as_str(),
            project.plan.bundle_digest.as_str(),
        ),
        (
            "selection target",
            header.target.as_str(),
            project.plan.target.as_str(),
        ),
        (
            "route policy",
            header.policy.as_str(),
            expected_policy.as_str(),
        ),
    ];
    for (field, actual, expected) in expected {
        if actual != expected {
            return Err(ProjectTraceError::InvalidMetadata(format!(
                "trace {field} `{actual}` differs from trusted project `{expected}`"
            )));
        }
    }
    if header.logical_graph_schema != super::logical::LOGICAL_HGRAPH_SCHEMA_V1 {
        return Err(ProjectTraceError::InvalidMetadata(
            "trace logical graph schema differs from LogicalHGraphV1".to_string(),
        ));
    }
    let expected_graph_digest = project_logical_graph_digest(project)?;
    if header.logical_graph_digest != expected_graph_digest {
        return Err(ProjectTraceError::InvalidMetadata(
            "trace logical graph digest differs from the trusted Project HGraph".to_string(),
        ));
    }
    if header.deployment_plan_schema != super::deployment::DEPLOYMENT_PLAN_SCHEMA_V1 {
        return Err(ProjectTraceError::InvalidMetadata(
            "trace deployment plan schema differs from DeploymentPlanV1".to_string(),
        ));
    }
    if header.deployment_plan_digest != expected_deployment_digest {
        return Err(ProjectTraceError::InvalidMetadata(
            "trace deployment plan digest differs from the trusted deployment".to_string(),
        ));
    }
    Ok(())
}

fn validate_transitive_prerequisite_coverage(
    project: &ProjectHGraph,
    events: &[ProjectAttemptEvent],
    consumer: PlanNodeId,
    visiting: &mut BTreeSet<PlanNodeId>,
) -> Result<(), ProjectTraceError> {
    if !visiting.insert(consumer) {
        return Err(ProjectTraceError::InvalidMetadata(format!(
            "trusted project prerequisite cycle reaches plan node {}",
            consumer.0
        )));
    }
    let operation = project
        .plan
        .operations
        .get(consumer.0)
        .filter(|operation| operation.id == consumer)
        .ok_or_else(|| {
            ProjectTraceError::InvalidMetadata(format!(
                "trace references missing project plan node {}",
                consumer.0
            ))
        })?;
    let consumer_ready = events
        .iter()
        .position(|event| event.plan_node == consumer && event.state == ProjectAttemptState::Ready)
        .ok_or_else(|| {
            ProjectTraceError::InvalidEvent(format!(
                "plan node {} has no Ready lifecycle event",
                consumer.0
            ))
        })?;

    for dependency in &operation.dependencies {
        let ProjectDependency::Success(prerequisite) = dependency else {
            continue;
        };
        let prerequisite_operation = project
            .plan
            .operations
            .get(prerequisite.0)
            .filter(|operation| operation.id == *prerequisite)
            .ok_or_else(|| {
                ProjectTraceError::InvalidMetadata(format!(
                    "plan node {} has missing Success prerequisite {}",
                    consumer.0, prerequisite.0
                ))
            })?;
        if !matches!(prerequisite_operation.op, ExecutableOp::RunRoute { .. }) {
            return Err(ProjectTraceError::InvalidMetadata(format!(
                "plan node {} has non-route Success prerequisite {}",
                consumer.0, prerequisite.0
            )));
        }
        let (terminal_index, terminal) = events
            .iter()
            .enumerate()
            .find(|(_, event)| event.plan_node == *prerequisite && event.state.is_terminal())
            .ok_or_else(|| {
                ProjectTraceError::InvalidEvent(format!(
                    "plan node {} omits terminal lifecycle coverage for prerequisite {}",
                    consumer.0, prerequisite.0
                ))
            })?;
        if terminal_index >= consumer_ready {
            return Err(ProjectTraceError::InvalidEvent(format!(
                "prerequisite plan node {} settled after dependent plan node {} became Ready",
                prerequisite.0, consumer.0
            )));
        }
        if !matches!(
            terminal.state,
            ProjectAttemptState::SettledSuccess | ProjectAttemptState::Skipped
        ) {
            return Err(ProjectTraceError::InvalidEvent(format!(
                "dependent plan node {} became Ready without successful prerequisite {}",
                consumer.0, prerequisite.0
            )));
        }
        validate_transitive_prerequisite_coverage(project, events, *prerequisite, visiting)?;
    }
    visiting.remove(&consumer);
    Ok(())
}

fn validate_completed_selection(
    project: &ProjectHGraph,
    events: &[ProjectAttemptEvent],
) -> Result<(), ProjectTraceError> {
    let Some((_, selection_finished)) = events.iter().enumerate().find(|(_, event)| {
        event.state == ProjectAttemptState::Finished
            && project
                .plan
                .operations
                .get(event.plan_node.0)
                .is_some_and(|operation| matches!(operation.op, ExecutableOp::SelectRoute { .. }))
    }) else {
        // Stalled/aborted coordinators intentionally retain partial traces with
        // no successfully committed SelectRoute root.
        return Ok(());
    };
    let selection_ready = events
        .iter()
        .position(|event| {
            event.plan_node == selection_finished.plan_node
                && event.state == ProjectAttemptState::Ready
        })
        .ok_or_else(|| {
            ProjectTraceError::InvalidEvent(
                "completed SelectRoute has no Ready lifecycle event".to_string(),
            )
        })?;
    let (terminal_index, terminal) = events
        .iter()
        .enumerate()
        .rfind(|(_, event)| {
            let Some(branch) = event.branch else {
                return false;
            };
            event
                .route_id
                .as_ref()
                .is_some_and(|route_id| project.plan.alternatives.get(branch) == Some(route_id))
                && matches!(
                    event.state,
                    ProjectAttemptState::SettledSuccess
                        | ProjectAttemptState::SettledFailure
                        | ProjectAttemptState::Skipped
                )
        })
        .ok_or_else(|| {
            ProjectTraceError::InvalidEvent(
                "completed SelectRoute omits its terminal alternative lifecycle".to_string(),
            )
        })?;
    if terminal_index >= selection_ready {
        return Err(ProjectTraceError::InvalidEvent(
            "SelectRoute became Ready before its selected terminal alternative settled".to_string(),
        ));
    }
    validate_transitive_prerequisite_coverage(
        project,
        events,
        terminal.plan_node,
        &mut BTreeSet::new(),
    )?;

    if matches!(
        project.plan.policy,
        RoutePolicy::Fallback | RoutePolicy::AnySuccess
    ) && terminal.state != ProjectAttemptState::SettledSuccess
    {
        let branch = terminal
            .branch
            .expect("filtered terminal alternative has a branch");
        let exhausted = branch + 1 == project.plan.alternatives.len();
        let denied = terminal
            .continuation
            .as_ref()
            .is_some_and(|decision| !decision.admitted);
        if !exhausted && !denied {
            return Err(ProjectTraceError::InvalidEvent(format!(
                "completed ordered selection stops after admitted branch {branch} without a later terminal alternative"
            )));
        }
    }
    Ok(())
}

fn recompute_continuation_evidence(
    project: &ProjectHGraph,
    events: &[ProjectAttemptEvent],
    branch: usize,
) -> Result<ProjectContinuationEvidence, ProjectTraceError> {
    let mut saw_executed = false;
    let mut every_executed_route_is_idempotent = true;
    for event in events.iter().filter(|event| {
        event.branch == Some(branch)
            && event.route_id.is_some()
            && matches!(
                event.state,
                ProjectAttemptState::SettledSuccess
                    | ProjectAttemptState::SettledFailure
                    | ProjectAttemptState::Skipped
            )
    }) {
        if event.state == ProjectAttemptState::Skipped {
            continue;
        }
        saw_executed = true;
        let operation = project
            .plan
            .operations
            .get(event.plan_node.0)
            .filter(|operation| operation.id == event.plan_node)
            .ok_or_else(|| {
                ProjectTraceError::InvalidMetadata(format!(
                    "trace references missing project plan node {}",
                    event.plan_node.0
                ))
            })?;
        let continuation = operation
            .route_facts
            .as_ref()
            .ok_or_else(|| {
                ProjectTraceError::InvalidMetadata(format!(
                    "run-route plan node {} lacks RoutePlanFacts",
                    event.plan_node.0
                ))
            })?
            .failure_continuation;
        every_executed_route_is_idempotent &=
            continuation == RouteFailureContinuation::DeclaredIdempotent;
    }

    Ok(if !saw_executed {
        ProjectContinuationEvidence::NoExecution
    } else if every_executed_route_is_idempotent {
        ProjectContinuationEvidence::DeclaredIdempotent
    } else {
        ProjectContinuationEvidence::UnprovenEffects
    })
}

/// Deterministic trace-construction failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectTraceError {
    InvalidMetadata(String),
    InvalidOutcome(String),
    InvalidEvent(String),
    NoncontiguousOrdinal {
        expected: u64,
        actual: u64,
    },
    OrdinalOverflow,
    MetadataChanged(PlanNodeId),
    InvalidTransition {
        plan_node: PlanNodeId,
        from: Option<ProjectAttemptState>,
        to: ProjectAttemptState,
    },
}

impl fmt::Display for ProjectTraceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMetadata(message)
            | Self::InvalidOutcome(message)
            | Self::InvalidEvent(message) => formatter.write_str(message),
            Self::NoncontiguousOrdinal { expected, actual } => write!(
                formatter,
                "noncontiguous coordinator event ordinal: expected {expected}, got {actual}"
            ),
            Self::OrdinalOverflow => {
                formatter.write_str("project attempt trace exceeds u64 event ordinals")
            }
            Self::MetadataChanged(plan_node) => write!(
                formatter,
                "project attempt metadata changed for plan node {}",
                plan_node.0
            ),
            Self::InvalidTransition {
                plan_node,
                from,
                to,
            } => write!(
                formatter,
                "invalid project attempt transition for plan node {}: {from:?} -> {to:?}",
                plan_node.0
            ),
        }
    }
}

impl Error for ProjectTraceError {}

fn validate_label(value: &str, field: &str) -> Result<(), ProjectTraceError> {
    // Route identifiers are an existing ProjectBundle compatibility surface.
    // The planner rejects empty IDs but otherwise treats them as opaque UTF-8;
    // tracing must not invent a stricter HGraph-only identifier grammar.
    if value.is_empty() {
        return Err(ProjectTraceError::InvalidMetadata(format!(
            "{field} must be nonempty"
        )));
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> Result<(), ProjectTraceError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ProjectTraceError::InvalidOutcome(format!(
            "{field} must be a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn validate_metadata_sha256(value: &str, field: &str) -> Result<(), ProjectTraceError> {
    validate_sha256(value, field).map_err(|error| match error {
        ProjectTraceError::InvalidOutcome(message) => ProjectTraceError::InvalidMetadata(message),
        other => other,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

mod plan_node_id_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::ir::PlanNodeId;

    pub fn serialize<S>(id: &PlanNodeId, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(id.0 as u64)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PlanNodeId, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        let value = usize::try_from(value).map_err(serde::de::Error::custom)?;
        Ok(PlanNodeId(value))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::project::model::{ExecutionProvenance, OutputCapture, RouteExecutionDisposition};

    fn identity(node: usize) -> ProjectAttemptIdentity {
        ProjectAttemptIdentity::new(
            PlanNodeId(node),
            "run-route:test",
            Some(0),
            Some("test".to_string()),
        )
    }

    fn non_route_identity(node: usize) -> ProjectAttemptIdentity {
        ProjectAttemptIdentity::new(PlanNodeId(node), "materialize-project", Some(0), None)
    }

    fn header() -> ProjectAttemptTraceHeader {
        ProjectAttemptTraceHeader::new(
            "project",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "main",
            "default",
            super::super::logical::LOGICAL_HGRAPH_SCHEMA_V1,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            super::super::deployment::DEPLOYMENT_PLAN_SCHEMA_V1,
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "attempt-1",
        )
    }

    fn result(artifacts: Vec<Artifact>) -> OExecutionResult {
        let stdout = b"abc".to_vec();
        let stderr = Vec::new();
        OExecutionResult {
            route_id: "test".to_string(),
            exit_code: Some(0),
            stdout_capture: OutputCapture::complete(&stdout),
            stdout,
            stderr_capture: OutputCapture::complete(&stderr),
            stderr,
            value: None,
            artifact_requirements: artifacts
                .iter()
                .map(|artifact| artifact.path.clone())
                .collect(),
            artifacts,
            artifact_capture: ArtifactCaptureStatus::Complete,
            disposition: RouteExecutionDisposition::Executed,
            duration_ns: 999,
            provenance: ExecutionProvenance {
                workspace: PathBuf::from("/volatile/workspace"),
                command: vec!["tool".to_string()],
                cwd: PathBuf::from("/volatile/workspace/project"),
            },
        }
    }

    fn skipped_outcome() -> ProjectRouteOutcome {
        let mut skipped = result(Vec::new());
        skipped.exit_code = None;
        skipped.stdout.clear();
        skipped.stdout_capture = OutputCapture::complete(&skipped.stdout);
        skipped.stderr = b"[olang-project] skipped: test guard\n".to_vec();
        skipped.stderr_capture = OutputCapture::complete(&skipped.stderr);
        skipped.disposition = RouteExecutionDisposition::GuardSkipped;
        ProjectRouteOutcome::from_result(&skipped).unwrap()
    }

    #[test]
    fn route_outcome_hashes_bytes_and_sorts_artifacts() {
        let outcome = ProjectRouteOutcome::from_result(&result(vec![
            Artifact {
                path: "z.bin".to_string(),
                content_hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_string(),
                bytes_len: 2,
            },
            Artifact {
                path: "a.bin".to_string(),
                content_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
                bytes_len: 1,
            },
        ]))
        .unwrap();

        assert_eq!(
            outcome.stdout_sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(outcome.stdout_total_observed_bytes, 3);
        assert_eq!(outcome.stdout_retained_bytes, 3);
        assert!(!outcome.stdout_truncated);
        assert_eq!(
            outcome.stderr_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(outcome.stderr_total_observed_bytes, 0);
        assert_eq!(outcome.stderr_retained_bytes, 0);
        assert!(!outcome.stderr_truncated);
        assert_eq!(
            outcome
                .artifacts
                .iter()
                .map(|artifact| artifact.path.as_str())
                .collect::<Vec<_>>(),
            ["a.bin", "z.bin"]
        );
    }

    #[test]
    fn incomplete_artifact_evidence_cannot_form_a_success_outcome() {
        let mut execution = result(Vec::new());
        execution.artifact_capture = ArtifactCaptureStatus::Incomplete {
            failure: Box::new(super::super::model::ArtifactCaptureFailure::Missing {
                requirement: "required.bin".to_string(),
            }),
        };

        assert!(!execution.succeeded());
        let error = ProjectRouteOutcome::from_result(&execution).unwrap_err();
        assert!(error
            .to_string()
            .contains("exit-zero route outcome has incomplete artifact evidence"));

        execution.exit_code = Some(9);
        let outcome = ProjectRouteOutcome::from_result(&execution).unwrap();
        outcome.validate().unwrap();
        assert!(matches!(
            outcome.artifact_capture,
            ArtifactCaptureStatus::Incomplete { .. }
        ));
    }

    #[test]
    fn route_outcome_binds_complete_stream_when_retention_is_truncated() {
        let mut result = result(Vec::new());
        result.stdout = b"prefix".to_vec();
        result.stdout_capture = OutputCapture {
            total_observed_bytes: 13,
            retained_bytes: 6,
            truncated: true,
            sha256: hex::encode(Sha256::digest(b"prefix-suffix")),
        };

        let outcome = ProjectRouteOutcome::from_result(&result).unwrap();
        assert_eq!(outcome.stdout_total_observed_bytes, 13);
        assert_eq!(outcome.stdout_retained_bytes, 6);
        assert!(outcome.stdout_truncated);
        assert_eq!(
            outcome.stdout_sha256,
            hex::encode(Sha256::digest(b"prefix-suffix"))
        );
    }

    #[test]
    fn trace_requires_a_canonical_execution_header() {
        let expected = header();
        let trace = ProjectAttemptTrace::new(expected.clone()).unwrap();
        assert_eq!(trace.format_version(), PROJECT_ATTEMPT_TRACE_VERSION);
        assert_eq!(trace.header(), &expected);
        let serialized = serde_json::to_value(&trace).unwrap();
        assert_eq!(serialized["format_version"], 6);
        assert_eq!(serialized["header"]["project_name"], "project");
        assert_eq!(serialized["header"]["logical_graph_schema"], 1);
        assert_eq!(serialized["header"]["deployment_plan_schema"], 1);
        assert_eq!(serialized["header"]["execution_attempt_id"], "attempt-1");

        let mut invalid = expected;
        invalid.logical_graph_digest = "not-a-digest".to_string();
        assert!(matches!(
            ProjectAttemptTrace::new(invalid),
            Err(ProjectTraceError::InvalidMetadata(message))
                if message == "logical graph digest must be a lowercase SHA-256 digest"
        ));
    }

    #[test]
    fn checked_trace_records_one_complete_lifecycle() {
        let identity = identity(4);
        let outcome = ProjectRouteOutcome::from_result(&result(Vec::new())).unwrap();
        let mut trace = ProjectAttemptTrace::new(header()).unwrap();
        trace.record_ready(&identity).unwrap();
        trace.record_started(&identity).unwrap();
        trace
            .record_settled_success(&identity, outcome.clone())
            .unwrap();

        assert_eq!(
            trace.state(PlanNodeId(4)),
            Some(ProjectAttemptState::SettledSuccess)
        );
        assert_eq!(
            trace
                .events()
                .iter()
                .map(|event| event.state)
                .collect::<Vec<_>>(),
            [
                ProjectAttemptState::Ready,
                ProjectAttemptState::Started,
                ProjectAttemptState::SettledSuccess,
            ]
        );
        assert_eq!(
            trace
                .events()
                .iter()
                .map(|event| event.coordinator_ordinal)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(trace.events()[2].outcome.as_ref(), Some(&outcome));
    }

    #[test]
    fn trace_rejects_metadata_drift_and_duplicate_terminal_events() {
        let identity = non_route_identity(1);
        let mut trace = ProjectAttemptTrace::new(header()).unwrap();
        trace.record_ready(&identity).unwrap();

        let mut changed = identity.clone();
        changed.branch = Some(1);
        let error = trace.record_started(&changed).unwrap_err();
        assert_eq!(error, ProjectTraceError::MetadataChanged(PlanNodeId(1)));

        trace.record_started(&identity).unwrap();
        trace.record_finished(&identity).unwrap();
        assert!(matches!(
            trace.record_aborted(&identity, None, "late failure"),
            Err(ProjectTraceError::InvalidTransition {
                from: Some(ProjectAttemptState::Finished),
                to: ProjectAttemptState::Aborted,
                ..
            })
        ));
    }

    #[test]
    fn trace_accepts_each_exact_terminal_vocabulary() {
        let outcome = ProjectRouteOutcome::from_result(&result(Vec::new())).unwrap();
        let mut trace = ProjectAttemptTrace::new(header()).unwrap();

        let finished = non_route_identity(0);
        trace.record_ready(&finished).unwrap();
        trace.record_started(&finished).unwrap();
        trace.record_finished(&finished).unwrap();

        for (node, state) in [
            (1, ProjectAttemptState::SettledSuccess),
            (2, ProjectAttemptState::SettledFailure),
            (3, ProjectAttemptState::Skipped),
        ] {
            let route = identity(node);
            trace.record_ready(&route).unwrap();
            trace.record_started(&route).unwrap();
            match state {
                ProjectAttemptState::SettledSuccess => trace
                    .record_settled_success(&route, outcome.clone())
                    .unwrap(),
                ProjectAttemptState::SettledFailure => {
                    let mut failed = outcome.clone();
                    failed.exit_code = Some(7);
                    trace.record_settled_failure(&route, failed).unwrap()
                }
                ProjectAttemptState::Skipped => {
                    trace.record_skipped(&route, skipped_outcome()).unwrap()
                }
                _ => unreachable!(),
            }
        }

        let aborted = identity(4);
        trace.record_ready(&aborted).unwrap();
        trace.record_started(&aborted).unwrap();
        trace
            .record_aborted(&aborted, None, b"normalized infrastructure failure")
            .unwrap();

        assert_eq!(
            [0, 1, 2, 3, 4].map(|node| trace.state(PlanNodeId(node)).unwrap()),
            [
                ProjectAttemptState::Finished,
                ProjectAttemptState::SettledSuccess,
                ProjectAttemptState::SettledFailure,
                ProjectAttemptState::Skipped,
                ProjectAttemptState::Aborted,
            ]
        );
        assert_eq!(
            trace
                .events()
                .iter()
                .map(|event| event.coordinator_ordinal)
                .collect::<Vec<_>>(),
            (0_u64..15).collect::<Vec<_>>()
        );
        let aborted_event = trace.events().last().unwrap();
        assert!(aborted_event.outcome.is_none());
        assert_eq!(
            aborted_event.failure_sha256.as_deref().map(str::len),
            Some(64)
        );
    }

    #[test]
    fn trace_rejects_route_and_non_route_terminal_confusion() {
        let outcome = ProjectRouteOutcome::from_result(&result(Vec::new())).unwrap();

        let route = identity(0);
        let mut route_trace = ProjectAttemptTrace::new(header()).unwrap();
        route_trace.record_ready(&route).unwrap();
        route_trace.record_started(&route).unwrap();
        assert!(matches!(
            route_trace.record_finished(&route),
            Err(ProjectTraceError::InvalidEvent(message))
                if message == "route attempt uses the non-route Finished state"
        ));

        let non_route = non_route_identity(1);
        let mut non_route_trace = ProjectAttemptTrace::new(header()).unwrap();
        non_route_trace.record_ready(&non_route).unwrap();
        non_route_trace.record_started(&non_route).unwrap();
        assert!(matches!(
            non_route_trace.record_settled_failure(&non_route, outcome),
            Err(ProjectTraceError::InvalidEvent(message))
                if message == "route settlement event does not identify a RunRoute operation"
        ));
    }

    #[test]
    fn trace_rejects_noncontiguous_external_ordinal() {
        let identity = identity(1);
        let mut trace = ProjectAttemptTrace::new(header()).unwrap();
        let event =
            ProjectAttemptEvent::new(1, &identity, ProjectAttemptState::Ready, None, None, None);
        assert!(matches!(
            trace.record(event),
            Err(ProjectTraceError::NoncontiguousOrdinal {
                expected: 0,
                actual: 1,
            })
        ));
    }

    #[test]
    fn trace_preserves_opaque_nonempty_project_route_ids() {
        let identity = ProjectAttemptIdentity::new(
            PlanNodeId(7),
            "run-route: route\n",
            Some(0),
            Some(" route\n".to_string()),
        );
        let mut trace = ProjectAttemptTrace::new(header()).unwrap();
        trace.record_ready(&identity).unwrap();
        trace.record_started(&identity).unwrap();
        trace
            .record_settled_success(
                &identity,
                ProjectRouteOutcome::from_result(&result(Vec::new())).unwrap(),
            )
            .unwrap();
        assert_eq!(trace.events()[2].route_id.as_deref(), Some(" route\n"));
    }

    #[test]
    fn serialized_event_uses_numeric_plan_node_and_no_raw_output() {
        let identity = identity(3);
        let outcome = ProjectRouteOutcome::from_result(&result(Vec::new())).unwrap();
        let event = ProjectAttemptEvent::new(
            0,
            &identity,
            ProjectAttemptState::SettledSuccess,
            Some(outcome),
            None,
            None,
        );
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["plan_node"], 3);
        assert_eq!(value["state"], "settled_success");
        assert!(value.get("stdout").is_none());
        assert!(value["outcome"].get("stdout_sha256").is_some());
    }
}
