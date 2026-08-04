//! Deterministic attempt tracing for the hosted project coordinator.
//!
//! A [`ProjectAttemptEvent`] records a coordinator-assigned event ordinal and
//! its planner-local [`PlanNodeId`]. The ordinal is the event's position in the
//! committed lifecycle sequence: ordinals are unique and contiguous from zero.
//! It is not a wall-clock timestamp or an operation identity; [`PlanNodeId`]
//! identifies the operation.
//!
//! A successful operation's commit (linearization) point is one indivisible
//! local coordinator transition containing all three of:
//!
//! 1. the terminal `Finished` event;
//! 2. the stored successful operation value; and
//! 3. atomic materialization/publication of the operation's declared outputs.
//!
//! A failed operation instead linearizes when the coordinator records its
//! terminal `Failed` event and marks the operation's output nodes failed. No
//! successful operation value is stored and none of that operation's outputs
//! are materialized or published. Appending a terminal event before its
//! corresponding coordinator state transition is therefore not a commit.
//!
//! This local commit discipline does **not** imply exactly-once external
//! effects: a command may have changed a host file, contacted a service, or
//! performed another effect before a crash, cancellation, or retry. Such
//! effects require their own idempotency or recovery protocol.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hgraph::ExecutableOp;
use crate::ir::PlanNodeId;

use super::model::{Artifact, OExecutionResult};
use super::plan::ProjectPlanOperation;

/// Version of the deterministic project-attempt event vocabulary.
pub const PROJECT_ATTEMPT_TRACE_VERSION: u32 = 1;

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
        if artifact.path.is_empty() || artifact.path.contains('\0') {
            return Err(ProjectTraceError::InvalidOutcome(
                "artifact path must be nonempty and contain no NUL".to_string(),
            ));
        }
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
    /// Lowercase hexadecimal SHA-256 of exact captured stdout bytes.
    pub stdout_sha256: String,
    /// Lowercase hexadecimal SHA-256 of exact captured stderr bytes.
    pub stderr_sha256: String,
    /// Deterministically ordered fingerprints of declared output artifacts.
    pub artifacts: Vec<ProjectArtifactFingerprint>,
}

impl ProjectRouteOutcome {
    /// Normalize an execution result into deterministic, content-addressed
    /// trace data.
    pub fn from_result(result: &OExecutionResult) -> Result<Self, ProjectTraceError> {
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
        Ok(Self {
            exit_code: result.exit_code,
            stdout_sha256: sha256_hex(&result.stdout),
            stderr_sha256: sha256_hex(&result.stderr),
            artifacts,
        })
    }

    fn validate(&self) -> Result<(), ProjectTraceError> {
        validate_sha256(&self.stdout_sha256, "stdout fingerprint")?;
        validate_sha256(&self.stderr_sha256, "stderr fingerprint")?;
        let mut prior: Option<&ProjectArtifactFingerprint> = None;
        for artifact in &self.artifacts {
            if artifact.path.is_empty() || artifact.path.contains('\0') {
                return Err(ProjectTraceError::InvalidOutcome(
                    "artifact path must be nonempty and contain no NUL".to_string(),
                ));
            }
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
        Ok(())
    }
}

impl TryFrom<&OExecutionResult> for ProjectRouteOutcome {
    type Error = ProjectTraceError;

    fn try_from(result: &OExecutionResult) -> Result<Self, Self::Error> {
        Self::from_result(result)
    }
}

/// Lifecycle state of one project-plan operation attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAttemptState {
    Ready,
    Started,
    Finished,
    Failed,
}

impl ProjectAttemptState {
    /// True only for the states that participate in the coordinator's local
    /// commit/linearization point.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Failed)
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
    /// Present for route terminal events; absent for non-route operations and
    /// all nonterminal lifecycle events.
    pub outcome: Option<ProjectRouteOutcome>,
    /// SHA-256 of the coordinator's normalized failure description. Raw error
    /// text is not retained because host paths and tool diagnostics are often
    /// nondeterministic.
    pub failure_sha256: Option<String>,
}

