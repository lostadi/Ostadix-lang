//! Canonical World PR8 deployment-intent schema for project logical graphs.
//!
//! A deployment plan describes intention. It is neither a runtime observation
//! nor an authority grant. The ordinary hosted profile keeps project work on
//! explicit ambient/coordinator bindings. A provider proposal is derived only
//! from a caller-supplied, exact [`PlacementSnapshotV1`] plus caller-supplied
//! [`TaskIdentity`] values; neither identity nor provider generations are
//! invented from route labels. Even then, the snapshot is descriptive input,
//! not proof that a Governor admitted or still considers the placement current.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::world::{
    ArtifactId, DomainIdentity, NodeIdentity, ProcessIdentity, ResourceIdentity, ResourceOwner,
    TaskIdentity, WorldIdentity, WorldIdentityError,
};

use super::logical::{
    LogicalArtifactRefV1, LogicalArtifactRoleV1, LogicalAuthorityRequirementV1, LogicalHGraphError,
    LogicalHGraphV1, LogicalOperationIdV1, LogicalOperationKindV1, LogicalOperationV1,
    LogicalResourceV1, LogicalRouteGuardV1, LogicalRouteKindV1, LogicalRoutePolicyV1,
    LOGICAL_HGRAPH_SCHEMA_V1,
};

pub const PLACEMENT_SNAPSHOT_SCHEMA_V1: u16 = 1;
pub const DEPLOYMENT_PLAN_SCHEMA_V1: u16 = 1;
pub const MAX_DEPLOYMENT_RECORD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DEPLOYMENT_PROVIDERS: usize = 16_384;
pub const MAX_DEPLOYMENT_OPERATIONS: usize = 65_536;
pub const MAX_DEPLOYMENT_REQUIREMENTS: usize = 4_096;

const MAX_DEPLOYMENT_TEXT_BYTES: usize = 4_096;
const PLACEMENT_SNAPSHOT_DIGEST_DOMAIN: &[u8] = b"ostadix.world.placement-snapshot/v1\0";
const DEPLOYMENT_PLAN_DIGEST_DOMAIN: &[u8] = b"ostadix.world.deployment-plan/v1\0";

#[derive(Debug, Error)]
pub enum DeploymentPlanError {
    #[error("invalid World PR8 deployment record: {0}")]
    Invalid(String),
    #[error("World PR8 deployment JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Logical(#[from] LogicalHGraphError),
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error("World PR8 deployment record is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge { actual: usize, maximum: usize },
    #[error("World PR8 deployment bytes are not the canonical encoding")]
    NonCanonicalEncoding,
}

fn invalid(reason: impl Into<String>) -> DeploymentPlanError {
    DeploymentPlanError::Invalid(reason.into())
}

fn validate_text(value: &str, field: &str) -> Result<(), DeploymentPlanError> {
    if value.is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    if value.len() > MAX_DEPLOYMENT_TEXT_BYTES {
        return Err(invalid(format!(
            "{field} exceeds {MAX_DEPLOYMENT_TEXT_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(format!("{field} contains a control character")));
    }
    Ok(())
}

fn validate_digest(digest: &ArtifactId, field: &str) -> Result<(), DeploymentPlanError> {
    if digest.as_sha256().bytes().all(|byte| byte == b'0') {
        return Err(invalid(format!(
            "{field} uses the reserved all-zero digest"
        )));
    }
    Ok(())
}

fn ensure_strict_order<T: Ord>(values: &[T], field: &str) -> Result<(), DeploymentPlanError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid(format!(
            "{field} must be strictly ordered without duplicates"
        )));
    }
    Ok(())
}

fn ensure_text_order(values: &[String], field: &str) -> Result<(), DeploymentPlanError> {
    if values.len() > MAX_DEPLOYMENT_REQUIREMENTS {
        return Err(invalid(format!(
            "{field} count exceeds {MAX_DEPLOYMENT_REQUIREMENTS}"
        )));
    }
    ensure_strict_order(values, field)?;
    for value in values {
        validate_text(value, field)?;
    }
    Ok(())
}

fn digest_canonical(domain: &[u8], bytes: &[u8]) -> Result<ArtifactId, DeploymentPlanError> {
    let byte_count = u64::try_from(bytes.len())
        .map_err(|_| invalid("canonical deployment length does not fit u64"))?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(byte_count.to_le_bytes());
    hasher.update(bytes);
    Ok(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
}

/// Reject provider metadata that contains contradictory generations for one
/// logical hierarchy slot. This proves internal record coherence only; it does
/// not establish that any described generation is current.
fn validate_provider_generation_coherence<'a>(
    bindings: impl IntoIterator<Item = &'a DeploymentProviderBindingV1>,
) -> Result<(), DeploymentPlanError> {
    let mut nodes = BTreeMap::new();
    let mut domains = BTreeMap::new();
    let mut processes = BTreeMap::new();
    let mut services = BTreeMap::new();
    for binding in bindings {
        let node_key = (binding.node.world().clone(), binding.node.node().clone());
        if nodes
            .insert(node_key, binding.node.clone())
            .is_some_and(|existing| existing != binding.node)
        {
            return Err(invalid(
                "placement contains conflicting generations for one logical node",
            ));
        }
        let domain_key = (
            binding.domain.node().clone(),
            binding.domain.domain().clone(),
        );
        if domains
            .insert(domain_key, binding.domain.clone())
            .is_some_and(|existing| existing != binding.domain)
        {
            return Err(invalid(
                "placement contains conflicting generations for one logical domain",
            ));
        }
        if let Some(process) = &binding.process {
            let process_key = (process.domain().clone(), process.process().clone());
            if processes
                .insert(process_key, process.clone())
                .is_some_and(|existing| existing != *process)
            {
                return Err(invalid(
                    "placement contains conflicting generations for one logical process",
                ));
            }
        }
        let service_key = (
            binding.service.owner().clone(),
            binding.service.resource().clone(),
        );
        if services
            .insert(service_key, binding.service.clone())
            .is_some_and(|existing| existing != binding.service)
        {
            return Err(invalid(
                "placement contains conflicting generations for one logical service",
            ));
        }
    }
    Ok(())
}

/// A generation-bound provider proposal composed from existing constitutional
/// identity atoms. `service` is descriptive resource identity, not a bearer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProviderBindingV1 {
    pub node: NodeIdentity,
    pub domain: DomainIdentity,
    pub process: Option<ProcessIdentity>,
    pub service: ResourceIdentity,
    pub implementation: ArtifactId,
}

