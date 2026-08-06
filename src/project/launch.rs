//! Canonical, non-authorizing World-bound launch input for hosted project execution.
//!
//! A [`HostedWorldLaunchV1`] binds one trusted logical graph and its exact
//! snapshot-derived deployment to caller-supplied World identities before the
//! coordinator may materialize a workspace or spawn a child. The record is a
//! freshness fence and provenance input only: its sole profile carries no
//! Governor admission, reservation, capability, lease, or dispatch authority.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::world::{
    ArtifactId, AttemptIdentity, DomainIdentity, GovernorIdentity, NodeIdentity, ProcessIdentity,
    ReceiptIdentity, ResourceOwner, TaskIdentity, WorldIdentity, WorldIdentityError,
};

use super::deployment::{
    DeploymentPlanError, DeploymentPlanV1, DeploymentProviderBindingV1, PlacementSnapshotV1,
    DEPLOYMENT_PLAN_SCHEMA_V1, MAX_DEPLOYMENT_OPERATIONS, PLACEMENT_SNAPSHOT_SCHEMA_V1,
};
use super::logical::{
    LogicalHGraphError, LogicalHGraphV1, LogicalOperationIdV1, LOGICAL_HGRAPH_SCHEMA_V1,
};

pub const HOSTED_WORLD_LAUNCH_SCHEMA_V1: u16 = 1;
pub const HOSTED_WORLD_CURRENT_SCHEMA_V1: u16 = 1;
pub const MAX_HOSTED_WORLD_LAUNCH_BYTES: usize = 4 * 1024 * 1024;

const HOSTED_WORLD_LAUNCH_DIGEST_DOMAIN: &[u8] = b"ostadix.world.hosted-world-launch/v1\0";
const HOSTED_WORLD_CURRENT_DIGEST_DOMAIN: &[u8] = b"ostadix.world.hosted-world-current/v1\0";