impl ProjectAttemptEvent {
    fn new(
        coordinator_ordinal: u64,
        identity: &ProjectAttemptIdentity,
        state: ProjectAttemptState,
        outcome: Option<ProjectRouteOutcome>,
        failure_sha256: Option<String>,
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
        match self.state {
            ProjectAttemptState::Ready | ProjectAttemptState::Started => {
                if self.outcome.is_some() || self.failure_sha256.is_some() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "nonterminal attempt event carries terminal data".to_string(),
                    ));
                }
            }
            ProjectAttemptState::Finished => {
                if self.failure_sha256.is_some() {
                    return Err(ProjectTraceError::InvalidEvent(
                        "finished attempt event carries a failure fingerprint".to_string(),
                    ));
                }
            }
            ProjectAttemptState::Failed => {
                let digest = self.failure_sha256.as_deref().ok_or_else(|| {
                    ProjectTraceError::InvalidEvent(
                        "failed attempt event lacks a failure fingerprint".to_string(),
                    )
                })?;
                validate_sha256(digest, "failure fingerprint")?;
            }
        }
        Ok(())
    }
}

/// Checked deterministic lifecycle history for project-plan attempts.
#[derive(Clone, Debug, Serialize)]
pub struct ProjectAttemptTrace {
    pub format_version: u32,
    events: Vec<ProjectAttemptEvent>,
    #[serde(skip)]
    attempts: BTreeMap<PlanNodeId, (ProjectAttemptIdentity, ProjectAttemptState)>,
}

impl Default for ProjectAttemptTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectAttemptTrace {
    pub fn new() -> Self {
        Self {
            format_version: PROJECT_ATTEMPT_TRACE_VERSION,
            events: Vec::new(),
            attempts: BTreeMap::new(),
        }
    }

    /// Rebuild a trace from events while replaying every lifecycle invariant.
    pub fn try_from_events(
        events: impl IntoIterator<Item = ProjectAttemptEvent>,
    ) -> Result<Self, ProjectTraceError> {
        let mut trace = Self::new();
        for event in events {
            trace.record(event)?;
        }
        Ok(trace)
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
        self.record_state(identity, ProjectAttemptState::Ready, None, None)
    }

    pub fn record_started(
        &mut self,
        identity: &ProjectAttemptIdentity,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(identity, ProjectAttemptState::Started, None, None)
    }

    /// Record a successful terminal event.
    ///
    /// `outcome` is required by the coordinator for `RunRoute` and remains
    /// `None` for other project operations. This method only validates/appends
    /// trace state; the caller must include it in the atomic terminal-event +
    /// stored-value + output-materialization commit described at module scope.
    pub fn record_finished(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: Option<ProjectRouteOutcome>,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(identity, ProjectAttemptState::Finished, outcome, None)
    }

