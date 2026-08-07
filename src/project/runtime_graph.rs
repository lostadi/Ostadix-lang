//! Canonical terminal RuntimeGraph observation for hosted World-bound projects.
//!
//! [`RuntimeGraphV1`] is an immutable, post-execution observation. It binds the
//! exact logical graph, deployment proposal, non-authorizing hosted launch,
//! placement snapshot, provider identity, World generation, operation attempts,
//! and normalized coordinator trace events that produced it. It is not a live
//! scheduler data structure, an admission decision, a capability, a Governor
//! statement, a provider reservation, or a commit fence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::ir::PlanNodeId;
use crate::world::{
    ArtifactId, AttemptIdentity, GovernorIdentity, ResourceOwner, TaskIdentity, WorldIdentity,
    WorldIdentityError,
};

use super::deployment::{
    DeploymentPlanError, DeploymentPlanV1, DeploymentProviderBindingV1, DEPLOYMENT_PLAN_SCHEMA_V1,
    MAX_DEPLOYMENT_OPERATIONS, PLACEMENT_SNAPSHOT_SCHEMA_V1,
};
use super::launch::{
    HostedWorldCoordinatorObserverV1, HostedWorldLaunchError, HostedWorldLaunchV1,
    HOSTED_WORLD_LAUNCH_SCHEMA_V1,
};
use super::logical::{
    LogicalHGraphError, LogicalHGraphV1, LogicalOperationIdV1, LogicalOperationKindV1,
    LogicalRoutePolicyV1, LOGICAL_HGRAPH_SCHEMA_V1,
};
use super::model::{OExecutionResult, RouteExecutionDisposition, RoutePolicy};
use super::plan::ProjectHGraph;
use super::trace::{
    ProjectAttemptEvent, ProjectAttemptState, ProjectAttemptTrace, ProjectAttemptTraceHeader,
    ProjectContinuationDecision, ProjectRouteOutcome, ProjectTraceError,
    PROJECT_ATTEMPT_TRACE_VERSION,
};

pub const RUNTIME_GRAPH_SCHEMA_V1: u16 = 1;
pub const MAX_RUNTIME_GRAPH_RECORD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RUNTIME_GRAPH_OPERATIONS: usize = MAX_DEPLOYMENT_OPERATIONS;
pub const MAX_RUNTIME_GRAPH_OBSERVATIONS: usize = MAX_RUNTIME_GRAPH_OPERATIONS * 3;

const MAX_RUNTIME_GRAPH_TEXT_BYTES: usize = 4_096;
const RUNTIME_GRAPH_DIGEST_DOMAIN: &[u8] = b"ostadix.world.runtime-graph/v1\0";