#[derive(Debug, Error)]
pub enum HostedWorldLaunchError {
    #[error("invalid hosted World launch: {0}")]
    Invalid(String),
    #[error("hosted World launch JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Logical(#[from] LogicalHGraphError),
    #[error(transparent)]
    Deployment(#[from] DeploymentPlanError),
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error("hosted World launch record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("hosted World launch bytes are not the canonical encoding")]
    NonCanonicalEncoding,
}

fn invalid(reason: impl Into<String>) -> HostedWorldLaunchError {
    HostedWorldLaunchError::Invalid(reason.into())
}

fn validate_digest(digest: &ArtifactId, field: &str) -> Result<(), HostedWorldLaunchError> {
    if digest.as_sha256().bytes().all(|byte| byte == b'0') {
        return Err(invalid(format!(
            "{field} uses the reserved all-zero digest"
        )));
    }
    Ok(())
}

fn digest_canonical(domain: &[u8], bytes: &[u8]) -> Result<ArtifactId, HostedWorldLaunchError> {
    let byte_count = u64::try_from(bytes.len())
        .map_err(|_| invalid("canonical hosted World launch length does not fit u64"))?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(byte_count.to_le_bytes());
    hasher.update(bytes);
    Ok(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
}

fn validate_provider(
    provider: &DeploymentProviderBindingV1,
    world: &WorldIdentity,
    field: &str,
) -> Result<(), HostedWorldLaunchError> {
    if provider.node.world() != world.world() {
        return Err(invalid(format!(
            "{field} provider belongs to another logical World"
        )));
    }
    if provider.domain.node() != &provider.node {
        return Err(invalid(format!(
            "{field} provider domain is not nested beneath its node"
        )));
    }
    if provider
        .process
        .as_ref()
        .is_some_and(|process| process.domain() != &provider.domain)
    {
        return Err(invalid(format!(
            "{field} provider process is not nested beneath its domain"
        )));
    }
    match provider.service.owner() {
        ResourceOwner::Domain { domain } if domain == &provider.domain => {}
        ResourceOwner::Process { process }
            if provider
                .process
                .as_ref()
                .is_some_and(|bound| bound == process) => {}
        _ => {
            return Err(invalid(format!(
                "{field} provider service must be owned by its exact domain or process"
            )))
        }
    }
    validate_digest(&provider.implementation, "provider implementation digest")
}

fn validate_operation_attempts(
    attempts: &[HostedWorldOperationAttemptV1],
    world: &WorldIdentity,
    field: &str,
) -> Result<(), HostedWorldLaunchError> {
    if attempts.is_empty() || attempts.len() > MAX_DEPLOYMENT_OPERATIONS {
        return Err(invalid(format!(
            "{field} count {} is outside 1..={MAX_DEPLOYMENT_OPERATIONS}",
            attempts.len()
        )));
    }
    let mut identities = BTreeSet::new();
    let mut tasks = BTreeSet::new();
    for (index, entry) in attempts.iter().enumerate() {
        let expected =
            u64::try_from(index).map_err(|_| invalid(format!("{field} index does not fit u64")))?;
        if entry.logical_operation.0 != expected {
            return Err(invalid(format!(
                "{field} index {index} has noncanonical logical operation {}",
                entry.logical_operation.0
            )));
        }
        if entry.attempt.world() != world.world() {
            return Err(invalid(format!(
                "{field} operation {} belongs to another logical World",
                entry.logical_operation.0
            )));
        }
        if !identities.insert(&entry.attempt) {
            return Err(invalid(format!(
                "{field} repeats an exact attempt identity"
            )));
        }
        if !tasks.insert(TaskIdentity::new(
            entry.attempt.world().clone(),
            entry.attempt.task().clone(),
        )) {
            return Err(invalid(format!("{field} repeats an exact task identity")));
        }
    }
    Ok(())
}

fn validate_coordinator_observer(
    observer: &HostedWorldCoordinatorObserverV1,
    world: &crate::world::WorldId,
    field: &str,
) -> Result<(), HostedWorldLaunchError> {
    if observer.node.world() != world {
        return Err(invalid(format!(
            "{field} coordinator observer belongs to another logical World"
        )));
    }
    if observer.domain.node() != &observer.node {
        return Err(invalid(format!(
            "{field} coordinator observer domain is not nested beneath its node"
        )));
    }
    if observer
        .process
        .as_ref()
        .is_some_and(|process| process.domain() != &observer.domain)
    {
        return Err(invalid(format!(
            "{field} coordinator observer process is not nested beneath its domain"
        )));
    }
    Ok(())
}

fn tasks_from_attempts(
    attempts: &[HostedWorldOperationAttemptV1],
) -> BTreeMap<LogicalOperationIdV1, TaskIdentity> {
    attempts
        .iter()
        .map(|entry| {
            (
                entry.logical_operation,
                TaskIdentity::new(entry.attempt.world().clone(), entry.attempt.task().clone()),
            )
        })
        .collect()
}

/// The only v1 launch profile. It is descriptive and freshness-checked but
/// cannot authorize execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedWorldLaunchProfileV1 {
    NonAuthorizingHostedReference,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWorldOperationAttemptV1 {
    pub logical_operation: LogicalOperationIdV1,
    pub attempt: AttemptIdentity,
}

/// Caller-supplied World identity of the hosted coordinator observation point.
///
/// This is deliberately distinct from the deployment's proposed provider. It
/// identifies the context that emits the uncommitted receipt; it neither proves
/// that the operating system process owns the identity nor turns the provider
/// proposal into an admitted placement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWorldCoordinatorObserverV1 {
    pub node: NodeIdentity,
    pub domain: DomainIdentity,
    pub process: Option<ProcessIdentity>,
}

impl HostedWorldCoordinatorObserverV1 {
    pub fn new(
        node: NodeIdentity,
        domain: DomainIdentity,
        process: Option<ProcessIdentity>,
    ) -> Result<Self, HostedWorldLaunchError> {
        let observer = Self {
            node,
            domain,
            process,
        };
        validate_coordinator_observer(&observer, observer.node.world(), "new")?;
        Ok(observer)
    }

    fn require_current(&self, reference: &Self) -> Result<(), HostedWorldLaunchError> {
        self.node.require_current(&reference.node)?;
        self.domain.require_current(&reference.domain)?;
        match (self.process.as_ref(), reference.process.as_ref()) {
            (Some(current), Some(reference)) => current.require_current(reference)?,
            (None, None) => {}
            _ => {
                return Err(invalid(
                    "current coordinator observer process identity differs from launch",
                ))
            }
        }
        Ok(())
    }
}

/// Exact launch reference consumed at the coordinator boundary.
///
/// This record intentionally contains no bearer capability, lease, admission
/// decision, reservation, dispatch token, or governed-effect authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWorldLaunchV1 {
    pub schema_version: u16,
    pub profile: HostedWorldLaunchProfileV1,
    pub project_bundle: ArtifactId,
    pub logical_hgraph_schema: u16,
    pub logical_hgraph: ArtifactId,
    pub deployment_plan_schema: u16,
    pub deployment_plan: ArtifactId,
    pub placement_snapshot_schema: u16,
    pub placement_snapshot: ArtifactId,
    pub world: WorldIdentity,
    pub governor: GovernorIdentity,
    pub coordinator_observer: HostedWorldCoordinatorObserverV1,
    pub coordinator_attempt: AttemptIdentity,
    pub selected_provider: DeploymentProviderBindingV1,
    pub receipt: ReceiptIdentity,
    pub operation_attempts: Vec<HostedWorldOperationAttemptV1>,
}

impl HostedWorldLaunchV1 {
    /// Construct a launch from exact trusted graph/deployment/snapshot inputs.
    ///
    /// The attempt map supplies identity only. It is converted back to the
    /// exact task map and used to re-derive the snapshot deployment, so neither
    /// task binding nor provider selection is accepted from this launch record.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        logical: &LogicalHGraphV1,
        deployment: &DeploymentPlanV1,
        snapshot: &PlacementSnapshotV1,
        governor: GovernorIdentity,
        coordinator_observer: HostedWorldCoordinatorObserverV1,
        coordinator_attempt: AttemptIdentity,
        receipt: ReceiptIdentity,
        attempts: &BTreeMap<LogicalOperationIdV1, AttemptIdentity>,
    ) -> Result<Self, HostedWorldLaunchError> {
        logical.validate()?;
        snapshot.validate()?;
        if attempts.len() != logical.operations.len()
            || logical
                .operations
                .iter()
                .any(|operation| !attempts.contains_key(&operation.id))
        {
            return Err(invalid(
                "launch requires exactly one caller-supplied attempt identity per logical operation",
            ));
        }
        let operation_attempts = logical
            .operations
            .iter()
            .map(|operation| HostedWorldOperationAttemptV1 {
                logical_operation: operation.id,
                attempt: attempts[&operation.id].clone(),
            })
            .collect::<Vec<_>>();
        let tasks = tasks_from_attempts(&operation_attempts);
        deployment.validate_trusted_snapshot(logical, snapshot, &tasks)?;
        let selected_provider = deployment.selected_provider.clone().ok_or_else(|| {
            invalid("snapshot-derived deployment has no compatible selected provider")
        })?;
        let launch = Self {
            schema_version: HOSTED_WORLD_LAUNCH_SCHEMA_V1,
            profile: HostedWorldLaunchProfileV1::NonAuthorizingHostedReference,
            project_bundle: logical.source.bundle.clone(),
            logical_hgraph_schema: logical.schema_version,
            logical_hgraph: logical.digest()?,
            deployment_plan_schema: deployment.schema_version,
            deployment_plan: deployment.digest()?,
            placement_snapshot_schema: snapshot.schema_version,
            placement_snapshot: snapshot.digest()?,
            world: snapshot.world.clone(),
            governor,
            coordinator_observer,
            coordinator_attempt,
            selected_provider,
            receipt,
            operation_attempts,
        };
        launch.validate_trusted(logical, deployment, snapshot)?;
        Ok(launch)
    }