    /// Record a failed terminal event without retaining nondeterministic raw
    /// diagnostics. The exact normalized failure bytes are SHA-256-bound. The
    /// caller must pair this event with failed-output bookkeeping only; it must
    /// not store a success value or materialize/publish this operation's
    /// outputs.
    pub fn record_failed(
        &mut self,
        identity: &ProjectAttemptIdentity,
        outcome: Option<ProjectRouteOutcome>,
        normalized_failure: impl AsRef<[u8]>,
    ) -> Result<(), ProjectTraceError> {
        self.record_state(
            identity,
            ProjectAttemptState::Failed,
            outcome,
            Some(sha256_hex(normalized_failure.as_ref())),
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
                            ProjectAttemptState::Finished | ProjectAttemptState::Failed
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

    fn record_state(
        &mut self,
        identity: &ProjectAttemptIdentity,
        state: ProjectAttemptState,
        outcome: Option<ProjectRouteOutcome>,
        failure_sha256: Option<String>,
    ) -> Result<(), ProjectTraceError> {
        let coordinator_ordinal = self.next_ordinal()?;
        self.record(ProjectAttemptEvent::new(
            coordinator_ordinal,
            identity,
            state,
            outcome,
            failure_sha256,
        ))
    }

    fn next_ordinal(&self) -> Result<u64, ProjectTraceError> {
        u64::try_from(self.events.len()).map_err(|_| ProjectTraceError::OrdinalOverflow)
    }
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
    use crate::project::model::ExecutionProvenance;

    fn identity(node: usize) -> ProjectAttemptIdentity {
        ProjectAttemptIdentity::new(
            PlanNodeId(node),
            "run-route:test",
            Some(0),
            Some("test".to_string()),
        )
    }

    fn result(artifacts: Vec<Artifact>) -> OExecutionResult {
        OExecutionResult {
            route_id: "test".to_string(),
            exit_code: Some(0),
            stdout: b"abc".to_vec(),
            stderr: Vec::new(),
            value: None,
            artifacts,
            duration_ns: 999,
            provenance: ExecutionProvenance {
                workspace: PathBuf::from("/volatile/workspace"),
                command: vec!["tool".to_string()],
                cwd: PathBuf::from("/volatile/workspace/project"),
            },
        }
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
        assert_eq!(
            outcome.stderr_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
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
    fn checked_trace_records_one_complete_lifecycle() {
        let identity = identity(4);
        let outcome = ProjectRouteOutcome::from_result(&result(Vec::new())).unwrap();
        let mut trace = ProjectAttemptTrace::new();
        trace.record_ready(&identity).unwrap();
        trace.record_started(&identity).unwrap();
        trace
            .record_finished(&identity, Some(outcome.clone()))
            .unwrap();

        assert_eq!(
            trace.state(PlanNodeId(4)),
            Some(ProjectAttemptState::Finished)
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
                ProjectAttemptState::Finished,
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
        let identity = identity(1);
        let mut trace = ProjectAttemptTrace::new();
        trace.record_ready(&identity).unwrap();

        let mut changed = identity.clone();
        changed.branch = Some(1);
        let error = trace.record_started(&changed).unwrap_err();
        assert_eq!(error, ProjectTraceError::MetadataChanged(PlanNodeId(1)));

        trace.record_started(&identity).unwrap();
        trace.record_finished(&identity, None).unwrap();
        assert!(matches!(
            trace.record_failed(&identity, None, "late failure"),
            Err(ProjectTraceError::InvalidTransition {
                from: Some(ProjectAttemptState::Finished),
                to: ProjectAttemptState::Failed,
                ..
            })
        ));
    }

    #[test]
    fn trace_rejects_noncontiguous_external_ordinal() {
        let identity = identity(1);
        let mut trace = ProjectAttemptTrace::new();
        let event = ProjectAttemptEvent::new(1, &identity, ProjectAttemptState::Ready, None, None);
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
        let mut trace = ProjectAttemptTrace::new();
        trace.record_ready(&identity).unwrap();
        trace.record_started(&identity).unwrap();
        trace.record_finished(&identity, None).unwrap();
        assert_eq!(trace.events()[2].route_id.as_deref(), Some(" route\n"));
    }

    #[test]
    fn serialized_event_uses_numeric_plan_node_and_no_raw_output() {
        let identity = identity(3);
        let outcome = ProjectRouteOutcome::from_result(&result(Vec::new())).unwrap();
        let event = ProjectAttemptEvent::new(
            0,
            &identity,
            ProjectAttemptState::Finished,
            Some(outcome),
            None,
        );
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["plan_node"], 3);
        assert_eq!(value["state"], "finished");
        assert!(value.get("stdout").is_none());
        assert!(value["outcome"].get("stdout_sha256").is_some());
    }
}