#[derive(Debug, Error)]
pub enum RuntimeGraphError {
    #[error("invalid RuntimeGraphV1: {0}")]
    Invalid(String),
    #[error("RuntimeGraphV1 JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Logical(#[from] LogicalHGraphError),
    #[error(transparent)]
    Deployment(#[from] DeploymentPlanError),
    #[error(transparent)]
    Launch(#[from] HostedWorldLaunchError),
    #[error(transparent)]
    Trace(#[from] ProjectTraceError),
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error("RuntimeGraphV1 record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("RuntimeGraphV1 bytes are not the canonical encoding")]
    NonCanonicalEncoding,
}

fn invalid(reason: impl Into<String>) -> RuntimeGraphError {
    RuntimeGraphError::Invalid(reason.into())
}

fn validate_text(value: &str, field: &str) -> Result<(), RuntimeGraphError> {
    if value.is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    if value.len() > MAX_RUNTIME_GRAPH_TEXT_BYTES {
        return Err(invalid(format!(
            "{field} exceeds {MAX_RUNTIME_GRAPH_TEXT_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(format!("{field} contains a control character")));
    }
    Ok(())
}

fn validate_digest(digest: &ArtifactId, field: &str) -> Result<(), RuntimeGraphError> {
    if digest.as_sha256().bytes().all(|byte| byte == b'0') {
        return Err(invalid(format!(
            "{field} uses the reserved all-zero digest"
        )));
    }
    Ok(())
}

fn digest_canonical(bytes: &[u8]) -> Result<ArtifactId, RuntimeGraphError> {
    let byte_count = u64::try_from(bytes.len())
        .map_err(|_| invalid("canonical RuntimeGraph length does not fit u64"))?;
    let mut hasher = Sha256::new();
    hasher.update(RUNTIME_GRAPH_DIGEST_DOMAIN);
    hasher.update(byte_count.to_le_bytes());
    hasher.update(bytes);
    Ok(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
}

fn artifact_from_bytes(bytes: &[u8]) -> Result<ArtifactId, RuntimeGraphError> {
    Ok(ArtifactId::from_sha256(hex::encode(Sha256::digest(bytes)))?)
}

/// One normalized coordinator lifecycle event with its exact World attempt.
///
/// The task and attempt are repeated intentionally: a standalone observation
/// remains generation-bound even when extracted from its operation record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeGraphObservationV1 {
    pub coordinator_ordinal: u64,
    pub logical_operation: LogicalOperationIdV1,
    pub task: TaskIdentity,
    pub attempt: AttemptIdentity,
    pub operation_label: String,
    pub branch: Option<usize>,
    pub route_id: Option<String>,
    pub state: ProjectAttemptState,
    pub outcome: Option<ProjectRouteOutcome>,
    pub failure_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ProjectContinuationDecision>,
}

impl RuntimeGraphObservationV1 {
    fn from_trace(
        event: &ProjectAttemptEvent,
        logical_operation: LogicalOperationIdV1,
        task: &TaskIdentity,
        attempt: &AttemptIdentity,
    ) -> Self {
        Self {
            coordinator_ordinal: event.coordinator_ordinal,
            logical_operation,
            task: task.clone(),
            attempt: attempt.clone(),
            operation_label: event.operation_label.clone(),
            branch: event.branch,
            route_id: event.route_id.clone(),
            state: event.state,
            outcome: event.outcome.clone(),
            failure_sha256: event.failure_sha256.clone(),
            continuation: event.continuation.clone(),
        }
    }

    fn to_trace_event(&self) -> Result<ProjectAttemptEvent, RuntimeGraphError> {
        let plan_node = usize::try_from(self.logical_operation.0)
            .map(PlanNodeId)
            .map_err(|_| invalid("runtime logical operation id does not fit usize"))?;
        Ok(ProjectAttemptEvent {
            coordinator_ordinal: self.coordinator_ordinal,
            plan_node,
            operation_label: self.operation_label.clone(),
            branch: self.branch,
            route_id: self.route_id.clone(),
            state: self.state,
            outcome: self.outcome.clone(),
            failure_sha256: self.failure_sha256.clone(),
            continuation: self.continuation.clone(),
        })
    }
}

/// Exact launch binding plus observed lifecycle for one logical operation.
/// Empty observations mean the operation never entered the coordinator trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeGraphOperationV1 {
    pub logical_operation: LogicalOperationIdV1,
    pub task: TaskIdentity,
    pub attempt: AttemptIdentity,
    pub residual_host_world: bool,
    pub observations: Vec<RuntimeGraphObservationV1>,
}

/// Terminal status of the hosted coordinator, not a commit decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeGraphTerminalV1 {
    /// The coordinator materialized a selected route-result value. The neutral
    /// variant name is intentional: `settlement` distinguishes process success,
    /// nonzero settlement, and guard skip.
    RouteSettlement {
        selected_operation: LogicalOperationIdV1,
        route_id: String,
        disposition: RouteExecutionDisposition,
        settlement: ProjectAttemptState,
        outcome: ProjectRouteOutcome,
        residual_host_world: bool,
    },
    /// The coordinator itself failed. The digest content-binds the supplied
    /// failure detail without embedding it. Host/tool diagnostics may remain
    /// environment-sensitive; this field does not claim cross-host stability.
    CoordinatorFailure {
        detail_sha256: ArtifactId,
        residual_host_world: bool,
    },
}

impl RuntimeGraphTerminalV1 {
    pub fn coordinator_failure(
        failure_detail: impl AsRef<[u8]>,
        residual_host_world: bool,
    ) -> Result<Self, RuntimeGraphError> {
        Ok(Self::CoordinatorFailure {
            detail_sha256: artifact_from_bytes(failure_detail.as_ref())?,
            residual_host_world,
        })
    }
}

/// Canonical terminal hosted-reference RuntimeGraph observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeGraphV1 {
    pub schema_version: u16,
    pub logical_hgraph_schema: u16,
    pub logical_hgraph: ArtifactId,
    pub deployment_plan_schema: u16,
    pub deployment_plan: ArtifactId,
    pub launch_schema: u16,
    pub launch: ArtifactId,
    pub placement_snapshot_schema: u16,
    pub placement_snapshot: ArtifactId,
    pub project_name: String,
    pub project_bundle: ArtifactId,
    pub target: String,
    pub policy: String,
    pub trace_format_version: u32,
    pub trace_execution_attempt_id: String,
    pub world: WorldIdentity,
    /// Descriptive freshness context only; not Governor admission or authority.
    pub governor_context: GovernorIdentity,
    /// Caller-supplied current-view identity of the hosted coordinator that
    /// produced this observation. This is not the proposed provider placement.
    pub coordinator_observer: HostedWorldCoordinatorObserverV1,
    /// Dedicated attempt identity for this coordinator invocation, distinct
    /// from every per-operation attempt.
    pub coordinator_attempt: AttemptIdentity,
    /// Exact provider proposal carried through the non-authorizing launch. It
    /// does not prove that the Governor admitted it or that work ran there.
    pub selected_provider: DeploymentProviderBindingV1,
    pub operations: Vec<RuntimeGraphOperationV1>,
    pub terminal: RuntimeGraphTerminalV1,
}

impl RuntimeGraphV1 {
    pub fn from_project_result(
        project: &ProjectHGraph,
        deployment: &DeploymentPlanV1,
        launch: &HostedWorldLaunchV1,
        trace: &ProjectAttemptTrace,
        result: &OExecutionResult,
    ) -> Result<Self, RuntimeGraphError> {
        let terminal = project_result_terminal(project, deployment, trace.events(), result)?;
        Self::from_trace(project, deployment, launch, trace, terminal)
    }

    pub fn from_coordinator_failure(
        project: &ProjectHGraph,
        deployment: &DeploymentPlanV1,
        launch: &HostedWorldLaunchV1,
        trace: &ProjectAttemptTrace,
        failure_detail: impl AsRef<[u8]>,
    ) -> Result<Self, RuntimeGraphError> {
        if !trace
            .events()
            .iter()
            .any(|event| event.state == ProjectAttemptState::Started)
        {
            return Err(invalid(
                "coordinator-failure RuntimeGraph has no observed started operation",
            ));
        }
        if trace.events().iter().any(|event| {
            event.state == ProjectAttemptState::Finished
                && project
                    .plan
                    .operations
                    .get(event.plan_node.0)
                    .is_some_and(|operation| {
                        matches!(
                            operation.op,
                            crate::hgraph::ExecutableOp::SelectRoute { .. }
                        )
                    })
        }) {
            return Err(invalid(
                "coordinator-failure RuntimeGraph contains a completed SelectRoute root",
            ));
        }
        Self::from_trace(
            project,
            deployment,
            launch,
            trace,
            RuntimeGraphTerminalV1::coordinator_failure(
                failure_detail,
                observed_residual_host_world(deployment, trace.events())?,
            )?,
        )
    }

    fn from_trace(
        project: &ProjectHGraph,
        deployment: &DeploymentPlanV1,
        launch: &HostedWorldLaunchV1,
        trace: &ProjectAttemptTrace,
        terminal: RuntimeGraphTerminalV1,
    ) -> Result<Self, RuntimeGraphError> {
        let logical = project.logical_v1()?;
        validate_trusted_inputs(project, &logical, deployment, launch, trace)?;
        let trace_header = trace.header();
        let trace_events = trace.events();

        let logical_digest = logical.digest()?;
        let deployment_digest = deployment.digest()?;
        let launch_digest = launch.digest()?;
        let attempt_by_operation = launch
            .operation_attempts
            .iter()
            .map(|entry| (entry.logical_operation, &entry.attempt))
            .collect::<BTreeMap<_, _>>();

        let mut operations = Vec::with_capacity(logical.operations.len());
        for (index, logical_operation) in logical.operations.iter().enumerate() {
            let deployment_operation = deployment
                .operations
                .get(index)
                .ok_or_else(|| invalid("deployment omits a logical operation"))?;
            if deployment_operation.logical_operation != logical_operation.id {
                return Err(invalid(
                    "deployment operation order differs from the logical graph",
                ));
            }
            let task = deployment_operation
                .task
                .as_ref()
                .ok_or_else(|| invalid("World-bound deployment operation has no task"))?;
            let attempt = attempt_by_operation
                .get(&logical_operation.id)
                .ok_or_else(|| invalid("launch omits a logical operation attempt"))?;
            operations.push(RuntimeGraphOperationV1 {
                logical_operation: logical_operation.id,
                task: task.clone(),
                attempt: (*attempt).clone(),
                residual_host_world: deployment_operation.requirements.residual_host_world,
                observations: Vec::new(),
            });
        }

        for event in trace_events {
            let logical_id = LogicalOperationIdV1(
                u64::try_from(event.plan_node.0)
                    .map_err(|_| invalid("trace plan-node id does not fit u64"))?,
            );
            let index = usize::try_from(logical_id.0)
                .map_err(|_| invalid("trace logical operation id does not fit usize"))?;
            let operation = operations
                .get_mut(index)
                .filter(|operation| operation.logical_operation == logical_id)
                .ok_or_else(|| invalid("trace event names no launch operation"))?;
            operation
                .observations
                .push(RuntimeGraphObservationV1::from_trace(
                    event,
                    logical_id,
                    &operation.task,
                    &operation.attempt,
                ));
        }

        let graph = Self {
            schema_version: RUNTIME_GRAPH_SCHEMA_V1,
            logical_hgraph_schema: LOGICAL_HGRAPH_SCHEMA_V1,
            logical_hgraph: logical_digest,
            deployment_plan_schema: DEPLOYMENT_PLAN_SCHEMA_V1,
            deployment_plan: deployment_digest,
            launch_schema: HOSTED_WORLD_LAUNCH_SCHEMA_V1,
            launch: launch_digest,
            placement_snapshot_schema: launch.placement_snapshot_schema,
            placement_snapshot: launch.placement_snapshot.clone(),
            project_name: logical.source.project_name.clone(),
            project_bundle: logical.source.bundle.clone(),
            target: logical.source.target.clone(),
            policy: logical_policy_token(&logical.source.policy),
            trace_format_version: PROJECT_ATTEMPT_TRACE_VERSION,
            trace_execution_attempt_id: trace_header.execution_attempt_id.clone(),
            world: launch.world.clone(),
            governor_context: launch.governor.clone(),
            coordinator_observer: launch.coordinator_observer.clone(),
            coordinator_attempt: launch.coordinator_attempt.clone(),
            selected_provider: launch.selected_provider.clone(),
            operations,
            terminal,
        };
        graph.validate()?;
        Ok(graph)
    }

    /// Validate structure, canonical lifecycle prefixes, exact identity nesting,
    /// and terminality. This cannot establish authority, live freshness, or
    /// digest-bound dependency/deployment semantics absent the referenced
    /// artifacts; callers with those trusted inputs must also use one of the
    /// `validate_trusted_*` methods.
    pub fn validate(&self) -> Result<(), RuntimeGraphError> {
        if self.schema_version != RUNTIME_GRAPH_SCHEMA_V1 {
            return Err(invalid(format!(
                "schema version must be {RUNTIME_GRAPH_SCHEMA_V1}, got {}",
                self.schema_version
            )));
        }
        if self.logical_hgraph_schema != LOGICAL_HGRAPH_SCHEMA_V1
            || self.deployment_plan_schema != DEPLOYMENT_PLAN_SCHEMA_V1
            || self.launch_schema != HOSTED_WORLD_LAUNCH_SCHEMA_V1
            || self.placement_snapshot_schema != PLACEMENT_SNAPSHOT_SCHEMA_V1
        {
            return Err(invalid(
                "one or more bound artifact schemas are unsupported",
            ));
        }
        for (digest, field) in [
            (&self.logical_hgraph, "logical HGraph digest"),
            (&self.deployment_plan, "deployment plan digest"),
            (&self.launch, "launch digest"),
            (&self.placement_snapshot, "placement snapshot digest"),
            (&self.project_bundle, "project bundle digest"),
        ] {
            validate_digest(digest, field)?;
        }
        validate_text(&self.project_name, "project name")?;
        validate_text(&self.target, "project target")?;
        validate_text(&self.policy, "project policy")?;
        let policy = validate_runtime_policy_token(&self.policy)?;
        validate_text(
            &self.trace_execution_attempt_id,
            "trace execution attempt id",
        )?;
        if self.trace_format_version != PROJECT_ATTEMPT_TRACE_VERSION {
            return Err(invalid(format!(
                "trace format version must be {PROJECT_ATTEMPT_TRACE_VERSION}, got {}",
                self.trace_format_version
            )));
        }
        if self.governor_context.world() != &self.world {
            return Err(invalid(
                "Governor context belongs to a different exact World",
            ));
        }
        if self.coordinator_observer.node.world() != self.world.world()
            || self.coordinator_observer.domain.node() != &self.coordinator_observer.node
            || self
                .coordinator_observer
                .process
                .as_ref()
                .is_some_and(|process| process.domain() != &self.coordinator_observer.domain)
        {
            return Err(invalid(
                "coordinator observer is not nested beneath the RuntimeGraph World",
            ));
        }
        if self.coordinator_attempt.world() != self.world.world()
            || self.trace_execution_attempt_id != self.coordinator_attempt.to_string()
        {
            return Err(invalid(
                "coordinator attempt differs from the RuntimeGraph World or trace attempt",
            ));
        }
        validate_provider(&self.selected_provider, &self.world)?;
        if self.operations.is_empty() || self.operations.len() > MAX_RUNTIME_GRAPH_OPERATIONS {
            return Err(invalid(format!(
                "operation count must be in 1..={MAX_RUNTIME_GRAPH_OPERATIONS}"
            )));
        }

        let mut trace_events = Vec::new();
        let mut tasks = BTreeSet::new();
        let mut attempts = BTreeSet::new();
        for (index, operation) in self.operations.iter().enumerate() {
            let expected = u64::try_from(index)
                .map_err(|_| invalid("runtime operation index does not fit u64"))?;
            if operation.logical_operation.0 != expected {
                return Err(invalid(format!(
                    "runtime operation index {index} has noncanonical logical id {}",
                    operation.logical_operation.0
                )));
            }
            if operation.task.world() != self.world.world()
                || operation.attempt.world() != self.world.world()
                || operation.attempt.task() != operation.task.task()
            {
                return Err(invalid(
                    "runtime operation task/attempt does not name the exact graph World and task",
                ));
            }
            if operation.task.task() == self.coordinator_attempt.task()
                || operation.attempt == self.coordinator_attempt
            {
                return Err(invalid(
                    "coordinator attempt aliases a per-operation task or attempt",
                ));
            }
            if !tasks.insert(operation.task.clone()) {
                return Err(invalid(
                    "RuntimeGraph repeats an exact task identity across operations",
                ));
            }
            if !attempts.insert(operation.attempt.clone()) {
                return Err(invalid(
                    "RuntimeGraph repeats an exact attempt identity across operations",
                ));
            }
            if operation.observations.len() > 3 {
                return Err(invalid(
                    "runtime operation has more than three lifecycle events",
                ));
            }
            if operation
                .observations
                .windows(2)
                .any(|pair| pair[0].coordinator_ordinal >= pair[1].coordinator_ordinal)
            {
                return Err(invalid(
                    "operation observations are not in strictly increasing ordinal order",
                ));
            }
            for observation in &operation.observations {
                validate_observation_operation_shape(
                    observation,
                    &self.policy,
                    index + 1 == self.operations.len(),
                )?;
                if observation.logical_operation != operation.logical_operation
                    || observation.task != operation.task
                    || observation.attempt != operation.attempt
                {
                    return Err(invalid(
                        "runtime observation changed its operation task or attempt identity",
                    ));
                }
                trace_events.push(observation.to_trace_event()?);
            }
            if matches!(
                &self.terminal,
                RuntimeGraphTerminalV1::RouteSettlement { .. }
            ) && operation
                .observations
                .last()
                .is_some_and(|observation| !observation.state.is_terminal())
            {
                return Err(invalid(
                    "project-result RuntimeGraph contains a partially observed operation",
                ));
            }
        }
        if trace_events.len() > MAX_RUNTIME_GRAPH_OBSERVATIONS {
            return Err(invalid(format!(
                "observation count exceeds {MAX_RUNTIME_GRAPH_OBSERVATIONS}"
            )));
        }
        trace_events.sort_by_key(|event| event.coordinator_ordinal);
        let header = self.trace_header();
        ProjectAttemptTrace::try_from_events(header, trace_events.iter().cloned())?;
        validate_observed_branch_sequence(&trace_events, &policy)?;
        if matches!(
            &self.terminal,
            RuntimeGraphTerminalV1::RouteSettlement { .. }
        ) && !trace_events.iter().any(|event| event.state.is_terminal())
        {
            return Err(invalid(
                "project-result RuntimeGraph has no terminal operation observation",
            ));
        }
        match &self.terminal {
            RuntimeGraphTerminalV1::RouteSettlement {
                selected_operation,
                route_id,
                disposition,
                settlement,
                outcome,
                residual_host_world,
            } => {
                if !matches!(
                    &policy,
                    RoutePolicy::Explicit(_)
                        | RoutePolicy::Default
                        | RoutePolicy::Fallback
                        | RoutePolicy::AnySuccess
                ) {
                    return Err(invalid(format!(
                        "route-settlement RuntimeGraph uses unsupported policy `{}`",
                        self.policy
                    )));
                }
                if let RoutePolicy::Explicit(expected_route) = &policy {
                    if route_id != expected_route {
                        return Err(invalid(
                            "explicit route-settlement terminal differs from its policy route",
                        ));
                    }
                }
                let selector_ready = validate_completed_selector_root(
                    self.operations
                        .last()
                        .expect("operation inventory was checked as nonempty"),
                    &self.policy,
                )?;
                validate_text(route_id, "terminal route id")?;
                if !matches!(
                    settlement,
                    ProjectAttemptState::SettledSuccess
                        | ProjectAttemptState::SettledFailure
                        | ProjectAttemptState::Skipped
                ) {
                    return Err(invalid(
                        "route-settlement RuntimeGraph terminal has a non-route settlement state",
                    ));
                }
                if matches!(disposition, RouteExecutionDisposition::GuardSkipped)
                    != matches!(settlement, ProjectAttemptState::Skipped)
                {
                    return Err(invalid(
                        "terminal route disposition disagrees with its trace settlement",
                    ));
                }
                let index = usize::try_from(selected_operation.0)
                    .map_err(|_| invalid("terminal selected operation does not fit usize"))?;
                let operation = self
                    .operations
                    .get(index)
                    .filter(|operation| operation.logical_operation == *selected_operation)
                    .ok_or_else(|| invalid("terminal names no RuntimeGraph operation"))?;
                let observed_residual = self.operations.iter().any(|operation| {
                    operation.residual_host_world
                        && operation.observations.iter().any(|observation| {
                            matches!(
                                observation.state,
                                ProjectAttemptState::Started
                                    | ProjectAttemptState::Finished
                                    | ProjectAttemptState::SettledSuccess
                                    | ProjectAttemptState::SettledFailure
                                    | ProjectAttemptState::Skipped
                                    | ProjectAttemptState::Aborted
                            )
                        })
                });
                if observed_residual != *residual_host_world {
                    return Err(invalid(
                        "terminal residual HostWorld truth differs from observed execution",
                    ));
                }
                let observation = operation
                    .observations
                    .last()
                    .ok_or_else(|| invalid("terminal selected operation was never observed"))?;
                if observation.route_id.as_deref() != Some(route_id.as_str())
                    || observation.state != *settlement
                    || observation.outcome.as_ref() != Some(outcome)
                {
                    return Err(invalid(
                        "terminal result differs from the selected operation observation",
                    ));
                }
                let selected_settlement =
                    policy_selected_route_settlement(&trace_events, selector_ready, &policy);
                if selected_settlement.map(|event| event.coordinator_ordinal)
                    != Some(observation.coordinator_ordinal)
                {
                    return Err(invalid(
                        "route-settlement terminal is not the policy-selected top-level route before selector readiness",
                    ));
                }
            }
            RuntimeGraphTerminalV1::CoordinatorFailure {
                detail_sha256,
                residual_host_world,
            } => {
                if !trace_events
                    .iter()
                    .any(|event| event.state == ProjectAttemptState::Started)
                {
                    return Err(invalid(
                        "coordinator-failure RuntimeGraph has no observed started operation",
                    ));
                }
                if self
                    .operations
                    .last()
                    .expect("operation inventory was checked as nonempty")
                    .observations
                    .iter()
                    .any(|observation| observation.state == ProjectAttemptState::Finished)
                {
                    return Err(invalid(
                        "coordinator-failure RuntimeGraph contains a completed SelectRoute root",
                    ));
                }
                validate_digest(detail_sha256, "coordinator failure detail")?;
                let observed = self.operations.iter().any(|operation| {
                    operation.residual_host_world
                        && operation.observations.iter().any(|observation| {
                            matches!(
                                observation.state,
                                ProjectAttemptState::Started
                                    | ProjectAttemptState::Finished
                                    | ProjectAttemptState::SettledSuccess
                                    | ProjectAttemptState::SettledFailure
                                    | ProjectAttemptState::Skipped
                                    | ProjectAttemptState::Aborted
                            )
                        })
                });
                if observed != *residual_host_world {
                    return Err(invalid(
                        "coordinator-failure residual HostWorld truth differs from observed starts",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Recompute the complete project-result observation, including its terminal,
    /// from the trusted Project HGraph, exact deployment, launch, checked trace,
    /// and selected route result.
    pub fn validate_trusted_project_result(
        &self,
        project: &ProjectHGraph,
        deployment: &DeploymentPlanV1,
        launch: &HostedWorldLaunchV1,
        trace: &ProjectAttemptTrace,
        result: &OExecutionResult,
    ) -> Result<(), RuntimeGraphError> {
        self.validate()?;
        let expected = Self::from_project_result(project, deployment, launch, trace, result)?;
        if self != &expected {
            return Err(invalid(
                "RuntimeGraph differs from its trusted project-result execution inputs",
            ));
        }
        Ok(())
    }

    /// Recompute a coordinator-failure observation, including the exact
    /// failure-detail digest, from a semantically replayed checked trace prefix.
    pub fn validate_trusted_coordinator_failure(
        &self,
        project: &ProjectHGraph,
        deployment: &DeploymentPlanV1,
        launch: &HostedWorldLaunchV1,
        trace: &ProjectAttemptTrace,
        failure_detail: impl AsRef<[u8]>,
    ) -> Result<(), RuntimeGraphError> {
        self.validate()?;
        let expected =
            Self::from_coordinator_failure(project, deployment, launch, trace, failure_detail)?;
        if self != &expected {
            return Err(invalid(
                "RuntimeGraph differs from its trusted coordinator-failure inputs",
            ));
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RuntimeGraphError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_RUNTIME_GRAPH_RECORD_BYTES {
            return Err(RuntimeGraphError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_RUNTIME_GRAPH_RECORD_BYTES,
            });
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, RuntimeGraphError> {
        if bytes.len() > MAX_RUNTIME_GRAPH_RECORD_BYTES {
            return Err(RuntimeGraphError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_RUNTIME_GRAPH_RECORD_BYTES,
            });
        }
        let graph: Self = serde_json::from_slice(bytes)?;
        graph.validate()?;
        Ok(graph)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, RuntimeGraphError> {
        let graph = Self::decode(bytes)?;
        if graph.canonical_bytes()? != bytes {
            return Err(RuntimeGraphError::NonCanonicalEncoding);
        }
        Ok(graph)
    }

    pub fn digest(&self) -> Result<ArtifactId, RuntimeGraphError> {
        digest_canonical(&self.canonical_bytes()?)
    }

    fn trace_header(&self) -> ProjectAttemptTraceHeader {
        ProjectAttemptTraceHeader::new(
            self.project_name.clone(),
            self.project_bundle.as_sha256().to_owned(),
            self.target.clone(),
            self.policy.clone(),
            self.logical_hgraph_schema,
            self.logical_hgraph.as_sha256().to_owned(),
            self.deployment_plan_schema,
            self.deployment_plan.as_sha256().to_owned(),
            self.trace_execution_attempt_id.clone(),
        )
    }
}

fn validate_trusted_inputs(
    project: &ProjectHGraph,
    logical: &LogicalHGraphV1,
    deployment: &DeploymentPlanV1,
    launch: &HostedWorldLaunchV1,
    trace: &ProjectAttemptTrace,
) -> Result<(), RuntimeGraphError> {
    logical.canonical_bytes()?;
    deployment.canonical_bytes()?;
    launch.validate()?;
    let trusted_logical = project.logical_v1()?;
    if logical != &trusted_logical {
        return Err(invalid(
            "RuntimeGraph logical input differs from the trusted Project HGraph",
        ));
    }
    ProjectAttemptTrace::try_from_project_events_with_deployment(
        project,
        deployment,
        trace.header().clone(),
        trace.events().iter().cloned(),
    )?;
    let trace_header = trace.header();
    let trace_events = trace.events();

    let logical_digest = logical.digest()?;
    let deployment_digest = deployment.digest()?;
    if deployment.logical_hgraph_schema != LOGICAL_HGRAPH_SCHEMA_V1
        || deployment.logical_hgraph != logical_digest
    {
        return Err(invalid(
            "deployment plan is not bound to the trusted logical HGraph",
        ));
    }
    if launch.logical_hgraph_schema != LOGICAL_HGRAPH_SCHEMA_V1
        || launch.logical_hgraph != logical_digest
        || launch.deployment_plan_schema != DEPLOYMENT_PLAN_SCHEMA_V1
        || launch.deployment_plan != deployment_digest
        || launch.project_bundle != logical.source.bundle
    {
        return Err(invalid(
            "hosted World launch is not bound to the trusted logical/deployment inputs",
        ));
    }
    if deployment.world.as_ref() != Some(&launch.world)
        || deployment.placement_snapshot.as_ref() != Some(&launch.placement_snapshot)
        || deployment.selected_provider.as_ref() != Some(&launch.selected_provider)
    {
        return Err(invalid(
            "launch World, snapshot, or provider differs from the deployment plan",
        ));
    }
    if launch.operation_attempts.len() != logical.operations.len()
        || deployment.operations.len() != logical.operations.len()
    {
        return Err(invalid(
            "logical, deployment, and launch operation inventories differ",
        ));
    }
    for (index, logical_operation) in logical.operations.iter().enumerate() {
        let deployment_operation = &deployment.operations[index];
        let launch_attempt = &launch.operation_attempts[index];
        if deployment_operation.logical_operation != logical_operation.id
            || launch_attempt.logical_operation != logical_operation.id
        {
            return Err(invalid(
                "deployment or launch operation order differs from the logical graph",
            ));
        }
        let task = deployment_operation
            .task
            .as_ref()
            .ok_or_else(|| invalid("World-bound deployment operation has no task"))?;
        if launch_attempt.attempt.world() != task.world()
            || launch_attempt.attempt.task() != task.task()
        {
            return Err(invalid(
                "launch attempt does not belong to its deployment task",
            ));
        }
    }
    for event in trace_events {
        let operation = logical
            .operations
            .get(event.plan_node.0)
            .ok_or_else(|| invalid("trace event names no logical operation"))?;
        let expected_branch = operation
            .branch
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid("logical branch does not fit trace branch identity"))?;
        let (expected_label, expected_route) = match &operation.kind {
            LogicalOperationKindV1::MaterializeProject => ("materialize-project".to_string(), None),
            LogicalOperationKindV1::BuildRoute { route_id } => {
                (format!("build-route:{route_id}"), Some(route_id.as_str()))
            }
            LogicalOperationKindV1::RunRoute { route_id } => {
                (format!("run-route:{route_id}"), Some(route_id.as_str()))
            }
            LogicalOperationKindV1::SelectRoute { policy } => (
                format!("select-route:{}", logical_policy_token(policy)),
                None,
            ),
            LogicalOperationKindV1::CompareRouteResults => {
                ("compare-route-results".to_string(), None)
            }
        };
        let expected_plan_node = usize::try_from(operation.id.0)
            .map_err(|_| invalid("logical operation id does not fit trace plan-node identity"))?;
        if expected_plan_node != event.plan_node.0
            || event.operation_label != expected_label
            || event.branch != expected_branch
            || event.route_id.as_deref() != expected_route
        {
            return Err(invalid(
                "trace event identity differs from its trusted logical operation",
            ));
        }
    }

    let expected_policy = logical_policy_token(&logical.source.policy);
    if trace_header.project_name != logical.source.project_name
        || trace_header.bundle_digest != logical.source.bundle.as_sha256()
        || trace_header.target != logical.source.target
        || trace_header.policy != expected_policy
        || trace_header.logical_graph_schema != LOGICAL_HGRAPH_SCHEMA_V1
        || trace_header.logical_graph_digest != logical_digest.as_sha256()
        || trace_header.deployment_plan_schema != DEPLOYMENT_PLAN_SCHEMA_V1
        || trace_header.deployment_plan_digest != deployment_digest.as_sha256()
        || trace_header.execution_attempt_id != launch.coordinator_attempt().to_string()
    {
        return Err(invalid(
            "project trace header differs from the trusted logical or deployment input",
        ));
    }
    Ok(())
}

fn project_result_terminal(
    project: &ProjectHGraph,
    deployment: &DeploymentPlanV1,
    trace_events: &[ProjectAttemptEvent],
    result: &OExecutionResult,
) -> Result<RuntimeGraphTerminalV1, RuntimeGraphError> {
    let outcome = ProjectRouteOutcome::from_result(result)?;
    let selection_ready = trace_events
        .iter()
        .find(|event| {
            event.state == ProjectAttemptState::Ready
                && project
                    .plan
                    .operations
                    .get(event.plan_node.0)
                    .is_some_and(|operation| {
                        matches!(
                            operation.op,
                            crate::hgraph::ExecutableOp::SelectRoute { .. }
                        )
                    })
        })
        .ok_or_else(|| invalid("project result trace has no completed selection boundary"))?;
    if !trace_events.iter().any(|event| {
        event.plan_node == selection_ready.plan_node && event.state == ProjectAttemptState::Finished
    }) {
        return Err(invalid(
            "project result trace has no finished SelectRoute root",
        ));
    }
    let alternatives = trace_events
        .iter()
        .take_while(|event| event.coordinator_ordinal < selection_ready.coordinator_ordinal)
        .filter(|event| {
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
        .collect::<Vec<_>>();
    let event = match project.plan.policy {
        RoutePolicy::Fallback | RoutePolicy::AnySuccess => alternatives
            .iter()
            .copied()
            .find(|event| event.state == ProjectAttemptState::SettledSuccess)
            .or_else(|| alternatives.last().copied()),
        RoutePolicy::Explicit(_) | RoutePolicy::Default => {
            (alternatives.len() == 1).then(|| alternatives[0])
        }
        _ => None,
    }
    .ok_or_else(|| invalid("trace does not identify one selected terminal alternative"))?;
    let expected_disposition = if event.state == ProjectAttemptState::Skipped {
        RouteExecutionDisposition::GuardSkipped
    } else {
        RouteExecutionDisposition::Executed
    };
    if event.route_id.as_deref() != Some(result.route_id.as_str())
        || event.outcome.as_ref() != Some(&outcome)
        || result.disposition != expected_disposition
        || (result.succeeded() != (event.state == ProjectAttemptState::SettledSuccess))
    {
        return Err(invalid(
            "supplied project result differs from the selected trace alternative",
        ));
    }
    let selected_operation = LogicalOperationIdV1(
        u64::try_from(event.plan_node.0)
            .map_err(|_| invalid("selected result plan-node id does not fit u64"))?,
    );
    let index = usize::try_from(selected_operation.0)
        .map_err(|_| invalid("selected result logical operation does not fit usize"))?;
    deployment
        .operations
        .get(index)
        .filter(|operation| operation.logical_operation == selected_operation)
        .ok_or_else(|| invalid("selected result names no deployment operation"))?;
    if matches!(result.disposition, RouteExecutionDisposition::GuardSkipped)
        != matches!(event.state, ProjectAttemptState::Skipped)
    {
        return Err(invalid(
            "selected result disposition disagrees with its terminal route observation",
        ));
    }
    Ok(RuntimeGraphTerminalV1::RouteSettlement {
        selected_operation,
        route_id: result.route_id.clone(),
        disposition: result.disposition,
        settlement: event.state,
        outcome,
        residual_host_world: observed_residual_host_world(deployment, trace_events)?,
    })
}

fn observed_residual_host_world(
    deployment: &DeploymentPlanV1,
    trace_events: &[ProjectAttemptEvent],
) -> Result<bool, RuntimeGraphError> {
    for event in trace_events {
        if !matches!(
            event.state,
            ProjectAttemptState::Started
                | ProjectAttemptState::Finished
                | ProjectAttemptState::SettledSuccess
                | ProjectAttemptState::SettledFailure
                | ProjectAttemptState::Skipped
                | ProjectAttemptState::Aborted
        ) {
            continue;
        }
        let index = event.plan_node.0;
        let logical_operation = LogicalOperationIdV1(
            u64::try_from(index)
                .map_err(|_| invalid("trace plan-node id does not fit logical operation id"))?,
        );
        let operation = deployment
            .operations
            .get(index)
            .filter(|operation| operation.logical_operation == logical_operation)
            .ok_or_else(|| invalid("trace event names no deployment operation"))?;
        if operation.requirements.residual_host_world {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_runtime_policy_token(policy: &str) -> Result<RoutePolicy, RuntimeGraphError> {
    let parsed = RoutePolicy::parse_checked(policy)
        .map_err(|error| invalid(format!("project policy is invalid: {error}")))?;
    if parsed.token() != policy
        || matches!(&parsed, RoutePolicy::Explicit(route_id) if route_id.is_empty())
    {
        return Err(invalid(format!(
            "project policy `{policy}` is not a canonical resolved policy token"
        )));
    }
    Ok(parsed)
}

fn validate_observed_branch_sequence(
    events: &[ProjectAttemptEvent],
    policy: &RoutePolicy,
) -> Result<(), RuntimeGraphError> {
    let branches = events
        .iter()
        .filter_map(|event| event.branch)
        .collect::<Vec<_>>();
    match policy {
        RoutePolicy::Explicit(_) | RoutePolicy::Default => {
            if branches.iter().any(|branch| *branch != 0) {
                return Err(invalid(
                    "single-alternative RuntimeGraph observation uses a nonzero branch",
                ));
            }
        }
        RoutePolicy::Fallback | RoutePolicy::AnySuccess => {
            let mut highest = 0_usize;
            let mut observed = BTreeSet::new();
            for branch in branches {
                if branch < highest {
                    return Err(invalid(
                        "ordered RuntimeGraph branch observations move backward",
                    ));
                }
                highest = highest.max(branch);
                observed.insert(branch);
            }
            if observed.iter().copied().ne(0..observed.len()) {
                return Err(invalid(
                    "ordered RuntimeGraph branches are not a contiguous prefix beginning at zero",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_observation_operation_shape(
    observation: &RuntimeGraphObservationV1,
    policy: &str,
    is_root: bool,
) -> Result<(), RuntimeGraphError> {
    let label = observation.operation_label.as_str();
    let shape_is = |branch: bool, route_id: Option<&str>| {
        observation.branch.is_some() == branch && observation.route_id.as_deref() == route_id
    };

    if is_root {
        if label != format!("select-route:{policy}") || !shape_is(false, None) {
            return Err(invalid(
                "terminal RuntimeGraph operation is not the canonical SelectRoute root",
            ));
        }
        return Ok(());
    }

    if label == "materialize-project" && shape_is(true, None) {
        return Ok(());
    }
    if label == "compare-route-results" && policy == "verify_equivalent" && shape_is(false, None) {
        return Ok(());
    }
    if let Some(route_id) = label.strip_prefix("build-route:") {
        if !route_id.is_empty() && shape_is(true, Some(route_id)) {
            return Ok(());
        }
    }
    if let Some(route_id) = label.strip_prefix("run-route:") {
        if !route_id.is_empty() && shape_is(true, Some(route_id)) {
            return Ok(());
        }
    }

    Err(invalid(format!(
        "runtime observation `{label}` has a noncanonical operation label, branch, or route identity"
    )))
}

fn validate_completed_selector_root(
    root: &RuntimeGraphOperationV1,
    policy: &str,
) -> Result<u64, RuntimeGraphError> {
    let [ready, started, finished] = root.observations.as_slice() else {
        return Err(invalid(
            "route-settlement RuntimeGraph has no completed SelectRoute root",
        ));
    };
    if ready.state != ProjectAttemptState::Ready
        || started.state != ProjectAttemptState::Started
        || finished.state != ProjectAttemptState::Finished
        || ready.operation_label != format!("select-route:{policy}")
    {
        return Err(invalid(
            "route-settlement RuntimeGraph has no canonical completed SelectRoute root",
        ));
    }
    Ok(ready.coordinator_ordinal)
}

fn is_canonical_route_settlement(event: &ProjectAttemptEvent) -> bool {
    matches!(
        event.state,
        ProjectAttemptState::SettledSuccess
            | ProjectAttemptState::SettledFailure
            | ProjectAttemptState::Skipped
    ) && event
        .route_id
        .as_ref()
        .is_some_and(|route_id| event.operation_label == format!("run-route:{route_id}"))
}

fn policy_selected_route_settlement<'a>(
    events: &'a [ProjectAttemptEvent],
    selector_ready: u64,
    policy: &RoutePolicy,
) -> Option<&'a ProjectAttemptEvent> {
    // The planner emits prerequisite RunRoute nodes before the selected route
    // for each branch. Consequently, the greatest observed RunRoute plan node
    // in a branch is its top-level alternative. Retain trace order after that
    // structural projection because fallback/any-success select the first
    // successful alternative, or the last settled alternative if none succeeds.
    let mut top_level_by_branch = BTreeMap::<usize, usize>::new();
    for event in events.iter().filter(|event| {
        event.coordinator_ordinal < selector_ready && is_canonical_route_settlement(event)
    }) {
        let branch = event.branch?;
        top_level_by_branch
            .entry(branch)
            .and_modify(|plan_node| *plan_node = (*plan_node).max(event.plan_node.0))
            .or_insert(event.plan_node.0);
    }
    let alternatives = events
        .iter()
        .filter(|event| {
            event.coordinator_ordinal < selector_ready
                && is_canonical_route_settlement(event)
                && event.branch.is_some_and(|branch| {
                    top_level_by_branch.get(&branch) == Some(&event.plan_node.0)
                })
        })
        .collect::<Vec<_>>();

    match policy {
        RoutePolicy::Explicit(_) | RoutePolicy::Default => {
            (alternatives.len() == 1).then(|| alternatives[0])
        }
        RoutePolicy::Fallback | RoutePolicy::AnySuccess => alternatives
            .iter()
            .copied()
            .find(|event| event.state == ProjectAttemptState::SettledSuccess)
            .or_else(|| alternatives.last().copied()),
        _ => None,
    }
}

fn validate_provider(
    provider: &DeploymentProviderBindingV1,
    world: &WorldIdentity,
) -> Result<(), RuntimeGraphError> {
    if provider.node.world() != world.world() || provider.domain.node() != &provider.node {
        return Err(invalid(
            "selected provider node/domain is not nested in the exact RuntimeGraph World",
        ));
    }
    if provider
        .process
        .as_ref()
        .is_some_and(|process| process.domain() != &provider.domain)
    {
        return Err(invalid(
            "selected provider process is not nested beneath its exact domain",
        ));
    }
    match provider.service.owner() {
        ResourceOwner::Domain { domain } if domain == &provider.domain => {}
        ResourceOwner::Process { process }
            if provider
                .process
                .as_ref()
                .is_some_and(|bound| bound == process) => {}
        _ => {
            return Err(invalid(
                "selected provider service is not owned by its exact domain or process",
            ));
        }
    }
    validate_digest(&provider.implementation, "provider implementation digest")
}

fn logical_policy_token(policy: &LogicalRoutePolicyV1) -> String {
    match policy {
        LogicalRoutePolicyV1::Explicit { route_id } => format!("explicit:{route_id}"),
        LogicalRoutePolicyV1::Default => "default".to_string(),
        LogicalRoutePolicyV1::Fallback => "fallback".to_string(),
        LogicalRoutePolicyV1::AnySuccess => "any_success".to_string(),
        LogicalRoutePolicyV1::RaceSuccess => "race_success".to_string(),
        LogicalRoutePolicyV1::RaceSettle => "race_settle".to_string(),
        LogicalRoutePolicyV1::All => "all".to_string(),
        LogicalRoutePolicyV1::VerifyEquivalent => "verify_equivalent".to_string(),
        LogicalRoutePolicyV1::BenchmarkAndSelect => "benchmark_and_select".to_string(),
    }
}