impl DeploymentProviderBindingV1 {
    fn validate(&self) -> Result<(), DeploymentPlanError> {
        if self.domain.node() != &self.node {
            return Err(invalid(
                "deployment provider domain is not nested beneath its node",
            ));
        }
        if self
            .process
            .as_ref()
            .is_some_and(|process| process.domain() != &self.domain)
        {
            return Err(invalid(
                "deployment provider process is not nested beneath its domain",
            ));
        }
        match self.service.owner() {
            ResourceOwner::Domain { domain } if domain == &self.domain => {}
            ResourceOwner::Process { process }
                if self.process.as_ref().is_some_and(|bound| bound == process) => {}
            _ => {
                return Err(invalid(
                    "deployment provider service must be owned by its exact domain or process",
                ))
            }
        }
        validate_digest(&self.implementation, "provider implementation digest")
    }

    fn world_matches(&self, world: &WorldIdentity) -> bool {
        self.node.world() == world.world()
    }
}

/// One provider description observed in a caller-supplied placement snapshot.
/// Compatibility fields are facts supplied to planning, not authority or live
/// health attestations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProviderSnapshotV1 {
    pub binding: DeploymentProviderBindingV1,
    pub architecture: String,
    pub platform_os: String,
    pub runtime_classes: Vec<String>,
    pub executables: Vec<String>,
    pub evaluators: Vec<String>,
    pub environment_keys: Vec<String>,
    pub packages: Vec<ArtifactId>,
    /// Exact project bundles this provider declares it can materialize or has
    /// received. This is descriptive input, not an object reservation.
    pub project_bundles: Vec<ArtifactId>,
    /// Role-specific path declarations: `Input` means available after
    /// materialization; `Output` means an accepted destination. These are not
    /// governed object locations and carry no `ObjectIdentity` or version.
    pub project_paths: Vec<DeploymentProjectPathV1>,
    pub failure_domain: String,
    /// Whether this provider is willing to host operations whose effects still
    /// include ambient `HostWorld`. This is descriptive compatibility only.
    pub admits_host_world: bool,
}

impl DeploymentProviderSnapshotV1 {
    fn validate(&self, world: &WorldIdentity) -> Result<(), DeploymentPlanError> {
        self.binding.validate()?;
        if !self.binding.world_matches(world) {
            return Err(invalid(
                "placement provider belongs to a different logical World",
            ));
        }
        validate_text(&self.architecture, "provider architecture")?;
        validate_text(&self.platform_os, "provider platform OS")?;
        validate_text(&self.failure_domain, "provider failure domain")?;
        ensure_text_order(&self.runtime_classes, "provider runtime classes")?;
        ensure_text_order(&self.executables, "provider executables")?;
        ensure_text_order(&self.evaluators, "provider evaluators")?;
        ensure_text_order(&self.environment_keys, "provider environment keys")?;
        if self.packages.len() > MAX_DEPLOYMENT_REQUIREMENTS {
            return Err(invalid("provider package count exceeds the limit"));
        }
        ensure_strict_order(&self.packages, "provider packages")?;
        if self.project_bundles.len() > MAX_DEPLOYMENT_REQUIREMENTS {
            return Err(invalid("provider project-bundle count exceeds the limit"));
        }
        ensure_strict_order(&self.project_bundles, "provider project bundles")?;
        if self.project_paths.len() > MAX_DEPLOYMENT_REQUIREMENTS {
            return Err(invalid("provider project-path count exceeds the limit"));
        }
        ensure_strict_order(&self.project_paths, "provider project paths")?;
        for package in &self.packages {
            validate_digest(package, "provider package digest")?;
        }
        for bundle in &self.project_bundles {
            validate_digest(bundle, "provider project bundle digest")?;
        }
        for path in &self.project_paths {
            validate_digest(&path.bundle, "provider project-path bundle digest")?;
            validate_text(&path.artifact.path, "provider project path")?;
            if self.project_bundles.binary_search(&path.bundle).is_err() {
                return Err(invalid(
                    "provider project path names an undeclared project bundle",
                ));
            }
        }
        Ok(())
    }
}

/// Exact, digestible placement input. It is not authenticated membership and
/// does not itself authorize a provider or claim that the observation is live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementSnapshotV1 {
    pub schema_version: u16,
    pub world: WorldIdentity,
    pub providers: Vec<DeploymentProviderSnapshotV1>,
}