    /// Validate the bounded wire record without treating it as authority or
    /// proving that its generations are still current.
    pub fn validate(&self) -> Result<(), HostedWorldLaunchError> {
        if self.schema_version != HOSTED_WORLD_LAUNCH_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported launch schema {}",
                self.schema_version
            )));
        }
        if self.profile != HostedWorldLaunchProfileV1::NonAuthorizingHostedReference {
            return Err(invalid(
                "launch profile is not the non-authorizing hosted reference",
            ));
        }
        if self.logical_hgraph_schema != LOGICAL_HGRAPH_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported logical HGraph schema binding {}",
                self.logical_hgraph_schema
            )));
        }
        if self.deployment_plan_schema != DEPLOYMENT_PLAN_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported deployment-plan schema binding {}",
                self.deployment_plan_schema
            )));
        }
        if self.placement_snapshot_schema != PLACEMENT_SNAPSHOT_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported placement-snapshot schema binding {}",
                self.placement_snapshot_schema
            )));
        }
        for (digest, field) in [
            (&self.project_bundle, "project bundle digest"),
            (&self.logical_hgraph, "logical HGraph digest"),
            (&self.deployment_plan, "deployment plan digest"),
            (&self.placement_snapshot, "placement snapshot digest"),
        ] {
            validate_digest(digest, field)?;
        }
        if self.governor.world() != &self.world {
            return Err(invalid(
                "Governor identity is not at the launch World epoch",
            ));
        }
        if self.receipt.world() != self.world.world() {
            return Err(invalid("receipt identity belongs to another logical World"));
        }
        validate_coordinator_observer(&self.coordinator_observer, self.world.world(), "launch")?;
        if self.coordinator_attempt.world() != self.world.world() {
            return Err(invalid(
                "coordinator attempt belongs to another logical World",
            ));
        }
        validate_provider(&self.selected_provider, &self.world, "launch")?;
        validate_operation_attempts(&self.operation_attempts, &self.world, "launch attempts")?;
        if self.operation_attempts.iter().any(|entry| {
            entry.attempt == self.coordinator_attempt
                || entry.attempt.task() == self.coordinator_attempt.task()
        }) {
            return Err(invalid(
                "coordinator attempt must use a task identity distinct from every logical operation",
            ));
        }
        Ok(())
    }

    /// Re-derive all source, task, deployment, placement, and provider bindings
    /// from trusted inputs. This is still not Governor authorization.
    pub fn validate_trusted(
        &self,
        logical: &LogicalHGraphV1,
        deployment: &DeploymentPlanV1,
        snapshot: &PlacementSnapshotV1,
    ) -> Result<(), HostedWorldLaunchError> {
        self.validate()?;
        logical.validate()?;
        snapshot.validate()?;
        if self.operation_attempts.len() != logical.operations.len()
            || self
                .operation_attempts
                .iter()
                .zip(&logical.operations)
                .any(|(attempt, operation)| attempt.logical_operation != operation.id)
        {
            return Err(invalid(
                "launch attempts differ from the trusted logical operations",
            ));
        }
        let tasks = tasks_from_attempts(&self.operation_attempts);
        deployment.validate_trusted_snapshot(logical, snapshot, &tasks)?;
        let expected_provider = deployment.selected_provider.as_ref().ok_or_else(|| {
            invalid("trusted snapshot-derived deployment has no selected provider")
        })?;
        let expected_logical = logical.digest()?;
        let expected_deployment = deployment.digest()?;
        let expected_snapshot = snapshot.digest()?;
        if self.project_bundle != logical.source.bundle
            || self.logical_hgraph_schema != logical.schema_version
            || self.logical_hgraph != expected_logical
            || self.deployment_plan_schema != deployment.schema_version
            || self.deployment_plan != expected_deployment
            || self.placement_snapshot_schema != snapshot.schema_version
            || self.placement_snapshot != expected_snapshot
            || self.world != snapshot.world
            || &self.selected_provider != expected_provider
        {
            return Err(invalid(
                "launch differs from the trusted logical source, deployment, or placement snapshot",
            ));
        }
        Ok(())
    }

    /// Fence every launch generation against a caller-supplied current view.
    pub fn validate_current(
        &self,
        current: &HostedWorldCurrentV1,
    ) -> Result<(), HostedWorldLaunchError> {
        self.validate()?;
        current.validate()?;
        current.world.require_current(&self.world)?;
        current.governor.require_current(&self.governor)?;
        current
            .coordinator_observer
            .require_current(&self.coordinator_observer)?;
        current
            .coordinator_attempt
            .require_current(&self.coordinator_attempt)?;
        current
            .selected_provider
            .node
            .require_current(&self.selected_provider.node)?;
        current
            .selected_provider
            .domain
            .require_current(&self.selected_provider.domain)?;
        match (
            current.selected_provider.process.as_ref(),
            self.selected_provider.process.as_ref(),
        ) {
            (Some(current), Some(reference)) => current.require_current(reference)?,
            (None, None) => {}
            _ => {
                return Err(invalid(
                    "current provider process identity differs from launch",
                ))
            }
        }
        current
            .selected_provider
            .service
            .require_current(&self.selected_provider.service)?;
        if current.selected_provider.implementation != self.selected_provider.implementation {
            return Err(invalid(
                "current provider implementation digest differs from launch",
            ));
        }
        if current.operation_attempts.len() != self.operation_attempts.len() {
            return Err(invalid(
                "current attempt set differs from the launch attempt set",
            ));
        }
        for (current, reference) in current
            .operation_attempts
            .iter()
            .zip(&self.operation_attempts)
        {
            if current.logical_operation != reference.logical_operation {
                return Err(invalid(
                    "current attempt operation differs from the launch operation",
                ));
            }
            current.attempt.require_current(&reference.attempt)?;
        }
        Ok(())
    }

    pub const fn profile(&self) -> HostedWorldLaunchProfileV1 {
        self.profile
    }

    pub fn project_bundle(&self) -> &ArtifactId {
        &self.project_bundle
    }

    pub fn logical_hgraph(&self) -> &ArtifactId {
        &self.logical_hgraph
    }

    pub fn deployment_plan(&self) -> &ArtifactId {
        &self.deployment_plan
    }

    pub fn placement_snapshot(&self) -> &ArtifactId {
        &self.placement_snapshot
    }

    pub fn world(&self) -> &WorldIdentity {
        &self.world
    }

    pub fn governor(&self) -> &GovernorIdentity {
        &self.governor
    }

    pub fn coordinator_observer(&self) -> &HostedWorldCoordinatorObserverV1 {
        &self.coordinator_observer
    }

    pub fn coordinator_attempt(&self) -> &AttemptIdentity {
        &self.coordinator_attempt
    }

    pub fn selected_provider(&self) -> &DeploymentProviderBindingV1 {
        &self.selected_provider
    }

    pub fn receipt(&self) -> &ReceiptIdentity {
        &self.receipt
    }

    pub fn operation_attempts(&self) -> &[HostedWorldOperationAttemptV1] {
        &self.operation_attempts
    }

    pub fn operation_attempt(
        &self,
        operation: LogicalOperationIdV1,
    ) -> Option<&HostedWorldOperationAttemptV1> {
        self.operation_attempts
            .get(usize::try_from(operation.0).ok()?)
            .filter(|entry| entry.logical_operation == operation)
    }

    pub fn tasks(&self) -> BTreeMap<LogicalOperationIdV1, TaskIdentity> {
        tasks_from_attempts(&self.operation_attempts)
    }

    pub fn attempt_for(&self, operation: LogicalOperationIdV1) -> Option<&AttemptIdentity> {
        self.operation_attempt(operation)
            .map(|entry| &entry.attempt)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, HostedWorldLaunchError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_HOSTED_WORLD_LAUNCH_BYTES {
            return Err(HostedWorldLaunchError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_HOSTED_WORLD_LAUNCH_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decode a structurally valid record. Trusted source and freshness checks
    /// remain explicit calls to [`Self::validate_trusted`] and
    /// [`Self::validate_current`].
    pub fn decode(bytes: &[u8]) -> Result<Self, HostedWorldLaunchError> {
        if bytes.len() > MAX_HOSTED_WORLD_LAUNCH_BYTES {
            return Err(HostedWorldLaunchError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_HOSTED_WORLD_LAUNCH_BYTES,
            });
        }
        let launch: Self = serde_json::from_slice(bytes)?;
        launch.validate()?;
        Ok(launch)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, HostedWorldLaunchError> {
        let launch = Self::decode(bytes)?;
        if launch.canonical_bytes()? != bytes {
            return Err(HostedWorldLaunchError::NonCanonicalEncoding);
        }
        Ok(launch)
    }

    pub fn digest(&self) -> Result<ArtifactId, HostedWorldLaunchError> {
        digest_canonical(HOSTED_WORLD_LAUNCH_DIGEST_DOMAIN, &self.canonical_bytes()?)
    }
}

/// Caller-supplied exact current generations used to fence a launch.
///
/// This view is neither an authenticated membership statement nor authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWorldCurrentV1 {
    pub schema_version: u16,
    pub world: WorldIdentity,
    pub governor: GovernorIdentity,
    pub coordinator_observer: HostedWorldCoordinatorObserverV1,
    pub coordinator_attempt: AttemptIdentity,
    pub selected_provider: DeploymentProviderBindingV1,
    pub operation_attempts: Vec<HostedWorldOperationAttemptV1>,
}

impl HostedWorldCurrentV1 {
    pub fn new(
        world: WorldIdentity,
        governor: GovernorIdentity,
        coordinator_observer: HostedWorldCoordinatorObserverV1,
        coordinator_attempt: AttemptIdentity,
        selected_provider: DeploymentProviderBindingV1,
        mut operation_attempts: Vec<HostedWorldOperationAttemptV1>,
    ) -> Result<Self, HostedWorldLaunchError> {
        operation_attempts.sort_by_key(|entry| entry.logical_operation);
        let current = Self {
            schema_version: HOSTED_WORLD_CURRENT_SCHEMA_V1,
            world,
            governor,
            coordinator_observer,
            coordinator_attempt,
            selected_provider,
            operation_attempts,
        };
        current.validate()?;
        Ok(current)
    }

    pub fn from_launch(launch: &HostedWorldLaunchV1) -> Result<Self, HostedWorldLaunchError> {
        launch.validate()?;
        Self::new(
            launch.world.clone(),
            launch.governor.clone(),
            launch.coordinator_observer.clone(),
            launch.coordinator_attempt.clone(),
            launch.selected_provider.clone(),
            launch.operation_attempts.clone(),
        )
    }

    pub fn validate(&self) -> Result<(), HostedWorldLaunchError> {
        if self.schema_version != HOSTED_WORLD_CURRENT_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported hosted World current-state schema {}",
                self.schema_version
            )));
        }
        if self.governor.world() != &self.world {
            return Err(invalid(
                "current Governor identity is not at the current World epoch",
            ));
        }
        validate_coordinator_observer(&self.coordinator_observer, self.world.world(), "current")?;
        if self.coordinator_attempt.world() != self.world.world() {
            return Err(invalid(
                "current coordinator attempt belongs to another logical World",
            ));
        }
        validate_provider(&self.selected_provider, &self.world, "current")?;
        validate_operation_attempts(&self.operation_attempts, &self.world, "current attempts")?;
        if self.operation_attempts.iter().any(|entry| {
            entry.attempt == self.coordinator_attempt
                || entry.attempt.task() == self.coordinator_attempt.task()
        }) {
            return Err(invalid(
                "current coordinator attempt must use a distinct task identity",
            ));
        }
        Ok(())
    }

    pub fn world(&self) -> &WorldIdentity {
        &self.world
    }

    pub fn governor(&self) -> &GovernorIdentity {
        &self.governor
    }

    pub fn coordinator_observer(&self) -> &HostedWorldCoordinatorObserverV1 {
        &self.coordinator_observer
    }

    pub fn coordinator_attempt(&self) -> &AttemptIdentity {
        &self.coordinator_attempt
    }

    pub fn selected_provider(&self) -> &DeploymentProviderBindingV1 {
        &self.selected_provider
    }

    pub fn operation_attempts(&self) -> &[HostedWorldOperationAttemptV1] {
        &self.operation_attempts
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, HostedWorldLaunchError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_HOSTED_WORLD_LAUNCH_BYTES {
            return Err(HostedWorldLaunchError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_HOSTED_WORLD_LAUNCH_BYTES,
            });
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HostedWorldLaunchError> {
        if bytes.len() > MAX_HOSTED_WORLD_LAUNCH_BYTES {
            return Err(HostedWorldLaunchError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_HOSTED_WORLD_LAUNCH_BYTES,
            });
        }
        let current: Self = serde_json::from_slice(bytes)?;
        current.validate()?;
        Ok(current)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, HostedWorldLaunchError> {
        let current = Self::decode(bytes)?;
        if current.canonical_bytes()? != bytes {
            return Err(HostedWorldLaunchError::NonCanonicalEncoding);
        }
        Ok(current)
    }

    pub fn digest(&self) -> Result<ArtifactId, HostedWorldLaunchError> {
        digest_canonical(HOSTED_WORLD_CURRENT_DIGEST_DOMAIN, &self.canonical_bytes()?)
    }
}