impl PlacementSnapshotV1 {
    pub fn new(
        world: WorldIdentity,
        mut providers: Vec<DeploymentProviderSnapshotV1>,
    ) -> Result<Self, DeploymentPlanError> {
        providers.sort_by(|left, right| left.binding.service.cmp(&right.binding.service));
        let snapshot = Self {
            schema_version: PLACEMENT_SNAPSHOT_SCHEMA_V1,
            world,
            providers,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Validate structural and internal generation coherence only. This does
    /// not authenticate the snapshot or prove that its observations are live.
    pub fn validate(&self) -> Result<(), DeploymentPlanError> {
        if self.schema_version != PLACEMENT_SNAPSHOT_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported placement snapshot schema {}",
                self.schema_version
            )));
        }
        if self.providers.len() > MAX_DEPLOYMENT_PROVIDERS {
            return Err(invalid(format!(
                "placement snapshot has {} providers; maximum is {MAX_DEPLOYMENT_PROVIDERS}",
                self.providers.len()
            )));
        }
        if self
            .providers
            .windows(2)
            .any(|pair| pair[0].binding.service >= pair[1].binding.service)
        {
            return Err(invalid(
                "placement providers must have unique, strictly ordered exact service identities",
            ));
        }
        for provider in &self.providers {
            provider.validate(&self.world)?;
        }
        validate_provider_generation_coherence(
            self.providers.iter().map(|provider| &provider.binding),
        )?;
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, DeploymentPlanError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_DEPLOYMENT_RECORD_BYTES {
            return Err(DeploymentPlanError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_DEPLOYMENT_RECORD_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decode a structurally valid snapshot. Callers must establish snapshot
    /// provenance and freshness outside this descriptive schema.
    pub fn decode(bytes: &[u8]) -> Result<Self, DeploymentPlanError> {
        if bytes.len() > MAX_DEPLOYMENT_RECORD_BYTES {
            return Err(DeploymentPlanError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_DEPLOYMENT_RECORD_BYTES,
            });
        }
        let snapshot: Self = serde_json::from_slice(bytes)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Decode exact canonical bytes without authenticating their source.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, DeploymentPlanError> {
        let snapshot = Self::decode(bytes)?;
        if snapshot.canonical_bytes()? != bytes {
            return Err(DeploymentPlanError::NonCanonicalEncoding);
        }
        Ok(snapshot)
    }

    pub fn digest(&self) -> Result<ArtifactId, DeploymentPlanError> {
        digest_canonical(PLACEMENT_SNAPSHOT_DIGEST_DOMAIN, &self.canonical_bytes()?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploymentArchitectureRequirementV1 {
    Unspecified,
    Exact { architecture: String },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploymentFailureDomainConstraintV1 {
    Require { failure_domain: String },
    Avoid { failure_domain: String },
}

/// One role/path declaration scoped to the exact project bundle whose
/// materialization gives that path meaning. This is still not an ObjectId.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProjectPathV1 {
    pub bundle: ArtifactId,
    pub artifact: LogicalArtifactRefV1,
}

/// Requirements copied or deterministically derived from one logical
/// operation. Empty authority/failure/package sets remain explicit; planning
/// must not manufacture their satisfaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentOperationRequirementsV1 {
    pub project_bundle: ArtifactId,
    pub architecture: DeploymentArchitectureRequirementV1,
    pub runtime_classes: Vec<String>,
    pub executables: Vec<String>,
    pub evaluators: Vec<String>,
    pub platform_os: Vec<String>,
    /// Environment keys whose values are supplied by the bundle itself. They
    /// are source-bound configuration, not provider inventory requirements.
    pub environment_overlay_keys: Vec<String>,
    /// Ambient environment names required by explicit `EnvVarSet` guards.
    pub environment_keys: Vec<String>,
    pub packages: Vec<ArtifactId>,
    pub locality: Vec<DeploymentProjectPathV1>,
    pub authority: Vec<LogicalAuthorityRequirementV1>,
    pub failure_domains: Vec<DeploymentFailureDomainConstraintV1>,
    pub residual_host_world: bool,
    /// False means the logical layer lacks enough command/evaluator facts to
    /// license a snapshot-derived provider match. Hosted ambient execution can
    /// remain explicit, but provider proposal must fail closed.
    pub runtime_contract_complete: bool,
}

impl DeploymentOperationRequirementsV1 {
    fn validate(&self) -> Result<(), DeploymentPlanError> {
        validate_digest(&self.project_bundle, "required project bundle digest")?;
        if let DeploymentArchitectureRequirementV1::Exact { architecture } = &self.architecture {
            validate_text(architecture, "required architecture")?;
        }
        ensure_text_order(&self.runtime_classes, "required runtime classes")?;
        ensure_text_order(&self.executables, "required executables")?;
        ensure_text_order(&self.evaluators, "required evaluators")?;
        ensure_text_order(&self.platform_os, "required platform OS values")?;
        ensure_text_order(
            &self.environment_overlay_keys,
            "bundle environment overlay keys",
        )?;
        ensure_text_order(&self.environment_keys, "required environment keys")?;
        if self.packages.len() > MAX_DEPLOYMENT_REQUIREMENTS
            || self.locality.len() > MAX_DEPLOYMENT_REQUIREMENTS
            || self.authority.len() > MAX_DEPLOYMENT_REQUIREMENTS
            || self.failure_domains.len() > MAX_DEPLOYMENT_REQUIREMENTS
        {
            return Err(invalid("operation requirement count exceeds the limit"));
        }
        ensure_strict_order(&self.packages, "required packages")?;
        ensure_strict_order(&self.locality, "locality requirements")?;
        ensure_strict_order(&self.authority, "authority requirements")?;
        ensure_strict_order(&self.failure_domains, "failure-domain constraints")?;
        for package in &self.packages {
            validate_digest(package, "required package digest")?;
        }
        for locality in &self.locality {
            validate_digest(&locality.bundle, "locality bundle digest")?;
            validate_text(&locality.artifact.path, "locality path")?;
            if locality.bundle != self.project_bundle {
                return Err(invalid(
                    "locality path belongs to a different project bundle",
                ));
            }
        }
        for authority in &self.authority {
            validate_text(&authority.right, "authority right")?;
        }
        for constraint in &self.failure_domains {
            let failure_domain = match constraint {
                DeploymentFailureDomainConstraintV1::Require { failure_domain }
                | DeploymentFailureDomainConstraintV1::Avoid { failure_domain } => failure_domain,
            };
            validate_text(failure_domain, "failure-domain constraint")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploymentCompatibilityIssueV1 {
    MissingPlacementSnapshot,
    MissingWorldReference,
    MissingTaskIdentity,
    RuntimeContractIncomplete,
    ArchitectureMismatch { required: String, available: String },
    PlatformOsMismatch { required: String, available: String },
    MissingRuntime { runtime_class: String },
    MissingExecutable { executable: String },
    MissingEvaluator { evaluator: String },
    MissingEnvironmentKey { name: String },
    MissingPackage { package: ArtifactId },
    MissingProjectBundle { bundle: ArtifactId },
    MissingProjectPath { path: DeploymentProjectPathV1 },
    AuthorityBrokerRequired,
    FailureDomainMismatch { failure_domain: String },
    ResidualHostWorldDenied,
    NoCompatibleProvider,
    UnsupportedHostedPolicy { policy: String },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProviderIssueV1 {
    pub operation: LogicalOperationIdV1,
    pub issue: DeploymentCompatibilityIssueV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProviderRejectionV1 {
    pub provider: DeploymentProviderBindingV1,
    pub issues: Vec<DeploymentProviderIssueV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
// Preserve the frozen inline v1 wire/API shape; boxing only the provider arm
// would trade a public schema type change for an allocation-size lint.
#[allow(clippy::large_enum_variant)]
pub enum DeploymentOperationBindingV1 {
    /// Current in-process coordinator work. This carries no World placement.
    HostedCoordinator,
    /// Current subprocess/workspace execution with explicit residual host
    /// effects. This carries no World placement or authority.
    AmbientHost,
    Unresolved {
        issues: Vec<DeploymentCompatibilityIssueV1>,
    },
    /// Deterministic provider proposal derived from descriptive snapshot data.
    /// It is not Governor admission, reservation, dispatch, or runtime proof.
    ProposedProvider {
        provider: DeploymentProviderBindingV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentOperationV1 {
    pub logical_operation: LogicalOperationIdV1,
    pub task: Option<TaskIdentity>,
    pub requirements: DeploymentOperationRequirementsV1,
    pub binding: DeploymentOperationBindingV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentPlanV1 {
    pub schema_version: u16,
    pub logical_hgraph_schema: u16,
    pub logical_hgraph: ArtifactId,
    pub world: Option<WorldIdentity>,
    pub placement_snapshot: Option<ArtifactId>,
    pub selected_provider: Option<DeploymentProviderBindingV1>,
    pub eligible_alternatives: Vec<DeploymentProviderBindingV1>,
    pub rejected_providers: Vec<DeploymentProviderRejectionV1>,
    pub operations: Vec<DeploymentOperationV1>,
}

fn route_runtime_class(kind: LogicalRouteKindV1) -> &'static str {
    match kind {
        LogicalRouteKindV1::InterpreterCommand => "route.interpreter-command",
        LogicalRouteKindV1::CompiledBinary => "route.compiled-binary",
        LogicalRouteKindV1::BuildTarget => "route.build-target",
        LogicalRouteKindV1::PackageEntrypoint => "route.package-entrypoint",
        LogicalRouteKindV1::ShellTask => "route.shell-task",
        LogicalRouteKindV1::OEvaluator => "route.o-evaluator",
        LogicalRouteKindV1::Composite => "route.composite",
    }
}

fn policy_runtime_class(policy: &LogicalRoutePolicyV1) -> &'static str {
    match policy {
        LogicalRoutePolicyV1::Explicit { .. } => "policy.explicit",
        LogicalRoutePolicyV1::Default => "policy.default",
        LogicalRoutePolicyV1::Fallback => "policy.fallback",
        LogicalRoutePolicyV1::AnySuccess => "policy.any-success",
        LogicalRoutePolicyV1::RaceSuccess => "policy.race-success",
        LogicalRoutePolicyV1::RaceSettle => "policy.race-settle",
        LogicalRoutePolicyV1::All => "policy.all",
        LogicalRoutePolicyV1::VerifyEquivalent => "policy.verify-equivalent",
        LogicalRoutePolicyV1::BenchmarkAndSelect => "policy.benchmark-and-select",
    }
}

fn hosted_coordinator_supports(policy: &LogicalRoutePolicyV1) -> bool {
    matches!(
        policy,
        LogicalRoutePolicyV1::Explicit { .. }
            | LogicalRoutePolicyV1::Default
            | LogicalRoutePolicyV1::Fallback
            | LogicalRoutePolicyV1::AnySuccess
    )
}

fn requirements_from_operation(
    operation: &LogicalOperationV1,
    project_bundle: &ArtifactId,
) -> DeploymentOperationRequirementsV1 {
    let mut runtime_classes = BTreeSet::new();
    match &operation.kind {
        LogicalOperationKindV1::MaterializeProject => {
            runtime_classes.insert("project.materializer".to_owned());
        }
        LogicalOperationKindV1::BuildRoute { .. } => {
            runtime_classes.insert("project.route-preparer".to_owned());
        }
        LogicalOperationKindV1::RunRoute { .. } => {
            runtime_classes.insert("project.runner".to_owned());
        }
        LogicalOperationKindV1::SelectRoute { policy } => {
            runtime_classes.insert("project.coordinator".to_owned());
            runtime_classes.insert(policy_runtime_class(policy).to_owned());
        }
        LogicalOperationKindV1::CompareRouteResults => {
            runtime_classes.insert("project.compare-route-results".to_owned());
            runtime_classes.insert("project.coordinator".to_owned());
        }
    }

    let mut executables = BTreeSet::new();
    let mut evaluators = BTreeSet::new();
    let mut platform_os = BTreeSet::new();
    let mut environment_overlay_keys = BTreeSet::new();
    let mut environment_keys = BTreeSet::new();
    let is_run_route = matches!(&operation.kind, LogicalOperationKindV1::RunRoute { .. });
    let mut locality = if is_run_route {
        operation.artifact_refs.clone()
    } else {
        Vec::new()
    };
    let mut runtime_contract_complete = !is_run_route;

    if let Some(facts) = operation.route_facts.as_ref().filter(|_| is_run_route) {
        runtime_classes.insert(route_runtime_class(facts.route_kind).to_owned());
        if let Some(executable) = &facts.executable {
            executables.insert(executable.clone());
        }
        if let Some(evaluator) = &facts.evaluator {
            evaluators.insert(evaluator.clone());
        }
        runtime_contract_complete = facts.executable.is_some() || facts.evaluator.is_some();
        environment_overlay_keys.extend(facts.environment_keys.iter().cloned());
        if let Some(entrypoint) = &facts.entrypoint {
            locality.push(LogicalArtifactRefV1 {
                role: LogicalArtifactRoleV1::Input,
                path: entrypoint.clone(),
            });
        }
        for guard in &facts.guards {
            match guard {
                LogicalRouteGuardV1::PlatformOs { value } => {
                    platform_os.insert(value.clone());
                }
                LogicalRouteGuardV1::CommandAvailable { command } => {
                    executables.insert(command.clone());
                }
                LogicalRouteGuardV1::EnvVarSet { name } => {
                    environment_keys.insert(name.clone());
                }
            }
        }
    }
    let mut locality = locality
        .into_iter()
        .map(|artifact| DeploymentProjectPathV1 {
            bundle: project_bundle.clone(),
            artifact,
        })
        .collect::<Vec<_>>();
    locality.sort();
    locality.dedup();

    DeploymentOperationRequirementsV1 {
        project_bundle: project_bundle.clone(),
        architecture: DeploymentArchitectureRequirementV1::Unspecified,
        runtime_classes: runtime_classes.into_iter().collect(),
        executables: executables.into_iter().collect(),
        evaluators: evaluators.into_iter().collect(),
        platform_os: platform_os.into_iter().collect(),
        environment_overlay_keys: environment_overlay_keys.into_iter().collect(),
        environment_keys: environment_keys.into_iter().collect(),
        packages: Vec::new(),
        locality,
        authority: operation.authority_requirements.clone(),
        failure_domains: Vec::new(),
        residual_host_world: operation
            .effects
            .scheduler_resources
            .contains(&LogicalResourceV1::HostWorld),
        runtime_contract_complete,
    }
}

fn compatibility_issues(
    requirements: &DeploymentOperationRequirementsV1,
    provider: &DeploymentProviderSnapshotV1,
) -> Vec<DeploymentCompatibilityIssueV1> {
    let mut issues = BTreeSet::new();
    if !requirements.runtime_contract_complete {
        issues.insert(DeploymentCompatibilityIssueV1::RuntimeContractIncomplete);
    }
    if let DeploymentArchitectureRequirementV1::Exact { architecture } = &requirements.architecture
    {
        if architecture != &provider.architecture {
            issues.insert(DeploymentCompatibilityIssueV1::ArchitectureMismatch {
                required: architecture.clone(),
                available: provider.architecture.clone(),
            });
        }
    }
    for required in &requirements.platform_os {
        if required != &provider.platform_os {
            issues.insert(DeploymentCompatibilityIssueV1::PlatformOsMismatch {
                required: required.clone(),
                available: provider.platform_os.clone(),
            });
        }
    }
    for runtime_class in &requirements.runtime_classes {
        if provider
            .runtime_classes
            .binary_search(runtime_class)
            .is_err()
        {
            issues.insert(DeploymentCompatibilityIssueV1::MissingRuntime {
                runtime_class: runtime_class.clone(),
            });
        }
    }
    for executable in &requirements.executables {
        if provider.executables.binary_search(executable).is_err() {
            issues.insert(DeploymentCompatibilityIssueV1::MissingExecutable {
                executable: executable.clone(),
            });
        }
    }
    for evaluator in &requirements.evaluators {
        if provider.evaluators.binary_search(evaluator).is_err() {
            issues.insert(DeploymentCompatibilityIssueV1::MissingEvaluator {
                evaluator: evaluator.clone(),
            });
        }
    }
    for name in &requirements.environment_keys {
        if provider.environment_keys.binary_search(name).is_err() {
            issues.insert(DeploymentCompatibilityIssueV1::MissingEnvironmentKey {
                name: name.clone(),
            });
        }
    }
    for package in &requirements.packages {
        if provider.packages.binary_search(package).is_err() {
            issues.insert(DeploymentCompatibilityIssueV1::MissingPackage {
                package: package.clone(),
            });
        }
    }
    if provider
        .project_bundles
        .binary_search(&requirements.project_bundle)
        .is_err()
    {
        issues.insert(DeploymentCompatibilityIssueV1::MissingProjectBundle {
            bundle: requirements.project_bundle.clone(),
        });
    }
    for path in &requirements.locality {
        if provider.project_paths.binary_search(path).is_err() {
            issues
                .insert(DeploymentCompatibilityIssueV1::MissingProjectPath { path: path.clone() });
        }
    }
    if !requirements.authority.is_empty() {
        // Snapshot metadata can never stand in for a live authority broker.
        issues.insert(DeploymentCompatibilityIssueV1::AuthorityBrokerRequired);
    }
    for constraint in &requirements.failure_domains {
        match constraint {
            DeploymentFailureDomainConstraintV1::Require { failure_domain }
                if failure_domain != &provider.failure_domain =>
            {
                issues.insert(DeploymentCompatibilityIssueV1::FailureDomainMismatch {
                    failure_domain: failure_domain.clone(),
                });
            }
            DeploymentFailureDomainConstraintV1::Avoid { failure_domain }
                if failure_domain == &provider.failure_domain =>
            {
                issues.insert(DeploymentCompatibilityIssueV1::FailureDomainMismatch {
                    failure_domain: failure_domain.clone(),
                });
            }
            _ => {}
        }
    }
    if requirements.residual_host_world && !provider.admits_host_world {
        issues.insert(DeploymentCompatibilityIssueV1::ResidualHostWorldDenied);
    }
    issues.into_iter().collect()
}

fn validate_issue(issue: &DeploymentCompatibilityIssueV1) -> Result<(), DeploymentPlanError> {
    match issue {
        DeploymentCompatibilityIssueV1::ArchitectureMismatch {
            required,
            available,
        }
        | DeploymentCompatibilityIssueV1::PlatformOsMismatch {
            required,
            available,
        } => {
            validate_text(required, "compatibility requirement")?;
            validate_text(available, "compatibility observation")?;
        }
        DeploymentCompatibilityIssueV1::MissingRuntime { runtime_class } => {
            validate_text(runtime_class, "missing runtime class")?;
        }
        DeploymentCompatibilityIssueV1::MissingExecutable { executable } => {
            validate_text(executable, "missing executable")?;
        }
        DeploymentCompatibilityIssueV1::MissingEvaluator { evaluator } => {
            validate_text(evaluator, "missing evaluator")?;
        }
        DeploymentCompatibilityIssueV1::MissingEnvironmentKey { name } => {
            validate_text(name, "missing environment key")?;
        }
        DeploymentCompatibilityIssueV1::MissingPackage { package } => {
            validate_digest(package, "missing package digest")?;
        }
        DeploymentCompatibilityIssueV1::MissingProjectBundle { bundle } => {
            validate_digest(bundle, "missing project bundle digest")?;
        }
        DeploymentCompatibilityIssueV1::MissingProjectPath { path } => {
            validate_digest(&path.bundle, "missing project-path bundle digest")?;
            validate_text(&path.artifact.path, "missing project path")?;
        }
        DeploymentCompatibilityIssueV1::FailureDomainMismatch { failure_domain } => {
            validate_text(failure_domain, "failure-domain mismatch")?;
        }
        DeploymentCompatibilityIssueV1::MissingPlacementSnapshot
        | DeploymentCompatibilityIssueV1::MissingWorldReference
        | DeploymentCompatibilityIssueV1::MissingTaskIdentity
        | DeploymentCompatibilityIssueV1::RuntimeContractIncomplete
        | DeploymentCompatibilityIssueV1::AuthorityBrokerRequired
        | DeploymentCompatibilityIssueV1::ResidualHostWorldDenied
        | DeploymentCompatibilityIssueV1::NoCompatibleProvider => {}
        DeploymentCompatibilityIssueV1::UnsupportedHostedPolicy { policy } => {
            validate_text(policy, "unsupported hosted policy")?;
        }
    }
    Ok(())
}

impl DeploymentPlanV1 {
    /// Construct the exact current hosted deployment profile. Supported graph
    /// policies keep workspaces/routes ambient and coordinator operations
    /// in-process. Other policies remain explicitly unresolved because the
    /// opt-in ProjectCoordinator does not instantiate them. No World or task
    /// identity is attached after the fact.
    pub fn hosted(logical: &LogicalHGraphV1) -> Result<Self, DeploymentPlanError> {
        logical.validate()?;
        let logical_hgraph = logical.digest()?;
        let supported = hosted_coordinator_supports(&logical.source.policy);
        let operations = logical
            .operations
            .iter()
            .map(|operation| DeploymentOperationV1 {
                logical_operation: operation.id,
                task: None,
                requirements: requirements_from_operation(operation, &logical.source.bundle),
                binding: if !supported {
                    DeploymentOperationBindingV1::Unresolved {
                        issues: vec![DeploymentCompatibilityIssueV1::UnsupportedHostedPolicy {
                            policy: policy_runtime_class(&logical.source.policy).to_owned(),
                        }],
                    }
                } else {
                    match &operation.kind {
                        LogicalOperationKindV1::BuildRoute { .. }
                        | LogicalOperationKindV1::SelectRoute { .. }
                        | LogicalOperationKindV1::CompareRouteResults => {
                            DeploymentOperationBindingV1::HostedCoordinator
                        }
                        LogicalOperationKindV1::MaterializeProject
                        | LogicalOperationKindV1::RunRoute { .. } => {
                            DeploymentOperationBindingV1::AmbientHost
                        }
                    }
                },
            })
            .collect();
        let plan = Self {
            schema_version: DEPLOYMENT_PLAN_SCHEMA_V1,
            logical_hgraph_schema: LOGICAL_HGRAPH_SCHEMA_V1,
            logical_hgraph,
            world: None,
            placement_snapshot: None,
            selected_provider: None,
            eligible_alternatives: Vec::new(),
            rejected_providers: Vec::new(),
            operations,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Derive a deterministic, single-provider proposal from one
    /// exact descriptive snapshot and caller-supplied task identities.
    ///
    /// Selection uses canonical provider order after filtering every logical
    /// requirement. It does not admit the provider, grant authority, dispatch a
    /// task, or prove that the snapshot remains current.
    pub fn from_snapshot_single_provider(
        logical: &LogicalHGraphV1,
        snapshot: &PlacementSnapshotV1,
        tasks: &BTreeMap<LogicalOperationIdV1, TaskIdentity>,
    ) -> Result<Self, DeploymentPlanError> {
        logical.validate()?;
        snapshot.validate()?;
        if tasks.len() != logical.operations.len()
            || logical
                .operations
                .iter()
                .any(|operation| !tasks.contains_key(&operation.id))
        {
            return Err(invalid(
                "snapshot-derived deployment requires exactly one caller-supplied task identity per logical operation",
            ));
        }
        let mut unique_tasks = BTreeSet::new();
        for task in tasks.values() {
            if task.world() != snapshot.world.world() {
                return Err(invalid(
                    "deployment task belongs to a different logical World",
                ));
            }
            if !unique_tasks.insert(task) {
                return Err(invalid("deployment repeats a task identity"));
            }
        }

        let requirements = logical
            .operations
            .iter()
            .map(|operation| {
                (
                    operation.id,
                    requirements_from_operation(operation, &logical.source.bundle),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut eligible = Vec::new();
        let mut rejected = Vec::new();
        for provider in &snapshot.providers {
            let mut issues = Vec::new();
            for operation in &logical.operations {
                issues.extend(
                    compatibility_issues(&requirements[&operation.id], provider)
                        .into_iter()
                        .map(|issue| DeploymentProviderIssueV1 {
                            operation: operation.id,
                            issue,
                        }),
                );
            }
            issues.sort();
            issues.dedup();
            if issues.is_empty() {
                eligible.push(provider.binding.clone());
            } else {
                rejected.push(DeploymentProviderRejectionV1 {
                    provider: provider.binding.clone(),
                    issues,
                });
            }
        }
        let selected_provider = eligible.first().cloned();
        let eligible_alternatives = eligible.into_iter().skip(1).collect::<Vec<_>>();
        let operations = logical
            .operations
            .iter()
            .map(|operation| DeploymentOperationV1 {
                logical_operation: operation.id,
                task: Some(tasks[&operation.id].clone()),
                requirements: requirements[&operation.id].clone(),
                binding: selected_provider.as_ref().map_or_else(
                    || DeploymentOperationBindingV1::Unresolved {
                        issues: vec![DeploymentCompatibilityIssueV1::NoCompatibleProvider],
                    },
                    |provider| DeploymentOperationBindingV1::ProposedProvider {
                        provider: provider.clone(),
                    },
                ),
            })
            .collect();
        let plan = Self {
            schema_version: DEPLOYMENT_PLAN_SCHEMA_V1,
            logical_hgraph_schema: LOGICAL_HGRAPH_SCHEMA_V1,
            logical_hgraph: logical.digest()?,
            world: Some(snapshot.world.clone()),
            placement_snapshot: Some(snapshot.digest()?),
            selected_provider,
            eligible_alternatives,
            rejected_providers: rejected,
            operations,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Validate record structure and internal consistency. This does not prove
    /// that source-derived fields match a trusted logical graph or snapshot;
    /// use `validate_trusted_hosted` or `validate_trusted_snapshot` for that.
    pub fn validate(&self) -> Result<(), DeploymentPlanError> {
        if self.schema_version != DEPLOYMENT_PLAN_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported deployment plan schema {}",
                self.schema_version
            )));
        }
        if self.logical_hgraph_schema != LOGICAL_HGRAPH_SCHEMA_V1 {
            return Err(invalid(format!(
                "unsupported logical HGraph schema binding {}",
                self.logical_hgraph_schema
            )));
        }
        validate_digest(&self.logical_hgraph, "logical HGraph digest")?;
        if self.operations.is_empty() || self.operations.len() > MAX_DEPLOYMENT_OPERATIONS {
            return Err(invalid(format!(
                "deployment operation count {} is outside 1..={MAX_DEPLOYMENT_OPERATIONS}",
                self.operations.len()
            )));
        }
        if self.world.is_some() != self.placement_snapshot.is_some() {
            return Err(invalid(
                "World reference and placement snapshot digest must be present together",
            ));
        }
        if let Some(snapshot) = &self.placement_snapshot {
            validate_digest(snapshot, "placement snapshot digest")?;
        }

        if self
            .eligible_alternatives
            .windows(2)
            .any(|pair| pair[0].service >= pair[1].service)
        {
            return Err(invalid(
                "eligible provider alternatives must be strictly ordered",
            ));
        }
        if self
            .rejected_providers
            .windows(2)
            .any(|pair| pair[0].provider.service >= pair[1].provider.service)
        {
            return Err(invalid("rejected providers must be strictly ordered"));
        }

        let mut provider_set = BTreeSet::new();
        if let Some(selected) = &self.selected_provider {
            selected.validate()?;
            if !provider_set.insert(selected.service.clone()) {
                return Err(invalid("selected provider is duplicated"));
            }
        }
        for provider in &self.eligible_alternatives {
            provider.validate()?;
            if !provider_set.insert(provider.service.clone()) {
                return Err(invalid("eligible provider is duplicated"));
            }
        }
        if let (Some(selected), Some(first_alternative)) = (
            self.selected_provider.as_ref(),
            self.eligible_alternatives.first(),
        ) {
            if selected.service >= first_alternative.service {
                return Err(invalid(
                    "selected provider is not the first canonical eligible provider",
                ));
            }
        }
        for rejection in &self.rejected_providers {
            rejection.provider.validate()?;
            if rejection.issues.is_empty()
                || rejection.issues.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(invalid(
                    "provider rejection issues must be nonempty and strictly ordered",
                ));
            }
            if !provider_set.insert(rejection.provider.service.clone()) {
                return Err(invalid(
                    "provider appears in more than one placement decision set",
                ));
            }
            for issue in &rejection.issues {
                if issue.operation.0
                    >= u64::try_from(self.operations.len())
                        .map_err(|_| invalid("deployment operation count does not fit u64"))?
                {
                    return Err(invalid(
                        "provider rejection names an unknown logical operation",
                    ));
                }
                validate_issue(&issue.issue)?;
            }
        }
        validate_provider_generation_coherence(
            self.selected_provider
                .iter()
                .chain(&self.eligible_alternatives)
                .chain(self.rejected_providers.iter().map(|entry| &entry.provider)),
        )?;
        if let Some(world) = &self.world {
            if self
                .selected_provider
                .iter()
                .chain(&self.eligible_alternatives)
                .chain(self.rejected_providers.iter().map(|entry| &entry.provider))
                .any(|provider| !provider.world_matches(world))
            {
                return Err(invalid(
                    "placement decision contains a provider from another World",
                ));
            }
        }

        let mut tasks = BTreeSet::new();
        let project_bundle = &self.operations[0].requirements.project_bundle;
        let operation_count = u64::try_from(self.operations.len())
            .map_err(|_| invalid("deployment operation count does not fit u64"))?;
        for (index, operation) in self.operations.iter().enumerate() {
            let expected = u64::try_from(index)
                .map_err(|_| invalid("deployment operation index does not fit u64"))?;
            if operation.logical_operation.0 != expected {
                return Err(invalid(format!(
                    "deployment operation index {index} has noncanonical logical id {}",
                    operation.logical_operation.0
                )));
            }
            operation.requirements.validate()?;
            if &operation.requirements.project_bundle != project_bundle {
                return Err(invalid(
                    "deployment operations disagree on the exact project bundle",
                ));
            }
            if let Some(task) = &operation.task {
                if !tasks.insert(task) {
                    return Err(invalid("deployment repeats a task identity"));
                }
                if self
                    .world
                    .as_ref()
                    .is_some_and(|world| task.world() != world.world())
                {
                    return Err(invalid(
                        "deployment task belongs to a different logical World",
                    ));
                }
            }
            match &operation.binding {
                DeploymentOperationBindingV1::HostedCoordinator
                | DeploymentOperationBindingV1::AmbientHost => {
                    if self.world.is_some() || operation.task.is_some() {
                        return Err(invalid(
                            "ambient/hosted deployment bindings cannot carry World task identity",
                        ));
                    }
                }
                DeploymentOperationBindingV1::Unresolved { issues } => {
                    if issues.is_empty() || issues.windows(2).any(|pair| pair[0] >= pair[1]) {
                        return Err(invalid(
                            "unresolved binding issues must be nonempty and strictly ordered",
                        ));
                    }
                    for issue in issues {
                        validate_issue(issue)?;
                    }
                }
                DeploymentOperationBindingV1::ProposedProvider { provider } => {
                    provider.validate()?;
                    let world = self
                        .world
                        .as_ref()
                        .ok_or_else(|| invalid("provider proposal has no exact World reference"))?;
                    let task = operation.task.as_ref().ok_or_else(|| {
                        invalid("provider proposal has no caller-supplied task identity")
                    })?;
                    if !provider.world_matches(world) || task.world() != world.world() {
                        return Err(invalid(
                            "proposed provider/task belongs to a different logical World",
                        ));
                    }
                    if self.selected_provider.as_ref() != Some(provider) {
                        return Err(invalid(
                            "operation provider disagrees with the single-provider decision",
                        ));
                    }
                }
            }
        }
        if operation_count == 0 {
            return Err(invalid("deployment plan has no operations"));
        }

        match (&self.world, &self.selected_provider) {
            (None, None) => {
                if !self.eligible_alternatives.is_empty() || !self.rejected_providers.is_empty() {
                    return Err(invalid(
                        "hosted deployment cannot carry snapshot provider decisions",
                    ));
                }
                if self.operations.iter().any(|operation| {
                    operation.task.is_some()
                        || !matches!(
                            operation.binding,
                            DeploymentOperationBindingV1::HostedCoordinator
                                | DeploymentOperationBindingV1::AmbientHost
                                | DeploymentOperationBindingV1::Unresolved { .. }
                        )
                }) {
                    return Err(invalid(
                        "unbound hosted deployment must use only explicit hosted, ambient, or unresolved bindings",
                    ));
                }
            }
            (Some(world), Some(selected)) => {
                if !selected.world_matches(world)
                    || self.operations.iter().any(|operation| {
                        !matches!(
                            operation.binding,
                            DeploymentOperationBindingV1::ProposedProvider { .. }
                        )
                    })
                {
                    return Err(invalid(
                        "selected proposed provider must bind every operation in the exact World",
                    ));
                }
            }
            (Some(world), None) => {
                if !self.eligible_alternatives.is_empty() {
                    return Err(invalid(
                        "deployment cannot leave compatible providers unselected",
                    ));
                }
                if self
                    .eligible_alternatives
                    .iter()
                    .chain(self.rejected_providers.iter().map(|entry| &entry.provider))
                    .any(|provider| !provider.world_matches(world))
                {
                    return Err(invalid(
                        "placement decision contains a provider from another World",
                    ));
                }
                if self.operations.iter().any(|operation| {
                    !matches!(
                        operation.binding,
                        DeploymentOperationBindingV1::Unresolved { .. }
                    ) || operation.task.is_none()
                }) {
                    return Err(invalid(
                        "a snapshot-bound deployment without a selected provider must remain unresolved",
                    ));
                }
            }
            (None, Some(_)) => {
                return Err(invalid(
                    "selected provider has no exact World/snapshot binding",
                ))
            }
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, DeploymentPlanError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_DEPLOYMENT_RECORD_BYTES {
            return Err(DeploymentPlanError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_DEPLOYMENT_RECORD_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decode a structurally valid record for inspection. Canonical source
    /// authenticity still requires one of the trusted validation methods.
    pub fn decode(bytes: &[u8]) -> Result<Self, DeploymentPlanError> {
        if bytes.len() > MAX_DEPLOYMENT_RECORD_BYTES {
            return Err(DeploymentPlanError::RecordTooLarge {
                actual: bytes.len(),
                maximum: MAX_DEPLOYMENT_RECORD_BYTES,
            });
        }
        let plan: Self = serde_json::from_slice(bytes)?;
        plan.validate()?;
        Ok(plan)
    }

    /// Decode canonical record bytes; this is not a trusted source comparison.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, DeploymentPlanError> {
        let plan = Self::decode(bytes)?;
        if plan.canonical_bytes()? != bytes {
            return Err(DeploymentPlanError::NonCanonicalEncoding);
        }
        Ok(plan)
    }

    pub fn digest(&self) -> Result<ArtifactId, DeploymentPlanError> {
        digest_canonical(DEPLOYMENT_PLAN_DIGEST_DOMAIN, &self.canonical_bytes()?)
    }

    pub fn validate_trusted_hosted(
        &self,
        logical: &LogicalHGraphV1,
    ) -> Result<(), DeploymentPlanError> {
        if self != &Self::hosted(logical)? {
            return Err(invalid(
                "deployment plan does not match the trusted hosted logical graph",
            ));
        }
        Ok(())
    }

    pub fn validate_trusted_snapshot(
        &self,
        logical: &LogicalHGraphV1,
        snapshot: &PlacementSnapshotV1,
        tasks: &BTreeMap<LogicalOperationIdV1, TaskIdentity>,
    ) -> Result<(), DeploymentPlanError> {
        if self != &Self::from_snapshot_single_provider(logical, snapshot, tasks)? {
            return Err(invalid(
                "deployment plan does not match the trusted logical graph, placement snapshot, and tasks",
            ));
        }
        Ok(())
    }

    /// Reject an old World epoch before a snapshot-bound plan is consumed.
    pub fn require_current_world(
        &self,
        current: &WorldIdentity,
    ) -> Result<(), DeploymentPlanError> {
        let reference = self
            .world
            .as_ref()
            .ok_or_else(|| invalid("hosted deployment is not bound to a World epoch"))?;
        current.require_current(reference)?;
        Ok(())
    }

    pub fn to_text(&self) -> String {
        let mut output = String::from("; DeploymentPlan oworld.deployment/v1\n");
        writeln!(
            output,
            "logical schema={} sha256={}",
            self.logical_hgraph_schema,
            self.logical_hgraph.as_sha256()
        )
        .unwrap();
        match (&self.world, &self.placement_snapshot) {
            (Some(world), Some(snapshot)) => {
                writeln!(
                    output,
                    "placement world={} snapshot-sha256={}",
                    world,
                    snapshot.as_sha256()
                )
                .unwrap();
            }
            _ => output.push_str("placement hosted-unbound\n"),
        }
        for operation in &self.operations {
            let task = operation
                .task
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "none".to_owned());
            let binding = match &operation.binding {
                DeploymentOperationBindingV1::HostedCoordinator => "hosted-coordinator".to_owned(),
                DeploymentOperationBindingV1::AmbientHost => "ambient-host".to_owned(),
                DeploymentOperationBindingV1::Unresolved { .. } => "unresolved".to_owned(),
                DeploymentOperationBindingV1::ProposedProvider { provider } => {
                    format!("proposed-provider:{}", provider.service)
                }
            };
            writeln!(
                output,
                "deploy L{} task={} binding={} hostworld={} runtime=[{}]",
                operation.logical_operation.0,
                task,
                binding,
                if operation.requirements.residual_host_world {
                    "residual"
                } else {
                    "no"
                },
                operation.requirements.runtime_classes.join(",")
            )
            .unwrap();
        }
        output.push_str(
            "deployment-note placement snapshots and provider metadata are descriptive; no authority or runtime instantiation is implied\n",
        );
        output
    }
}
