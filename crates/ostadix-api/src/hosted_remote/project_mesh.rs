//! Scheduler policy and execution records for project actors placed on the
//! Ostadix peer mesh.
//!
//! A mesh actor is one source-closed project route branch: its immutable
//! [`ProjectBundle`], selected route, and transitive
//! prerequisites move together so build products remain in one workspace.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::executor::CancellationToken;
use crate::hosted_remote::mesh::{
    mesh_bundle_artifact, mesh_client_failure_disposition, mesh_client_rejection_stage,
    mesh_project_ir_projection, mesh_route_contract_sha256, MeshActorFenceV1, MeshActorIdV1,
    MeshActorPhaseV1, MeshActorRefV1, MeshActorResultV1, MeshActorSpecV1, MeshArtifactUploadV1,
    MeshCapacityV1, MeshClientFailureDisposition, MeshExecutionLimitsV1, MeshNodeClient,
    MeshNodeConnection, MeshNodeProfileV1, MeshRejectionStageV1, MeshRouteRequirementsV1,
};
use crate::hosted_remote::{
    discover_lan_nodes, fetch_lan_bootstrap, lan_peers_config_dir, list_stored_lan_peers,
    store_lan_peer, ClientTlsIdentity, StoredLanPeerPathsV1, StoredLanPeerV1,
};

use crate::project::executor::ConfiguredProjectExecution;
use crate::project::model::{
    OExecutionResult, ProjectBundle, RouteFailureContinuation, RoutePolicy, RouteSpec,
};
use crate::project::runtime::{
    benchmark_validate_and_select, is_cancellation_error, potential_route_execution_count,
    resolve_selection, run_all_alternatives_parallel_measured, verify_results_equivalent,
    RouteExecutionError, RouteSelectionExecution, RunOptions,
};

pub const MESH_EXECUTION_TRACE_SCHEMA_V1: &str = "ostadix.project-mesh-trace/v1";
pub const MESH_READ_ONLY_DISCOVERY_SCHEMA_V1: &str = "ostadix.mesh-read-only-discovery/v1";

/// Whether a caller merely prefers the mesh or requires a remote provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshRequirement {
    Prefer,
    Required,
}

/// The point at which a local provider is allowed to replace a remote one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshLocalFallback {
    /// Local execution is allowed only while every remote attempt is proven
    /// not to have begun execution.
    PreSend,
    /// Local execution is also allowed after a confirmed terminal failure,
    /// but only for a route island whose every command explicitly declares
    /// idempotent continuation. Ambiguous delivery always fails closed.
    Idempotent,
    Never,
}

/// Invocation policy for one scheduler-driven project-mesh execution.
#[derive(Debug, Clone)]
pub struct MeshExecutionConfig {
    pub requirement: MeshRequirement,
    /// Additional actor generations after the first dispatch.
    pub max_retries: u32,
    pub local_fallback: MeshLocalFallback,
    /// Whether live UDP LAN advertisements may augment the paired registry.
    /// Turning this off makes an explicit registry a closed discovery set.
    pub discover_lan: bool,
    pub discovery_timeout: Duration,
    /// Override the automatic Ostadix paired-peer registry root.
    pub peer_root: Option<PathBuf>,
    pub trace_out: Option<PathBuf>,
    pub explain: bool,
}

impl Default for MeshExecutionConfig {
    fn default() -> Self {
        Self {
            requirement: MeshRequirement::Prefer,
            max_retries: 2,
            local_fallback: MeshLocalFallback::PreSend,
            discover_lan: true,
            discovery_timeout: Duration::from_millis(750),
            peer_root: None,
            trace_out: None,
            explain: false,
        }
    }
}

impl MeshExecutionConfig {
    fn validate(&self) -> Result<()> {
        if self.discovery_timeout.is_zero() {
            bail!("mesh discovery timeout must be positive");
        }
        if self.discovery_timeout > Duration::from_secs(60) {
            bail!("mesh discovery timeout may not exceed 60 seconds");
        }
        if self.max_retries > 64 {
            bail!("mesh retries may not exceed 64");
        }
        Ok(())
    }
}

/// Read-only bounds for a live placement preview.
///
/// Unlike [`MeshExecutionConfig`], this type has no retry, fallback, actor, or
/// trace-output controls because this path cannot execute. LAN advertisements
/// are only endpoint hints for identities already present in the pinned peer
/// registry.
#[derive(Debug, Clone)]
pub struct MeshReadOnlyDiscoveryConfig {
    pub discover_lan: bool,
    pub discovery_timeout: Duration,
    pub peer_root: Option<PathBuf>,
}

impl Default for MeshReadOnlyDiscoveryConfig {
    fn default() -> Self {
        Self {
            discover_lan: true,
            discovery_timeout: Duration::from_millis(750),
            peer_root: None,
        }
    }
}

impl MeshReadOnlyDiscoveryConfig {
    fn validate(&self) -> Result<()> {
        if self.discovery_timeout.is_zero() {
            bail!("mesh discovery timeout must be positive");
        }
        if self.discovery_timeout > Duration::from_secs(60) {
            bail!("mesh discovery timeout may not exceed 60 seconds");
        }
        Ok(())
    }
}

impl From<&MeshExecutionConfig> for MeshReadOnlyDiscoveryConfig {
    fn from(config: &MeshExecutionConfig) -> Self {
        Self {
            discover_lan: config.discover_lan,
            discovery_timeout: config.discovery_timeout,
            peer_root: config.peer_root.clone(),
        }
    }
}

/// Stable reason that an authenticated live peer is not currently eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshLivePeerRejectionV1 {
    NoAvailableSlots,
}

/// Stable stage at which a pinned peer could not be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshLivePeerErrorV1 {
    Unreachable,
    ProfileQueryFailed,
    ProfileIdentityMismatch,
    CapacityQueryFailed,
    CapacityInvalid,
}

/// One authenticated peer observation made without any execution-side RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshLivePeerObservationV1 {
    pub node_id: String,
    /// Deterministically ordered endpoints. Advertised endpoints appear here
    /// only when they match this already-pinned node id and TLS server name.
    pub endpoint_hints: Vec<String>,
    pub selected_endpoint: Option<String>,
    pub profile: Option<MeshNodeProfileV1>,
    pub capacity: Option<MeshCapacityV1>,
    pub observed_latency_micros: Option<u64>,
    pub eligible: bool,
    pub rejection: Option<MeshLivePeerRejectionV1>,
    pub error: Option<MeshLivePeerErrorV1>,
    /// Diagnostic transport text. Policy should branch on `rejection` or
    /// `error`, whose values are stable across platforms.
    pub detail: Option<String>,
}

/// Bounded live discovery plus authenticated profile/capacity reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshReadOnlyDiscoveryV1 {
    pub schema: String,
    pub lan_discovery_attempted: bool,
    pub lan_discovery_error: Option<String>,
    pub peers: Vec<MeshLivePeerObservationV1>,
}

impl MeshReadOnlyDiscoveryV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != MESH_READ_ONLY_DISCOVERY_SCHEMA_V1 {
            bail!("mesh read-only discovery has an unsupported schema");
        }
        if !self.lan_discovery_attempted && self.lan_discovery_error.is_some() {
            bail!("disabled LAN discovery cannot retain a discovery error");
        }
        if self
            .lan_discovery_error
            .as_ref()
            .is_some_and(|detail| detail.is_empty())
        {
            bail!("mesh read-only discovery retained an empty discovery error");
        }
        let mut previous = None;
        for peer in &self.peers {
            if peer.node_id.is_empty() {
                bail!("mesh read-only discovery contains an empty node id");
            }
            if previous.is_some_and(|value: &String| value >= &peer.node_id) {
                bail!("mesh read-only discovery peers are not strictly node-id ordered");
            }
            previous = Some(&peer.node_id);
            if peer.endpoint_hints.is_empty()
                || !peer.endpoint_hints.windows(2).all(|pair| pair[0] < pair[1])
            {
                bail!("mesh read-only discovery endpoint hints are not strictly ordered");
            }
            match (&peer.profile, &peer.capacity) {
                (Some(profile), Some(capacity)) => {
                    profile.validate()?;
                    capacity.validate_against(profile)?;
                    if profile.node_id != peer.node_id
                        || peer.selected_endpoint.is_none()
                        || peer.observed_latency_micros.is_none()
                        || peer.error.is_some()
                        || peer.detail.is_some()
                        || !peer
                            .selected_endpoint
                            .as_ref()
                            .is_some_and(|selected| peer.endpoint_hints.contains(selected))
                    {
                        bail!("mesh read-only discovery contains an inconsistent live peer");
                    }
                    let has_capacity = capacity.available_slots > 0;
                    if peer.eligible != has_capacity
                        || peer.rejection
                            != (!has_capacity).then_some(MeshLivePeerRejectionV1::NoAvailableSlots)
                    {
                        bail!("mesh read-only discovery eligibility disagrees with capacity");
                    }
                }
                (None, None) => {
                    if peer.eligible
                        || peer.rejection.is_some()
                        || peer.error.is_none()
                        || peer.selected_endpoint.is_some()
                        || peer.observed_latency_micros.is_some()
                        || peer.detail.as_ref().is_none_or(|detail| detail.is_empty())
                    {
                        bail!("mesh read-only discovery contains an inconsistent failed peer");
                    }
                }
                _ => bail!("mesh read-only discovery retained only half of a live profile"),
            }
        }
        Ok(())
    }
}

/// A current, eligible execution target observed by the mesh resolver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshTargetCandidateV1 {
    pub node_id: String,
    pub is_local: bool,
    pub available_slots: u32,
    pub observed_latency_micros: u64,
}

/// Capacity-first target ordering used only after the HGraph makes an actor
/// ready. The tuple is public so traces and tests can reproduce every choice.
pub fn mesh_target_rank(candidate: &MeshTargetCandidateV1) -> (Reverse<u32>, u64, bool, String) {
    // Capacity is the primary execution concern. Latency follows, remote peers
    // precede a future local candidate at equal measurements, and node id is
    // the stable final tie-break used by traces and tests.
    (
        Reverse(candidate.available_slots),
        candidate.observed_latency_micros,
        candidate.is_local,
        candidate.node_id.clone(),
    )
}

/// A durable, user-readable account of discovery, placement, retries, actor
/// migration, and fallback for one o-link invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshExecutionTraceV1 {
    pub schema: String,
    pub execution_id: String,
    pub bundle_sha256: String,
    pub target: String,
    pub policy: String,
    pub candidates: Vec<MeshTraceCandidateV1>,
    pub events: Vec<MeshTraceEventV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshTraceCandidateV1 {
    pub node_id: String,
    pub address: Option<String>,
    pub available_slots: u32,
    pub observed_latency_micros: u64,
    pub eligible: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MeshTraceEventV1 {
    Dispatched {
        route_id: String,
        actor_id: String,
        generation: u32,
        node_id: String,
    },
    Settled {
        route_id: String,
        actor_id: String,
        generation: u32,
        node_id: String,
        succeeded: bool,
    },
    /// One bounded remote generation failed either before or after the first
    /// admission RPC. `submitted=false` proves no `Dispatched` event exists
    /// for this generation while still retaining the retry's causal reason.
    AttemptFailed {
        route_id: String,
        actor_id: String,
        generation: u32,
        node_id: String,
        submitted: bool,
        delivery: String,
        replay_contract: String,
        reason: String,
    },
    Migrated {
        route_id: String,
        actor_id: String,
        from_generation: u32,
        to_generation: u32,
        from_node_id: String,
        to_node_id: String,
        replay_contract: String,
    },
    RetryDenied {
        route_id: String,
        actor_id: String,
        generation: u32,
        reason: String,
    },
    LocalFallback {
        route_id: String,
        actor_id: String,
        after_generation: u32,
        replay_contract: String,
        reason: String,
    },
}

impl MeshExecutionTraceV1 {
    fn new(
        execution_id: String,
        bundle_sha256: String,
        target: String,
        policy: &RoutePolicy,
    ) -> Self {
        Self {
            schema: MESH_EXECUTION_TRACE_SCHEMA_V1.to_string(),
            execution_id,
            bundle_sha256,
            target,
            policy: policy.token(),
            candidates: Vec::new(),
            events: Vec::new(),
        }
    }

    fn write(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self).context("failed to encode mesh trace")?;
        std::fs::write(path, bytes)
            .with_context(|| format!("failed to write mesh trace {}", path.display()))
    }

    /// Validate the versioned observation before it is persisted or embedded
    /// in a higher-level run record.
    pub fn validate(&self) -> Result<()> {
        if self.schema != MESH_EXECUTION_TRACE_SCHEMA_V1 {
            bail!("mesh execution trace has an unsupported schema");
        }
        validate_lower_hex("mesh execution id", &self.execution_id, 32)?;
        validate_lower_hex("mesh bundle sha256", &self.bundle_sha256, 64)?;
        if self.target.is_empty() {
            bail!("mesh execution trace has an empty target");
        }
        let parsed_policy = RoutePolicy::parse_checked(&self.policy)
            .map_err(anyhow::Error::msg)
            .context("mesh execution trace has an invalid route policy")?;
        if parsed_policy.token() != self.policy {
            bail!("mesh execution trace route policy is not canonical");
        }
        let mut previous = None;
        let mut eligible_nodes = BTreeSet::new();
        for candidate in &self.candidates {
            if candidate.node_id.is_empty() {
                bail!("mesh execution trace contains an empty candidate node id");
            }
            if previous.is_some_and(|value: &String| value >= &candidate.node_id) {
                bail!("mesh execution trace candidates are not strictly node-id ordered");
            }
            previous = Some(&candidate.node_id);
            if candidate.detail.is_empty() {
                bail!("mesh execution trace candidate detail is empty");
            }
            if candidate.eligible {
                eligible_nodes.insert(candidate.node_id.clone());
            }
        }
        let mut actor_routes = BTreeMap::<String, String>::new();
        let mut dispatched = BTreeMap::<(String, u32), (String, String, bool)>::new();
        let mut failed_attempts = BTreeSet::<(String, u32)>::new();
        let mut last_dispatch = BTreeMap::<String, (u32, String)>::new();
        let mut pending_migrations = BTreeMap::<String, (u32, u32, String, String)>::new();
        let mut terminal_actors = BTreeSet::<String>::new();
        for event in &self.events {
            let (route_id, actor_id, node_id) = match event {
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
                }
                | MeshTraceEventV1::AttemptFailed {
                    route_id,
                    actor_id,
                    node_id,
                    ..
                } => (route_id, actor_id, Some(node_id.as_str())),
                MeshTraceEventV1::Migrated {
                    route_id,
                    actor_id,
                    from_node_id,
                    to_node_id,
                    replay_contract,
                    ..
                } => {
                    if from_node_id.is_empty()
                        || to_node_id.is_empty()
                        || replay_contract.is_empty()
                    {
                        bail!("mesh execution trace migration event is incomplete");
                    }
                    (route_id, actor_id, None)
                }
                MeshTraceEventV1::RetryDenied {
                    route_id,
                    actor_id,
                    reason,
                    ..
                } => {
                    if reason.is_empty() {
                        bail!("mesh execution trace decision event has an empty reason");
                    }
                    (route_id, actor_id, None)
                }
                MeshTraceEventV1::LocalFallback {
                    route_id,
                    actor_id,
                    reason,
                    replay_contract,
                    ..
                } => {
                    if reason.is_empty() || replay_contract.is_empty() {
                        bail!("mesh execution trace fallback event is incomplete");
                    }
                    (route_id, actor_id, None)
                }
            };
            if route_id.is_empty() || node_id.is_some_and(str::is_empty) {
                bail!("mesh execution trace event has an empty route or node id");
            }
            validate_lower_hex("mesh actor id", actor_id, 64)?;
            if actor_routes
                .insert(actor_id.clone(), route_id.clone())
                .is_some_and(|known| known != *route_id)
            {
                bail!("mesh actor id is reused across different routes");
            }
            if terminal_actors.contains(actor_id) {
                bail!("mesh execution trace records an event after an actor became terminal");
            }

            match event {
                MeshTraceEventV1::Dispatched {
                    generation,
                    node_id,
                    ..
                } => {
                    if *generation == 0 || !eligible_nodes.contains(node_id) {
                        bail!("mesh dispatch names generation zero or an ineligible candidate");
                    }
                    let previous_dispatch = last_dispatch.get(actor_id);
                    if previous_dispatch.is_some_and(|(previous, _)| *generation <= *previous)
                        || dispatched.contains_key(&(actor_id.clone(), *generation))
                    {
                        bail!("mesh actor dispatch generations are not strictly increasing");
                    }
                    if previous_dispatch.is_none() {
                        if pending_migrations.contains_key(actor_id) {
                            bail!("first mesh dispatch unexpectedly has migration evidence");
                        }
                    } else {
                        let Some((last_generation, last_node)) = last_dispatch.get(actor_id) else {
                            bail!("mesh migration has no preceding dispatch");
                        };
                        if last_node == node_id {
                            if pending_migrations.remove(actor_id).is_some() {
                                bail!("same-node mesh retry must not claim actor migration");
                            }
                        } else {
                            let Some((from_generation, to_generation, from_node, to_node)) =
                                pending_migrations.remove(actor_id)
                            else {
                                bail!(
                                    "cross-node mesh retry is missing its preceding migration event"
                                );
                            };
                            if from_generation != *last_generation
                                || to_generation != *generation
                                || from_node != *last_node
                                || to_node != *node_id
                            {
                                bail!("mesh migration does not connect consecutive dispatches");
                            }
                        }
                    }
                    dispatched.insert(
                        (actor_id.clone(), *generation),
                        (route_id.clone(), node_id.clone(), false),
                    );
                    last_dispatch.insert(actor_id.clone(), (*generation, node_id.clone()));
                }
                MeshTraceEventV1::Settled {
                    generation,
                    node_id,
                    succeeded,
                    ..
                } => {
                    let Some((known_route, known_node, settled)) =
                        dispatched.get_mut(&(actor_id.clone(), *generation))
                    else {
                        bail!("mesh settlement has no matching dispatch");
                    };
                    if *settled || known_route != route_id || known_node != node_id {
                        bail!("mesh settlement disagrees with or repeats its dispatch");
                    }
                    *settled = true;
                    if *succeeded {
                        terminal_actors.insert(actor_id.clone());
                    }
                }
                MeshTraceEventV1::AttemptFailed {
                    generation,
                    node_id,
                    submitted,
                    delivery,
                    replay_contract,
                    reason,
                    ..
                } => {
                    if *generation == 0
                        || !eligible_nodes.contains(node_id)
                        || !matches!(
                            delivery.as_str(),
                            "proven_not_started" | "ambiguous" | "executed"
                        )
                        || !matches!(replay_contract.as_str(), "unproven" | "declared_idempotent")
                        || reason.is_empty()
                        || !failed_attempts.insert((actor_id.clone(), *generation))
                    {
                        bail!("mesh failed-attempt observation is invalid");
                    }
                    let matching_dispatch = dispatched
                        .get(&(actor_id.clone(), *generation))
                        .is_some_and(|(known_route, known_node, settled)| {
                            known_route == route_id && known_node == node_id && !settled
                        });
                    if *submitted != matching_dispatch {
                        bail!("mesh failed-attempt submission state disagrees with its dispatch");
                    }
                }
                MeshTraceEventV1::Migrated {
                    from_generation,
                    to_generation,
                    from_node_id,
                    to_node_id,
                    ..
                } => {
                    let Some((last_generation, last_node)) = last_dispatch.get(actor_id) else {
                        bail!("mesh migration has no preceding dispatch");
                    };
                    if *from_generation != *last_generation
                        || *to_generation <= *from_generation
                        || from_node_id != last_node
                        || from_node_id == to_node_id
                        || !eligible_nodes.contains(to_node_id)
                        || pending_migrations
                            .insert(
                                actor_id.clone(),
                                (
                                    *from_generation,
                                    *to_generation,
                                    from_node_id.clone(),
                                    to_node_id.clone(),
                                ),
                            )
                            .is_some()
                    {
                        bail!("mesh migration lifecycle is inconsistent");
                    }
                }
                MeshTraceEventV1::RetryDenied { generation, .. } => {
                    if last_dispatch.get(actor_id).map(|(value, _)| value) != Some(generation)
                        || pending_migrations.contains_key(actor_id)
                    {
                        bail!("mesh retry denial does not follow its last dispatch");
                    }
                    terminal_actors.insert(actor_id.clone());
                }
                MeshTraceEventV1::LocalFallback {
                    after_generation, ..
                } => {
                    let observed = last_dispatch
                        .get(actor_id)
                        .map_or(0, |(generation, _)| *generation);
                    if observed != *after_generation || pending_migrations.contains_key(actor_id) {
                        bail!("mesh local fallback disagrees with the last remote generation");
                    }
                    terminal_actors.insert(actor_id.clone());
                }
            }
        }
        if !pending_migrations.is_empty() {
            bail!("mesh execution trace ends with an incomplete migration");
        }
        Ok(())
    }
}

fn validate_lower_hex(field: &str, value: &str, expected_len: usize) -> Result<()> {
    if value.len() != expected_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("{field} must be exactly {expected_len} lowercase hexadecimal characters");
    }
    Ok(())
}

/// Successful mesh results together with the exact discovery/placement trace.
#[derive(Debug)]
pub struct MeshExecutionOutcome {
    pub execution: ConfiguredProjectExecution,
    pub trace: MeshExecutionTraceV1,
}

/// An execution-phase mesh failure retaining every observation recorded before
/// failure. Errors before trace construction remain ordinary preflight errors.
#[derive(Debug)]
pub struct MeshExecutionError {
    message: String,
    public_message: String,
    source: anyhow::Error,
    class: MeshExecutionFailureClass,
    pub trace: MeshExecutionTraceV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshExecutionFailureClass {
    Semantic,
    Infrastructure,
}

impl MeshExecutionError {
    fn new(source: anyhow::Error, trace: MeshExecutionTraceV1) -> Self {
        let class = classify_mesh_failure(&trace);
        let public_message = if source
            .downcast_ref::<RequiredMeshPlacementUnavailable>()
            .is_some()
        {
            REQUIRED_MESH_PLACEMENT_UNAVAILABLE.to_string()
        } else {
            "project mesh execution failed after observed placement; detailed transport diagnostic omitted from persistent observation"
                .to_string()
        };
        Self {
            message: format!("{source:#}"),
            public_message,
            source,
            class,
            trace,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Credential-safe causal diagnostic suitable for CLI output and
    /// persistent unsigned observations.
    pub fn public_message(&self) -> &str {
        &self.public_message
    }

    pub fn source_error(&self) -> &anyhow::Error {
        &self.source
    }

    pub const fn class(&self) -> MeshExecutionFailureClass {
        self.class
    }
}

fn classify_mesh_failure(trace: &MeshExecutionTraceV1) -> MeshExecutionFailureClass {
    let dispatched = trace
        .events
        .iter()
        .filter_map(|event| match event {
            MeshTraceEventV1::Dispatched {
                actor_id,
                generation,
                ..
            } => Some((actor_id.as_str(), *generation)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let settled = trace
        .events
        .iter()
        .filter_map(|event| match event {
            MeshTraceEventV1::Settled {
                actor_id,
                generation,
                ..
            } => Some((actor_id.as_str(), *generation)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if !settled.is_empty() && dispatched == settled {
        MeshExecutionFailureClass::Semantic
    } else {
        MeshExecutionFailureClass::Infrastructure
    }
}

impl fmt::Display for MeshExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for MeshExecutionError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

const REQUIRED_MESH_PLACEMENT_UNAVAILABLE: &str = "mesh placement is required, but discovery found no authenticated peer eligible for this bundle, execution policy, and actor slot";

#[derive(Debug)]
struct RequiredMeshPlacementUnavailable;

impl fmt::Display for RequiredMeshPlacementUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REQUIRED_MESH_PLACEMENT_UNAVAILABLE)
    }
}

impl StdError for RequiredMeshPlacementUnavailable {}

/// Strongest replay statement available for an entire route/prerequisite
/// island. Every member must be covered because they share one workspace and
/// migrate as a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchReplayContract {
    Unproven,
    DeclaredIdempotent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptDeliveryState {
    /// The authenticated request was not delivered, or the destination later
    /// proved the exact actor generation absent.
    ProvenNotStarted,
    /// The destination may have started the actor and no terminal record could
    /// yet be reconciled.
    Ambiguous,
    /// A terminal execution result or execution-stage failure exists.
    Executed,
}

fn merge_delivery_state(
    current: AttemptDeliveryState,
    observed: AttemptDeliveryState,
) -> AttemptDeliveryState {
    match (current, observed) {
        (AttemptDeliveryState::Ambiguous, _) | (_, AttemptDeliveryState::Ambiguous) => {
            AttemptDeliveryState::Ambiguous
        }
        (AttemptDeliveryState::Executed, _) | (_, AttemptDeliveryState::Executed) => {
            AttemptDeliveryState::Executed
        }
        _ => AttemptDeliveryState::ProvenNotStarted,
    }
}

fn submit_failure_proves_not_started(
    disposition: Option<MeshClientFailureDisposition>,
    rejection_stage: Option<MeshRejectionStageV1>,
) -> bool {
    matches!(disposition, Some(MeshClientFailureDisposition::PreSend))
        || matches!(rejection_stage, Some(MeshRejectionStageV1::PreAdmission))
}

fn may_migrate_after(delivery: AttemptDeliveryState, replay: BranchReplayContract) -> bool {
    delivery == AttemptDeliveryState::ProvenNotStarted
        || (delivery == AttemptDeliveryState::Executed && replay.allows_replay())
}

fn may_fallback_locally(
    requirement: MeshRequirement,
    policy: MeshLocalFallback,
    delivery: AttemptDeliveryState,
    replay: BranchReplayContract,
) -> bool {
    if requirement == MeshRequirement::Required || policy == MeshLocalFallback::Never {
        return false;
    }
    match policy {
        MeshLocalFallback::PreSend => delivery == AttemptDeliveryState::ProvenNotStarted,
        MeshLocalFallback::Idempotent => may_migrate_after(delivery, replay),
        MeshLocalFallback::Never => false,
    }
}

/// Assign ready branches to the best observed slots without changing branch
/// order. A candidate contributes at most its advertised free slots; when
/// there are more ready actors than observed slots, the ordered pool repeats
/// so live admission can refresh capacity before each actual submission.
fn target_assignment(candidates: &[MeshTargetCandidateV1], actors: usize) -> Vec<usize> {
    if candidates.is_empty() || actors == 0 {
        return Vec::new();
    }
    let mut ordered = (0..candidates.len()).collect::<Vec<_>>();
    ordered.sort_by_key(|index| mesh_target_rank(&candidates[*index]));
    let mut slots = Vec::new();
    let maximum_slots = ordered
        .iter()
        .map(|index| candidates[*index].available_slots.max(1))
        .max()
        .unwrap_or(1);
    for wave in 0..maximum_slots {
        for index in &ordered {
            if candidates[*index].available_slots.max(1) > wave {
                slots.push(*index);
            }
        }
    }
    (0..actors)
        .map(|actor| slots[actor % slots.len()])
        .collect()
}

impl BranchReplayContract {
    fn allows_replay(self) -> bool {
        !matches!(self, Self::Unproven)
    }

    fn token(self) -> &'static str {
        match self {
            Self::Unproven => "unproven",
            Self::DeclaredIdempotent => "declared_idempotent",
        }
    }
}

fn branch_replay_contract(bundle: &ProjectBundle, route_id: &str) -> Result<BranchReplayContract> {
    fn visit<'a>(
        bundle: &'a ProjectBundle,
        route_id: &str,
        seen: &mut BTreeSet<String>,
        routes: &mut Vec<&'a RouteSpec>,
    ) -> Result<()> {
        if !seen.insert(route_id.to_string()) {
            return Ok(());
        }
        let route = bundle
            .route(route_id)
            .with_context(|| format!("mesh route `{route_id}` is missing from its bundle"))?;
        for prerequisite in &route.prerequisites {
            visit(bundle, prerequisite, seen, routes)?;
        }
        routes.push(route);
        Ok(())
    }

    let mut routes = Vec::new();
    visit(bundle, route_id, &mut BTreeSet::new(), &mut routes)?;
    // `pure` is descriptive project metadata, not compiler proof. The only
    // current replay authority is the explicit continuation declaration on
    // every command that can have executed in this workspace island.
    if routes
        .iter()
        .all(|route| route.failure_continuation == RouteFailureContinuation::DeclaredIdempotent)
    {
        return Ok(BranchReplayContract::DeclaredIdempotent);
    }
    Ok(BranchReplayContract::Unproven)
}

fn random_execution_id() -> Result<String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).context("failed to obtain entropy for mesh execution id")?;
    Ok(hex::encode(random))
}

fn actor_id(execution_id: &str, bundle_sha256: &str, route_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"OSTADIX/PROJECT-MESH-ACTOR/V1\0");
    digest.update(execution_id.as_bytes());
    digest.update([0]);
    digest.update(bundle_sha256.as_bytes());
    digest.update([0]);
    digest.update(route_id.as_bytes());
    hex::encode(digest.finalize())
}

#[derive(Debug, Clone)]
struct ResolvedMeshPeer {
    node_id: String,
    addresses: Vec<String>,
    identity: ClientTlsIdentity,
    profile: MeshNodeProfileV1,
    capacity: MeshCapacityV1,
    observed_latency_micros: u64,
    io_timeout: Duration,
}

impl ResolvedMeshPeer {
    fn client_at(&self, address: &str) -> MeshNodeClient {
        MeshNodeClient::new(
            address.to_owned(),
            self.identity.clone(),
            Duration::from_secs(5),
            self.io_timeout,
        )
    }

    fn primary_address(&self) -> &str {
        self.addresses
            .first()
            .map(String::as_str)
            .expect("resolved mesh peer must retain a working endpoint")
    }

    fn connect(&self) -> Result<(MeshNodeClient, MeshNodeConnection)> {
        let mut failures = Vec::new();
        for address in &self.addresses {
            let client = self.client_at(address);
            match client.connect() {
                Ok(mut connection) => match connection.profile() {
                    Ok(profile) if profile == self.profile => return Ok((client, connection)),
                    Ok(profile) => failures.push(format!(
                        "{address}: authenticated profile for `{}` differs from the discovered pinned profile for `{}`",
                        profile.node_id, self.node_id
                    )),
                    Err(error) => failures.push(format!(
                        "{address}: authenticated profile reconciliation failed: {error:#}"
                    )),
                },
                Err(error) => failures.push(format!("{address}: {error:#}")),
            }
        }
        bail!(
            "all pinned endpoints for mesh node `{}` failed: {}",
            self.node_id,
            failures.join("; ")
        )
    }

    fn candidate(&self) -> MeshTargetCandidateV1 {
        MeshTargetCandidateV1 {
            node_id: self.node_id.clone(),
            is_local: false,
            available_slots: self.capacity.available_slots,
            observed_latency_micros: self.observed_latency_micros,
        }
    }
}

fn client_identity(paths: &StoredLanPeerPathsV1, peer: &StoredLanPeerV1) -> ClientTlsIdentity {
    ClientTlsIdentity {
        ca_path: paths.ca.clone(),
        cert_path: paths.client_cert.clone(),
        key_path: paths.client_key.clone(),
        server_name: peer.server_name.clone(),
    }
}

/// Observe already-pinned peers for live planning without changing either end
/// of the mesh. This path performs only LAN advertisement reads, pinned-registry
/// reads, mutual-TLS connection setup, and authenticated profile/capacity RPCs.
/// It does not bootstrap identities, upload CAS objects, probe routes, create
/// fences, submit actors, retrieve results, or start a node.
pub fn observe_mesh_peers_read_only(
    config: &MeshReadOnlyDiscoveryConfig,
) -> Result<MeshReadOnlyDiscoveryV1> {
    config.validate()?;
    let peers_root = config
        .peer_root
        .clone()
        .unwrap_or_else(lan_peers_config_dir);
    let (discovered, lan_discovery_error) = if config.discover_lan {
        match discover_lan_nodes(config.discovery_timeout) {
            Ok(nodes) => (nodes, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        }
    } else {
        (Vec::new(), None)
    };
    let known = list_stored_lan_peers(&peers_root).with_context(|| {
        format!(
            "mesh peer registry {} is unreadable or corrupt",
            peers_root.display()
        )
    })?;

    let mut peers = Vec::with_capacity(known.len());
    for (peer, paths) in known {
        let mut endpoint_hints = discovered
            .iter()
            .filter(|node| {
                node.advertisement.node_id == peer.node_id
                    && node.advertisement.server_name == peer.server_name
            })
            .map(|node| node.service_address().to_string())
            .collect::<Vec<_>>();
        endpoint_hints.push(peer.address.clone());
        endpoint_hints.sort();
        endpoint_hints.dedup();

        let identity = client_identity(&paths, &peer);
        let mut failures = Vec::new();
        let mut strongest_error = MeshLivePeerErrorV1::Unreachable;
        let mut selected = None;
        for address in &endpoint_hints {
            let client = MeshNodeClient::new(
                address.clone(),
                identity.clone(),
                Duration::from_secs(5),
                Duration::from_secs(10),
            );
            let started = Instant::now();
            let mut connection = match client.connect() {
                Ok(connection) => connection,
                Err(error) => {
                    failures.push(format!("{address}: {error:#}"));
                    continue;
                }
            };
            let profile = match connection.profile() {
                Ok(profile) => profile,
                Err(error) => {
                    strongest_error = strongest_error.max(MeshLivePeerErrorV1::ProfileQueryFailed);
                    failures.push(format!("{address}: profile query failed: {error:#}"));
                    continue;
                }
            };
            if profile.node_id != peer.node_id {
                strongest_error = strongest_error.max(MeshLivePeerErrorV1::ProfileIdentityMismatch);
                failures.push(format!(
                    "{address}: authenticated profile node id `{}` differs from pinned peer `{}`",
                    profile.node_id, peer.node_id
                ));
                continue;
            }
            let capacity = match connection.capacity() {
                Ok(capacity) => capacity,
                Err(error) => {
                    strongest_error = strongest_error.max(MeshLivePeerErrorV1::CapacityQueryFailed);
                    failures.push(format!("{address}: capacity query failed: {error:#}"));
                    continue;
                }
            };
            if let Err(error) = capacity.validate_against(&profile) {
                strongest_error = strongest_error.max(MeshLivePeerErrorV1::CapacityInvalid);
                failures.push(format!(
                    "{address}: invalid capacity observation: {error:#}"
                ));
                continue;
            }
            selected = Some((
                address.clone(),
                profile,
                capacity,
                started.elapsed().as_micros().try_into().unwrap_or(u64::MAX),
            ));
            break;
        }

        peers.push(match selected {
            Some((selected_endpoint, profile, capacity, observed_latency_micros)) => {
                let eligible = capacity.available_slots > 0;
                MeshLivePeerObservationV1 {
                    node_id: peer.node_id,
                    endpoint_hints,
                    selected_endpoint: Some(selected_endpoint),
                    profile: Some(profile),
                    capacity: Some(capacity),
                    observed_latency_micros: Some(observed_latency_micros),
                    eligible,
                    rejection: (!eligible).then_some(MeshLivePeerRejectionV1::NoAvailableSlots),
                    error: None,
                    detail: None,
                }
            }
            None => MeshLivePeerObservationV1 {
                node_id: peer.node_id,
                endpoint_hints,
                selected_endpoint: None,
                profile: None,
                capacity: None,
                observed_latency_micros: None,
                eligible: false,
                rejection: None,
                error: Some(strongest_error),
                detail: Some(if failures.is_empty() {
                    "pinned peer has no usable endpoint".to_string()
                } else {
                    failures.join("; ")
                }),
            },
        });
    }

    let observation = MeshReadOnlyDiscoveryV1 {
        schema: MESH_READ_ONLY_DISCOVERY_SCHEMA_V1.to_string(),
        lan_discovery_attempted: config.discover_lan,
        lan_discovery_error,
        peers,
    };
    observation.validate()?;
    Ok(observation)
}

/// Join the live LAN advertisements with the durable paired-peer registry.
/// Discovery may refresh an endpoint, but it never substitutes the stored TLS
/// identity. Legacy LAN-open advertisements can enroll through their existing
/// explicit bootstrap contract; pairing-required peers must already be pinned.
fn discover_mesh_peers(
    config: &MeshExecutionConfig,
    trace: &mut MeshExecutionTraceV1,
) -> Result<Vec<ResolvedMeshPeer>> {
    let peers_root = config
        .peer_root
        .clone()
        .unwrap_or_else(lan_peers_config_dir);
    let discovered = if config.discover_lan {
        match discover_lan_nodes(config.discovery_timeout) {
            Ok(nodes) => nodes,
            Err(error) => {
                if config.explain {
                    eprintln!("o-link mesh: LAN discovery unavailable: {error:#}");
                }
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // Enroll only the legacy LAN-open mode. A pairing-required advertisement
    // remains a routing hint until the user completes passcode pairing.
    let mut known = list_stored_lan_peers(&peers_root).with_context(|| {
        format!(
            "mesh peer registry {} is unreadable or corrupt",
            peers_root.display()
        )
    })?;
    let mut known_ids = known
        .iter()
        .map(|(peer, _)| peer.node_id.clone())
        .collect::<BTreeSet<_>>();
    for node in &discovered {
        if known_ids.contains(&node.advertisement.node_id)
            || node.advertisement.is_pairing_required()
        {
            continue;
        }
        match fetch_lan_bootstrap(node, Duration::from_secs(5))
            .and_then(|bundle| store_lan_peer(&peers_root, node, &bundle))
        {
            Ok(peer) => {
                known_ids.insert(node.advertisement.node_id.clone());
                known.push(peer);
            }
            Err(error) if config.explain => eprintln!(
                "o-link mesh: could not enroll LAN-open node {}: {error:#}",
                node.advertisement.node_id
            ),
            Err(_) => {}
        }
    }
    known.sort_by(|left, right| left.0.node_id.cmp(&right.0.node_id));
    known.dedup_by(|left, right| left.0.node_id == right.0.node_id);

    // Route deadlines govern actor workers, not control-plane RPCs. Keeping
    // this short also bounds how long a cancelled race loser can hold the
    // coordinator while a peer or connection is unhealthy.
    let io_timeout = Duration::from_secs(10);
    let mut resolved = Vec::new();
    for (peer, paths) in known {
        let mut addresses = discovered
            .iter()
            .filter(|node| {
                node.advertisement.node_id == peer.node_id
                    && node.advertisement.server_name == peer.server_name
            })
            .map(|node| node.service_address().to_string())
            .collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        if !addresses.contains(&peer.address) {
            addresses.push(peer.address.clone());
        }

        let identity = client_identity(&paths, &peer);
        let mut failures = Vec::new();
        let mut selected = None;
        for (index, address) in addresses.iter().enumerate() {
            let client = MeshNodeClient::new(
                address.clone(),
                identity.clone(),
                Duration::from_secs(5),
                Duration::from_secs(10),
            );
            let started = std::time::Instant::now();
            match client.connect().and_then(|mut connection| {
                connection.profile().and_then(|profile| {
                    profile.validate()?;
                    if profile.node_id != peer.node_id {
                        bail!(
                            "authenticated mesh profile node id `{}` differs from pinned peer `{}`",
                            profile.node_id,
                            peer.node_id
                        );
                    }
                    let capacity = connection.capacity()?;
                    capacity.validate_against(&profile)?;
                    if capacity.node_id != peer.node_id {
                        bail!(
                            "mesh capacity node id `{}` differs from pinned peer `{}`",
                            capacity.node_id,
                            peer.node_id
                        );
                    }
                    Ok((profile, capacity))
                })
            }) {
                Ok((profile, capacity)) => {
                    let mut preferred_addresses = addresses.clone();
                    preferred_addresses.rotate_left(index);
                    selected = Some(ResolvedMeshPeer {
                        node_id: peer.node_id.clone(),
                        addresses: preferred_addresses,
                        identity: identity.clone(),
                        profile,
                        capacity,
                        observed_latency_micros: started
                            .elapsed()
                            .as_micros()
                            .try_into()
                            .unwrap_or(u64::MAX),
                        io_timeout,
                    });
                    break;
                }
                Err(error) => failures.push(format!("{address}: {error:#}")),
            }
        }

        match selected {
            Some(peer) => {
                let has_capacity = peer.capacity.available_slots > 0;
                trace.candidates.push(MeshTraceCandidateV1 {
                    node_id: peer.node_id.clone(),
                    address: Some(peer.primary_address().to_owned()),
                    available_slots: peer.capacity.available_slots,
                    observed_latency_micros: peer.observed_latency_micros,
                    eligible: has_capacity,
                    detail: if has_capacity {
                        format!(
                            "authenticated mesh profile; max parallel {}",
                            peer.profile.max_parallel
                        )
                    } else {
                        "authenticated mesh profile currently has no free actor slots".to_string()
                    },
                });
                if has_capacity {
                    resolved.push(peer);
                }
            }
            None => trace.candidates.push(MeshTraceCandidateV1 {
                node_id: peer.node_id,
                address: None,
                available_slots: 0,
                observed_latency_micros: u64::MAX,
                eligible: false,
                detail: if failures.is_empty() {
                    "peer registry contained no usable endpoint".to_string()
                } else {
                    failures.join("; ")
                },
            }),
        }
    }
    resolved.sort_by_key(|peer| mesh_target_rank(&peer.candidate()));
    Ok(resolved)
}

#[derive(Debug)]
enum RemoteAttemptOutcome {
    Settled(Box<OExecutionResult>),
    Failed {
        delivery: AttemptDeliveryState,
        detail: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorStopReason {
    RaceCancellation,
    WallClockDeadline,
}

#[derive(Debug, Clone, Copy)]
struct RemotePollPolicy {
    deadline: Instant,
    termination_grace_period: Duration,
    max_result_bytes: u64,
    max_result_chunk_bytes: u32,
}

fn trace_event(events: &Arc<Mutex<Vec<MeshTraceEventV1>>>, event: MeshTraceEventV1) {
    match events.lock() {
        Ok(mut events) => events.push(event),
        Err(poisoned) => poisoned.into_inner().push(event),
    }
}

fn route_requirements(
    bundle: &ProjectBundle,
    upload: &MeshArtifactUploadV1,
    route_id: &str,
    opts: &RunOptions,
) -> Result<MeshRouteRequirementsV1> {
    let projection = mesh_project_ir_projection(bundle, route_id)
        .context("failed to construct the canonical mesh actor project IR")?;
    let mut requirements = MeshRouteRequirementsV1::new(
        upload.artifact.clone(),
        route_id.to_string(),
        projection.sha256,
        mesh_route_contract_sha256(bundle, route_id)?,
    );
    requirements.resources.project_ir_bytes = projection.canonical_bytes_len;
    requirements.execution_limits = MeshExecutionLimitsV1::from_run_options(opts);
    requirements.validate()?;
    Ok(requirements)
}

fn validate_result_artifact(bytes: &[u8], result: &MeshActorResultV1) -> Result<()> {
    let observed = crate::hosted_remote::mesh::MeshArtifactIdV1::for_bytes(bytes);
    if observed != result.artifact {
        bail!(
            "mesh actor result artifact mismatch: expected {} bytes {}, received {} bytes {}",
            result.artifact.sha256,
            result.artifact.bytes,
            observed.sha256,
            observed.bytes
        );
    }
    Ok(())
}

fn validate_result_receive_manifest(
    result: &MeshActorResultV1,
    max_result_bytes: u64,
    max_result_chunk_bytes: u32,
) -> Result<()> {
    result.validate()?;
    if result.artifact.bytes > max_result_bytes {
        bail!(
            "mesh actor result declares {} bytes; authenticated node profile maximum is {max_result_bytes}",
            result.artifact.bytes
        );
    }
    if let Some(chunk) = result
        .chunks
        .iter()
        .find(|chunk| chunk.bytes > max_result_chunk_bytes)
    {
        bail!(
            "mesh actor result chunk declares {} bytes; authenticated node profile maximum is {max_result_chunk_bytes}",
            chunk.bytes
        );
    }
    Ok(())
}

fn fetch_actor_result(
    peer: &ResolvedMeshPeer,
    connection: &mut MeshNodeConnection,
    actor_ref: &MeshActorRefV1,
    result: &MeshActorResultV1,
    max_result_bytes: u64,
    max_result_chunk_bytes: u32,
) -> Result<OExecutionResult> {
    validate_result_receive_manifest(result, max_result_bytes, max_result_chunk_bytes)?;
    let initial_capacity = result.artifact.bytes.min(64 * 1024 * 1024) as usize;
    let mut encoded = Vec::with_capacity(initial_capacity);
    for (index, expected) in result.chunks.iter().enumerate() {
        let index = u32::try_from(index).context("mesh result has too many chunks")?;
        let response = match connection.result_chunk(actor_ref.clone(), index) {
            Ok(response) => response,
            Err(first_error) => {
                let (_, mut recovered) = peer.connect().with_context(|| {
                    format!(
                        "result chunk {index} failed on the actor stream ({first_error:#}) and same-node reconnection failed"
                    )
                })?;
                let response = recovered
                    .result_chunk(actor_ref.clone(), index)
                    .with_context(|| {
                        format!(
                            "result chunk {index} failed on both the actor stream and a same-node reconciliation stream"
                        )
                    })?;
                *connection = recovered;
                response
            }
        };
        if response.actor != actor_ref.actor
            || response.spec_sha256 != actor_ref.spec_sha256
            || response.index != index
            || response.total_chunks as usize != result.chunks.len()
            || response.chunk != *expected
        {
            bail!("mesh result chunk {index} does not match the terminal actor manifest");
        }
        let assembled = encoded
            .len()
            .checked_add(response.bytes.len())
            .context("mesh result assembled length overflowed usize")?;
        if u64::try_from(assembled).unwrap_or(u64::MAX) > max_result_bytes
            || u64::try_from(assembled).unwrap_or(u64::MAX) > result.artifact.bytes
        {
            bail!("mesh result chunks exceed the bound terminal artifact length");
        }
        encoded.extend_from_slice(&response.bytes);
    }
    validate_result_artifact(&encoded, result)?;
    let decoded: OExecutionResult =
        serde_json::from_slice(&encoded).context("mesh actor result JSON is malformed")?;
    if decoded.succeeded() != result.route_succeeded || decoded.exit_code != result.exit_code {
        bail!("mesh actor terminal summary disagrees with its decoded route result");
    }
    Ok(decoded)
}

fn poll_remote_actor(
    peer: &ResolvedMeshPeer,
    mut connection: MeshNodeConnection,
    actor_ref: &MeshActorRefV1,
    initial: crate::hosted_remote::mesh::MeshActorStatusV1,
    cancel: &CancellationToken,
    policy: RemotePollPolicy,
) -> RemoteAttemptOutcome {
    let mut status = initial;
    let mut stop_reason = None;
    let mut cancellation_settlement_deadline = None;
    loop {
        match status.phase {
            MeshActorPhaseV1::Succeeded { ref result } => {
                return fetch_actor_result(
                    peer,
                    &mut connection,
                    actor_ref,
                    result,
                    policy.max_result_bytes,
                    policy.max_result_chunk_bytes,
                )
                .map(|result| RemoteAttemptOutcome::Settled(Box::new(result)))
                .unwrap_or_else(|error| RemoteAttemptOutcome::Failed {
                    delivery: AttemptDeliveryState::Executed,
                    detail: format!("terminal mesh result validation failed: {error:#}"),
                });
            }
            MeshActorPhaseV1::Failed {
                ref code,
                ref message,
                retryable,
            } => {
                if code == "route-cancelled" {
                    return match stop_reason {
                        Some(ActorStopReason::RaceCancellation) => {
                            RemoteAttemptOutcome::Cancelled
                        }
                        Some(ActorStopReason::WallClockDeadline) => {
                            RemoteAttemptOutcome::Failed {
                                delivery: AttemptDeliveryState::Executed,
                                detail: format!(
                                    "mesh actor exceeded the route wall-clock deadline and durably settled cancellation: {message}"
                                ),
                            }
                        }
                        None => RemoteAttemptOutcome::Failed {
                            delivery: AttemptDeliveryState::Executed,
                            detail: format!(
                                "mesh actor recorded an unsolicited cancellation: {message}"
                            ),
                        },
                    };
                }
                return RemoteAttemptOutcome::Failed {
                    delivery: AttemptDeliveryState::Executed,
                    detail: format!(
                        "mesh actor failed with {code} (retryable={retryable}): {message}"
                    ),
                };
            }
            MeshActorPhaseV1::Indeterminate => {
                return RemoteAttemptOutcome::Failed {
                    delivery: AttemptDeliveryState::Ambiguous,
                    detail: "destination retained an indeterminate actor record".to_string(),
                };
            }
            MeshActorPhaseV1::Running => {}
        }

        if stop_reason.is_none() {
            let reason = if cancel.is_cancelled() {
                Some(ActorStopReason::RaceCancellation)
            } else if Instant::now() >= policy.deadline {
                Some(ActorStopReason::WallClockDeadline)
            } else {
                None
            };
            if let Some(reason) = reason {
                let cancellation = match connection.cancel_actor(actor_ref.clone()) {
                    Ok(cancellation) => cancellation,
                    Err(first_error) => match peer.connect().and_then(|(_, mut recovered)| {
                        recovered
                            .cancel_actor(actor_ref.clone())
                            .map(|cancellation| (recovered, cancellation))
                    }) {
                        Ok((recovered, cancellation)) => {
                            connection = recovered;
                            cancellation
                        }
                        Err(reconcile_error) => {
                            return RemoteAttemptOutcome::Failed {
                                delivery: AttemptDeliveryState::Ambiguous,
                                detail: format!(
                                    "mesh actor cancellation failed on the actor stream ({first_error:#}) and same-node reconciliation failed: {reconcile_error:#}"
                                ),
                            }
                        }
                    },
                };
                stop_reason = Some(reason);
                cancellation_settlement_deadline = Some(
                    Instant::now()
                        .checked_add(policy.termination_grace_period)
                        .unwrap_or_else(Instant::now),
                );
                status = cancellation.status;
                continue;
            }
        } else if cancellation_settlement_deadline.is_some_and(|limit| Instant::now() >= limit) {
            return RemoteAttemptOutcome::Failed {
                delivery: AttemptDeliveryState::Ambiguous,
                detail: match stop_reason {
                    Some(ActorStopReason::RaceCancellation) => {
                        "race-loser cancellation did not durably settle before its grace deadline"
                            .to_string()
                    }
                    Some(ActorStopReason::WallClockDeadline) => {
                        "timed-out mesh actor did not durably settle cancellation before its grace deadline"
                            .to_string()
                    }
                    None => unreachable!("a cancellation deadline requires a stop reason"),
                },
            };
        }
        std::thread::sleep(Duration::from_millis(250));
        status = match connection.actor_status(actor_ref.clone()) {
            Ok(status) => status,
            Err(first_error) => {
                match peer.connect().and_then(|(_, mut recovered)| {
                    recovered
                        .actor_status(actor_ref.clone())
                        .map(|status| (recovered, status))
                }) {
                    Ok((recovered, status)) => {
                        connection = recovered;
                        status
                    }
                    Err(reconcile_error) => {
                        return RemoteAttemptOutcome::Failed {
                            delivery: AttemptDeliveryState::Ambiguous,
                            detail: format!(
                                "running mesh actor status failed on the persistent stream ({first_error:#}) and same-node reconciliation failed: {reconcile_error:#}"
                            ),
                        };
                    }
                }
            }
        };
    }
}

fn execute_remote_attempt(
    peer: &ResolvedMeshPeer,
    upload: &MeshArtifactUploadV1,
    requirements: &MeshRouteRequirementsV1,
    actor: MeshActorIdV1,
    cancel: &CancellationToken,
    opts: &RunOptions,
    on_submit: impl FnOnce(),
) -> RemoteAttemptOutcome {
    if cancel.is_cancelled() {
        return RemoteAttemptOutcome::Cancelled;
    }
    let (_, mut connection) = match peer.connect() {
        Ok(connected) => connected,
        Err(error) => {
            return RemoteAttemptOutcome::Failed {
                delivery: AttemptDeliveryState::ProvenNotStarted,
                detail: format!(
                    "connection to {} failed before actor submission: {error:#}",
                    peer.node_id
                ),
            };
        }
    };
    if cancel.is_cancelled() {
        return RemoteAttemptOutcome::Cancelled;
    }
    if let Err(error) = connection.upload_artifact(upload) {
        return RemoteAttemptOutcome::Failed {
            delivery: AttemptDeliveryState::ProvenNotStarted,
            detail: format!("bundle upload to {} failed: {error:#}", peer.node_id),
        };
    }
    if cancel.is_cancelled() {
        return RemoteAttemptOutcome::Cancelled;
    }
    let probe = match connection.probe_route(requirements.clone()) {
        Ok(probe) => probe,
        Err(error) => {
            return RemoteAttemptOutcome::Failed {
                delivery: AttemptDeliveryState::ProvenNotStarted,
                detail: format!("route probe on {} failed: {error:#}", peer.node_id),
            }
        }
    };
    if cancel.is_cancelled() {
        return RemoteAttemptOutcome::Cancelled;
    }
    if probe.node_id != peer.node_id || probe.requirements != *requirements {
        return RemoteAttemptOutcome::Failed {
            delivery: AttemptDeliveryState::ProvenNotStarted,
            detail: format!(
                "route probe on {} returned substituted identity",
                peer.node_id
            ),
        };
    }
    if !probe.eligible {
        return RemoteAttemptOutcome::Failed {
            delivery: AttemptDeliveryState::ProvenNotStarted,
            detail: format!(
                "route is ineligible on {}: {}",
                peer.node_id,
                probe.missing.join(", ")
            ),
        };
    }
    if probe.available_slots == 0 {
        return RemoteAttemptOutcome::Failed {
            delivery: AttemptDeliveryState::ProvenNotStarted,
            detail: format!(
                "route is eligible on {} but has no free actor slot",
                peer.node_id
            ),
        };
    }

    let spec = MeshActorSpecV1::new(actor.clone(), requirements.clone(), peer.node_id.clone());
    let actor_ref = match spec.actor_ref() {
        Ok(actor_ref) => actor_ref,
        Err(error) => {
            return RemoteAttemptOutcome::Failed {
                delivery: AttemptDeliveryState::ProvenNotStarted,
                detail: format!("mesh actor spec is invalid before submission: {error:#}"),
            }
        }
    };
    let deadline = Instant::now()
        .checked_add(opts.limits.wall_clock_timeout)
        .unwrap_or_else(Instant::now);
    let poll_policy = RemotePollPolicy {
        deadline,
        termination_grace_period: opts.limits.termination_grace_period,
        max_result_bytes: peer.profile.max_result_bytes,
        max_result_chunk_bytes: peer.profile.max_result_chunk_bytes,
    };
    if cancel.is_cancelled() {
        return RemoteAttemptOutcome::Cancelled;
    }
    // This callback is the trace boundary: it runs only after every pre-submit
    // cancellation check and immediately before the first admission RPC.
    on_submit();
    match connection.execute_actor(spec) {
        Ok(status) => poll_remote_actor(peer, connection, &actor_ref, status, cancel, poll_policy),
        Err(submit_error) => {
            if submit_failure_proves_not_started(
                mesh_client_failure_disposition(&submit_error),
                mesh_client_rejection_stage(&submit_error),
            ) {
                return RemoteAttemptOutcome::Failed {
                    delivery: AttemptDeliveryState::ProvenNotStarted,
                    detail: format!(
                        "mesh node proved the actor request was not admitted: {submit_error:#}"
                    ),
                };
            }
            // A lost submit response is ambiguous. Reconcile the exact actor
            // generation on the same authenticated peer. The fence operation
            // atomically observes an existing admission or durably prevents a
            // delayed request from starting before proving absence.
            match peer.connect().and_then(|(_, mut reconciliation)| {
                reconciliation
                    .fence_actor_if_absent(actor_ref.clone())
                    .map(|fence| (reconciliation, fence))
            }) {
                Ok((reconciliation, MeshActorFenceV1::Existing(status))) => {
                    poll_remote_actor(
                            peer,
                            reconciliation,
                        &actor_ref,
                        status,
                        cancel,
                        poll_policy,
                    )
                }
                Ok((_, MeshActorFenceV1::FencedAbsent(_))) => RemoteAttemptOutcome::Failed {
                    delivery: AttemptDeliveryState::ProvenNotStarted,
                    detail: format!(
                        "mesh submit failed and destination durably fenced the actor absent: {submit_error:#}"
                    ),
                },
                Err(status_error) => RemoteAttemptOutcome::Failed {
                    delivery: AttemptDeliveryState::Ambiguous,
                    detail: format!(
                        "mesh submit outcome is ambiguous ({submit_error:#}); exact status reconciliation also failed: {status_error:#}"
                    ),
                },
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_route_actor(
    bundle: &ProjectBundle,
    upload: &MeshArtifactUploadV1,
    route_id: &str,
    peers: &[ResolvedMeshPeer],
    preferred_peer: usize,
    execution_id: &str,
    opts: &RunOptions,
    config: &MeshExecutionConfig,
    events: &Arc<Mutex<Vec<MeshTraceEventV1>>>,
    cancel: CancellationToken,
) -> Result<OExecutionResult> {
    let bundle_sha256 = &upload.artifact.sha256;
    let actor_id = actor_id(execution_id, bundle_sha256, route_id);
    let replay = branch_replay_contract(bundle, route_id)?;
    let requirements = route_requirements(bundle, upload, route_id, opts)?;
    let attempts = config
        .max_retries
        .checked_add(1)
        .context("mesh attempt count overflowed")?;
    let mut last_delivery = AttemptDeliveryState::ProvenNotStarted;
    let mut last_failure = "no eligible remote mesh peer was discovered".to_string();
    let mut last_node: Option<String> = None;
    let mut last_settled_failure: Option<OExecutionResult> = None;
    let mut attempted_generations = 0_u32;

    for offset in 0..attempts {
        if cancel.is_cancelled() {
            return Err(RouteExecutionError::Cancelled {
                route_id: route_id.to_string(),
            }
            .into());
        }
        if peers.is_empty() {
            break;
        }
        if offset > 0 && !may_migrate_after(last_delivery, replay) {
            trace_event(
                events,
                MeshTraceEventV1::RetryDenied {
                    route_id: route_id.to_string(),
                    actor_id: actor_id.clone(),
                    generation: attempted_generations,
                    reason: format!(
                        "{} delivery cannot replay a {} route island",
                        match last_delivery {
                            AttemptDeliveryState::ProvenNotStarted => "pre-send",
                            AttemptDeliveryState::Ambiguous => "ambiguous",
                            AttemptDeliveryState::Executed => "executed",
                        },
                        replay.token()
                    ),
                },
            );
            break;
        }
        let peer = &peers[(preferred_peer + offset as usize) % peers.len()];
        let generation = u64::from(offset) + 1;
        let actor = MeshActorIdV1::new(actor_id.clone(), generation);
        let generation_u32 = u32::try_from(generation).unwrap_or(u32::MAX);
        let migration_from = last_node.clone();
        let mut submitted = false;
        let outcome =
            execute_remote_attempt(peer, upload, &requirements, actor, &cancel, opts, || {
                submitted = true;
                if let Some(from_node) = migration_from
                    .as_ref()
                    .filter(|from_node| *from_node != &peer.node_id)
                {
                    trace_event(
                        events,
                        MeshTraceEventV1::Migrated {
                            route_id: route_id.to_string(),
                            actor_id: actor_id.clone(),
                            from_generation: attempted_generations,
                            to_generation: generation_u32,
                            from_node_id: from_node.clone(),
                            to_node_id: peer.node_id.clone(),
                            replay_contract: replay.token().to_string(),
                        },
                    );
                }
                trace_event(
                    events,
                    MeshTraceEventV1::Dispatched {
                        route_id: route_id.to_string(),
                        actor_id: actor_id.clone(),
                        generation: generation_u32,
                        node_id: peer.node_id.clone(),
                    },
                );
            });
        if submitted {
            attempted_generations = generation_u32;
            last_node = Some(peer.node_id.clone());
        }

        match outcome {
            RemoteAttemptOutcome::Settled(result) => {
                let result = *result;
                let succeeded = result.succeeded();
                trace_event(
                    events,
                    MeshTraceEventV1::Settled {
                        route_id: route_id.to_string(),
                        actor_id: actor_id.clone(),
                        generation: attempted_generations,
                        node_id: peer.node_id.clone(),
                        succeeded,
                    },
                );
                if succeeded || result.was_guard_skipped() {
                    return Ok(result);
                }
                last_delivery = merge_delivery_state(last_delivery, AttemptDeliveryState::Executed);
                last_failure = format!(
                    "mesh actor settled unsuccessfully on {} with exit {:?}",
                    peer.node_id, result.exit_code
                );
                last_settled_failure = Some(result);
            }
            RemoteAttemptOutcome::Cancelled => {
                return Err(RouteExecutionError::Cancelled {
                    route_id: route_id.to_string(),
                }
                .into())
            }
            RemoteAttemptOutcome::Failed { delivery, detail } => {
                trace_event(
                    events,
                    MeshTraceEventV1::AttemptFailed {
                        route_id: route_id.to_string(),
                        actor_id: actor_id.clone(),
                        generation: generation_u32,
                        node_id: peer.node_id.clone(),
                        submitted,
                        delivery: match delivery {
                            AttemptDeliveryState::ProvenNotStarted => "proven_not_started",
                            AttemptDeliveryState::Ambiguous => "ambiguous",
                            AttemptDeliveryState::Executed => "executed",
                        }
                        .to_string(),
                        replay_contract: replay.token().to_string(),
                        reason: detail.clone(),
                    },
                );
                last_delivery = merge_delivery_state(last_delivery, delivery);
                last_failure = detail;
                if delivery != AttemptDeliveryState::ProvenNotStarted {
                    last_settled_failure = None;
                }
            }
        }
    }

    if cancel.is_cancelled() {
        return Err(RouteExecutionError::Cancelled {
            route_id: route_id.to_string(),
        }
        .into());
    }
    if may_fallback_locally(
        config.requirement,
        config.local_fallback,
        last_delivery,
        replay,
    ) {
        trace_event(
            events,
            MeshTraceEventV1::LocalFallback {
                route_id: route_id.to_string(),
                actor_id,
                after_generation: attempted_generations,
                replay_contract: replay.token().to_string(),
                reason: last_failure.clone(),
            },
        );
        return crate::project::runtime::run_route_cancellable(bundle, route_id, opts, cancel)
            .with_context(|| format!("local fallback for mesh actor `{route_id}` failed"));
    }
    if last_delivery == AttemptDeliveryState::Executed {
        if let Some(result) = last_settled_failure {
            return Ok(result);
        }
    }
    bail!(
        "mesh actor `{route_id}` did not settle and local fallback is not authorized: {last_failure}"
    )
}

/// Execute a resolved route selection through discovered mesh peers, with
/// bounded retry and a policy-governed local provider. The transport-backed
/// implementation is below the pure scheduling helpers in this module.
pub fn execute_mesh_selection_observed(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
    config: &MeshExecutionConfig,
) -> Result<MeshExecutionOutcome> {
    config.validate()?;
    let selection = resolve_selection(bundle, target, policy_override)?;
    let bundle_bytes = crate::project::bundle::serialize(bundle)?;
    let bundle_sha256 = hex::encode(Sha256::digest(&bundle_bytes));
    let execution_id = random_execution_id()?;
    let mut trace = MeshExecutionTraceV1::new(
        execution_id,
        bundle_sha256,
        selection.target.clone(),
        &selection.policy,
    );
    let limit_validation = potential_route_execution_count(bundle, &selection.alternatives)
        .and_then(|count| {
            opts.limits
                .validate_route_execution_set(count)
                .map_err(anyhow::Error::new)
        });
    if let Err(error) = limit_validation {
        let trace_retention = trace.validate().and_then(|()| {
            config
                .trace_out
                .as_deref()
                .map_or(Ok(()), |path| trace.write(path))
        });
        let error = match trace_retention {
            Ok(()) => error,
            Err(trace_error) => error.context(format!(
                "additionally failed to validate or retain mesh trace: {trace_error:#}"
            )),
        };
        return Err(anyhow::Error::new(MeshExecutionError::new(error, trace)));
    }

    let execution = execute_mesh_policy(
        bundle,
        &bundle_bytes,
        &selection.alternatives,
        &selection.policy,
        opts,
        config,
        &mut trace,
    );
    let trace_retention = trace.validate().and_then(|()| {
        config
            .trace_out
            .as_deref()
            .map_or(Ok(()), |path| trace.write(path))
    });

    match (execution, trace_retention) {
        (Ok(selection_execution), Ok(())) => Ok(MeshExecutionOutcome {
            execution: ConfiguredProjectExecution {
                results: selection_execution.results,
                trace: None,
                validated_selection_receipt: selection_execution.validated_selection_receipt,
                validated_selection_measurements: selection_execution
                    .validated_selection_measurements,
            },
            trace,
        }),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => {
            Err(anyhow::Error::new(MeshExecutionError::new(error, trace)))
        }
        (Err(error), Err(trace_error)) => {
            let error = error.context(format!(
                "additionally failed to validate or retain mesh trace: {trace_error:#}"
            ));
            Err(anyhow::Error::new(MeshExecutionError::new(error, trace)))
        }
    }
}

/// Compatibility wrapper retaining the result-plus-project-trace surface.
/// Mesh-aware callers should use [`execute_mesh_selection_observed`] so the
/// mesh trace is not discarded on success.
pub fn execute_mesh_selection(
    bundle: &ProjectBundle,
    target: Option<&str>,
    policy_override: Option<RoutePolicy>,
    opts: &RunOptions,
    config: &MeshExecutionConfig,
) -> Result<ConfiguredProjectExecution> {
    Ok(execute_mesh_selection_observed(bundle, target, policy_override, opts, config)?.execution)
}

fn execute_mesh_policy(
    bundle: &ProjectBundle,
    bundle_bytes: &[u8],
    alternatives: &[String],
    policy: &RoutePolicy,
    opts: &RunOptions,
    config: &MeshExecutionConfig,
    trace: &mut MeshExecutionTraceV1,
) -> Result<RouteSelectionExecution> {
    let mut peers = discover_mesh_peers(config, trace)?;
    let bundle_bytes_len = u64::try_from(bundle_bytes.len()).unwrap_or(u64::MAX);
    let requested_limits = MeshExecutionLimitsV1::from_run_options(opts);
    peers.retain(|peer| {
        let rejection = if bundle_bytes_len > peer.profile.max_artifact_bytes {
            Some(format!(
                "bundle is {bundle_bytes_len} bytes; authenticated node maximum is {}",
                peer.profile.max_artifact_bytes
            ))
        } else if !requested_limits.fits_within(&peer.profile.execution_limit_ceiling) {
            Some("requested execution policy exceeds the authenticated node ceiling".to_string())
        } else {
            None
        };
        if let Some(detail) = rejection {
            if let Some(candidate) = trace
                .candidates
                .iter_mut()
                .find(|candidate| candidate.node_id == peer.node_id)
            {
                candidate.eligible = false;
                candidate.detail = detail;
            }
            false
        } else {
            true
        }
    });
    if config.requirement == MeshRequirement::Required && peers.is_empty() {
        return Err(RequiredMeshPlacementUnavailable.into());
    }
    if config.explain {
        eprintln!(
            "o-link mesh: resolved {} authenticated peer(s) with free capacity for {} route alternative(s)",
            peers.len(),
            alternatives.len()
        );
        for peer in &peers {
            eprintln!(
                "o-link mesh: candidate {} at {} slots={} latency={}us",
                peer.node_id,
                peer.primary_address(),
                peer.capacity.available_slots,
                peer.observed_latency_micros
            );
        }
    }

    let chunk_bytes = peers
        .iter()
        .map(|peer| usize::try_from(peer.profile.max_chunk_bytes).unwrap_or(usize::MAX))
        .min()
        .unwrap_or(512 * 1024)
        .min(512 * 1024);
    let upload = mesh_bundle_artifact(bundle_bytes, chunk_bytes)?;
    let execution_id = trace.execution_id.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let candidates = peers
        .iter()
        .map(ResolvedMeshPeer::candidate)
        .collect::<Vec<_>>();
    let assignments = target_assignment(&candidates, alternatives.len());
    let preferred = |index: usize| assignments.get(index).copied().unwrap_or(0);

    let dispatch_one = |index: usize, route_id: &str, cancel: CancellationToken| {
        dispatch_route_actor(
            bundle,
            &upload,
            route_id,
            &peers,
            preferred(index),
            &execution_id,
            opts,
            config,
            &events,
            cancel,
        )
    };
    let outcome = (|| -> Result<RouteSelectionExecution> {
        if matches!(policy, RoutePolicy::BenchmarkValidateAndSelect) {
            let measured = run_all_alternatives_parallel_measured(alternatives, &dispatch_one)?;
            return benchmark_validate_and_select(bundle, &trace.target, alternatives, measured);
        }
        let results = match policy {
            RoutePolicy::Explicit(_) | RoutePolicy::Default => vec![dispatch_one(
                0,
                alternatives
                    .first()
                    .context("resolved mesh selection lost its route")?,
                CancellationToken::new(),
            )?],
            RoutePolicy::Fallback | RoutePolicy::AnySuccess => {
                let mut results = Vec::new();
                for (index, route_id) in alternatives.iter().enumerate() {
                    let result = dispatch_one(index, route_id, CancellationToken::new())?;
                    let succeeded = result.succeeded();
                    let skipped = result.was_guard_skipped();
                    results.push(result);
                    if succeeded {
                        break;
                    }
                    if !skipped && !branch_replay_contract(bundle, route_id)?.allows_replay() {
                        bail!(
                        "route `{route_id}` settled unsuccessfully, but its workspace island has no declared-idempotent continuation contract"
                    );
                    }
                }
                results
            }
            RoutePolicy::All => run_mesh_all_parallel(alternatives, &dispatch_one)?,
            RoutePolicy::VerifyEquivalent => {
                let results = run_mesh_all_parallel(alternatives, &dispatch_one)?;
                let failures = results
                    .iter()
                    .filter(|result| !result.succeeded())
                    .map(|result| format!("`{}` (exit {:?})", result.route_id, result.exit_code))
                    .collect::<Vec<_>>();
                if !failures.is_empty() {
                    bail!(
                        "verify_equivalent requires every mesh alternative to succeed; failed: {}",
                        failures.join(", ")
                    );
                }
                verify_results_equivalent(&results)?;
                results
            }
            RoutePolicy::BenchmarkAndSelect => {
                let mut results = run_mesh_all_parallel(alternatives, &dispatch_one)?;
                let winner = results
                    .iter()
                    .enumerate()
                    .filter(|(_, result)| result.succeeded())
                    .min_by_key(|(index, result)| (result.duration_ns, *index))
                    .map(|(index, _)| index)
                    .context("benchmark_and_select: no mesh alternative succeeded")?;
                let selected = results.remove(winner);
                results.push(selected);
                results
            }
            RoutePolicy::RaceSuccess => {
                run_mesh_race(alternatives, &dispatch_one, MeshRaceMode::FirstSuccess)?
            }
            RoutePolicy::RaceSettle => {
                run_mesh_race(alternatives, &dispatch_one, MeshRaceMode::FirstSettle)?
            }
            RoutePolicy::BenchmarkValidateAndSelect => unreachable!(
                "validated benchmark selection is finalized before ordinary mesh policies"
            ),
        };
        Ok(RouteSelectionExecution::plain(results))
    })();

    let mut recorded = match Arc::try_unwrap(events) {
        Ok(events) => events
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        Err(events) => events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone()),
    };
    trace.events.append(&mut recorded);
    outcome
}

fn run_mesh_all_parallel<F>(alternatives: &[String], dispatch: &F) -> Result<Vec<OExecutionResult>>
where
    F: Fn(usize, &str, CancellationToken) -> Result<OExecutionResult> + Sync,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for (index, route_id) in alternatives.iter().enumerate() {
            let sender = sender.clone();
            scope.spawn(move || {
                let _ = sender.send((index, dispatch(index, route_id, CancellationToken::new())));
            });
        }
        drop(sender);
        let mut slots = (0..alternatives.len())
            .map(|_| None)
            .collect::<Vec<Option<Result<OExecutionResult>>>>();
        for (index, outcome) in receiver {
            slots[index] = Some(outcome);
        }
        slots
            .into_iter()
            .enumerate()
            .map(|(index, outcome)| {
                outcome
                    .context("mesh alternative worker never reported a result")?
                    .with_context(|| format!("mesh alternative `{}` failed", alternatives[index]))
            })
            .collect()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeshRaceMode {
    FirstSuccess,
    FirstSettle,
}

fn run_mesh_race<F>(
    alternatives: &[String],
    dispatch: &F,
    mode: MeshRaceMode,
) -> Result<Vec<OExecutionResult>>
where
    F: Fn(usize, &str, CancellationToken) -> Result<OExecutionResult> + Sync,
{
    let tokens = alternatives
        .iter()
        .map(|_| CancellationToken::new())
        .collect::<Vec<_>>();
    let (sender, receiver) = std::sync::mpsc::channel();
    // A worker records completion under the same gate the coordinator uses to
    // begin cancellation. This linearizes the declaration-order tie-break:
    // late race losers can never replace the winner merely because they
    // ignored cancellation or failed during reconciliation.
    let cancellation_started = Arc::new(Mutex::new(false));
    std::thread::scope(|scope| {
        for (index, route_id) in alternatives.iter().enumerate() {
            let sender = sender.clone();
            let token = tokens[index].clone();
            let cancellation_started = Arc::clone(&cancellation_started);
            scope.spawn(move || {
                let outcome = dispatch(index, route_id, token);
                let completed_before_cancel = cancellation_started
                    .lock()
                    .map(|started| !*started)
                    .unwrap_or_else(|poisoned| !*poisoned.into_inner());
                let _ = sender.send((index, outcome, completed_before_cancel));
            });
        }
        drop(sender);
        let mut slots = (0..alternatives.len())
            .map(|_| None)
            .collect::<Vec<Option<Result<OExecutionResult>>>>();
        let mut completed_before_cancel = vec![false; alternatives.len()];
        let mut winner = None;
        for (index, outcome, before_cancel) in receiver {
            let qualifies = match (&outcome, mode) {
                (Ok(result), MeshRaceMode::FirstSuccess) => result.succeeded(),
                (Ok(_), MeshRaceMode::FirstSettle) => true,
                (Err(error), MeshRaceMode::FirstSettle) => !is_cancellation_error(error),
                (Err(_), MeshRaceMode::FirstSuccess) => false,
            };
            slots[index] = Some(outcome);
            completed_before_cancel[index] = before_cancel;
            if qualifies && winner.is_none() {
                let begins_cancellation = cancellation_started
                    .lock()
                    .map(|mut started| {
                        if *started {
                            false
                        } else {
                            *started = true;
                            true
                        }
                    })
                    .unwrap_or_else(|poisoned| {
                        let mut started = poisoned.into_inner();
                        if *started {
                            false
                        } else {
                            *started = true;
                            true
                        }
                    });
                if begins_cancellation {
                    winner = Some(index);
                    for (other, token) in tokens.iter().enumerate() {
                        if other != index {
                            token.cancel();
                        }
                    }
                }
            }
        }

        // Preserve the existing deterministic race tie-break: among outcomes
        // that crossed the cancellation boundary, declaration order wins.
        if winner.is_some() {
            for (index, outcome) in slots.iter().enumerate() {
                if !completed_before_cancel[index] {
                    continue;
                }
                let qualifies = match (outcome, mode) {
                    (Some(Ok(result)), MeshRaceMode::FirstSuccess) => result.succeeded(),
                    (Some(Ok(_)), MeshRaceMode::FirstSettle) => true,
                    (Some(Err(error)), MeshRaceMode::FirstSettle) => !is_cancellation_error(error),
                    _ => false,
                };
                if qualifies {
                    winner = Some(index);
                    break;
                }
            }
        }
        let Some(winner) = winner else {
            let mut results = Vec::new();
            for (index, outcome) in slots.into_iter().enumerate() {
                match outcome {
                    Some(Ok(result)) => results.push(result),
                    Some(Err(error)) => {
                        return Err(error.context(format!(
                            "mesh race alternative `{}` failed",
                            alternatives[index]
                        )))
                    }
                    None => {}
                }
            }
            if results.is_empty() {
                bail!("mesh race: no alternative settled");
            }
            return Ok(results);
        };

        let mut results = Vec::new();
        let mut selected = None;
        for (index, outcome) in slots.into_iter().enumerate() {
            match outcome {
                Some(Ok(result)) if index == winner => selected = Some(result),
                Some(Ok(result)) => results.push(result),
                Some(Err(error)) if index == winner => {
                    return Err(error.context(format!(
                        "mesh race selected alternative `{}` failed",
                        alternatives[index]
                    )))
                }
                _ => {}
            }
        }
        let selected = selected.context("mesh race winner produced no route result")?;
        results.push(selected);
        Ok(results)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::OutputCapture;
    use crate::project::{
        ArtifactCaptureStatus, ExecutionProvenance, RouteEffects, RouteExecutionDisposition,
        RouteProvenance, RouteSet,
    };

    fn successful_result(route_id: &str) -> OExecutionResult {
        let stdout = Vec::new();
        let stderr = Vec::new();
        OExecutionResult {
            route_id: route_id.to_owned(),
            exit_code: Some(0),
            stdout_capture: OutputCapture::complete(&stdout),
            stdout,
            stderr_capture: OutputCapture::complete(&stderr),
            stderr,
            value: None,
            artifacts: Vec::new(),
            artifact_requirements: Vec::new(),
            artifact_capture: ArtifactCaptureStatus::Complete,
            disposition: RouteExecutionDisposition::Executed,
            duration_ns: 1,
            provenance: ExecutionProvenance {
                workspace: PathBuf::from("test-workspace"),
                command: vec!["test".to_owned()],
                cwd: PathBuf::from("test-workspace"),
            },
        }
    }

    fn bundle_with_chain() -> ProjectBundle {
        let mut bundle = ProjectBundle::empty("mesh");
        let mut build = RouteSpec::new("build", RouteProvenance::CliOverride);
        build.effects = RouteEffects {
            pure: true,
            unknown: false,
            reads: vec![],
            writes: vec![],
        };
        build.failure_continuation = RouteFailureContinuation::DeclaredIdempotent;
        let mut run = RouteSpec::new("run", RouteProvenance::CliOverride);
        run.prerequisites.push("build".to_string());
        run.failure_continuation = RouteFailureContinuation::DeclaredIdempotent;
        bundle.routes = vec![build, run];
        bundle
    }

    #[test]
    fn target_rank_is_capacity_first_and_deterministic() {
        let mut candidates = [
            MeshTargetCandidateV1 {
                node_id: "slow".into(),
                is_local: false,
                available_slots: 4,
                observed_latency_micros: 20,
            },
            MeshTargetCandidateV1 {
                node_id: "fast".into(),
                is_local: false,
                available_slots: 4,
                observed_latency_micros: 10,
            },
            MeshTargetCandidateV1 {
                node_id: "small".into(),
                is_local: false,
                available_slots: 2,
                observed_latency_micros: 1,
            },
        ];
        candidates.sort_by_key(mesh_target_rank);
        assert_eq!(
            candidates
                .iter()
                .map(|value| value.node_id.as_str())
                .collect::<Vec<_>>(),
            ["fast", "slow", "small"]
        );
    }

    #[test]
    fn same_node_retry_is_not_mislabeled_as_actor_migration() {
        let actor_id = "c".repeat(64);
        let mut trace = MeshExecutionTraceV1 {
            schema: MESH_EXECUTION_TRACE_SCHEMA_V1.to_string(),
            execution_id: "a".repeat(32),
            bundle_sha256: "b".repeat(64),
            target: "route".to_string(),
            policy: "explicit:route".to_string(),
            candidates: vec![MeshTraceCandidateV1 {
                node_id: "only-node".to_string(),
                address: None,
                available_slots: 1,
                observed_latency_micros: 1,
                eligible: true,
                detail: "authenticated candidate".to_string(),
            }],
            events: vec![
                MeshTraceEventV1::Dispatched {
                    route_id: "route".to_string(),
                    actor_id: actor_id.clone(),
                    generation: 1,
                    node_id: "only-node".to_string(),
                },
                MeshTraceEventV1::Settled {
                    route_id: "route".to_string(),
                    actor_id: actor_id.clone(),
                    generation: 1,
                    node_id: "only-node".to_string(),
                    succeeded: false,
                },
                MeshTraceEventV1::Dispatched {
                    route_id: "route".to_string(),
                    actor_id: actor_id.clone(),
                    generation: 2,
                    node_id: "only-node".to_string(),
                },
                MeshTraceEventV1::Settled {
                    route_id: "route".to_string(),
                    actor_id,
                    generation: 2,
                    node_id: "only-node".to_string(),
                    succeeded: true,
                },
            ],
        };
        trace.validate().unwrap();

        trace.events.insert(
            2,
            MeshTraceEventV1::Migrated {
                route_id: "route".to_string(),
                actor_id: "c".repeat(64),
                from_generation: 1,
                to_generation: 2,
                from_node_id: "only-node".to_string(),
                to_node_id: "only-node".to_string(),
                replay_contract: "declared_idempotent".to_string(),
            },
        );
        assert!(trace.validate().is_err());
    }

    #[test]
    fn read_only_discovery_does_not_create_or_enroll_a_registry() {
        let temp = tempfile::tempdir().unwrap();
        let peer_root = temp.path().join("absent-peers");
        let observation = observe_mesh_peers_read_only(&MeshReadOnlyDiscoveryConfig {
            discover_lan: false,
            discovery_timeout: Duration::from_millis(10),
            peer_root: Some(peer_root.clone()),
        })
        .unwrap();
        assert_eq!(observation.schema, MESH_READ_ONLY_DISCOVERY_SCHEMA_V1);
        assert!(!observation.lan_discovery_attempted);
        assert!(observation.peers.is_empty());
        assert!(
            !peer_root.exists(),
            "read-only planning must not create an empty peer registry"
        );
        observation.validate().unwrap();
    }

    #[test]
    fn observed_required_mesh_failure_retains_a_valid_partial_trace() {
        let temp = tempfile::tempdir().unwrap();
        let error = execute_mesh_selection_observed(
            &bundle_with_chain(),
            Some("run"),
            None,
            &RunOptions::default(),
            &MeshExecutionConfig {
                requirement: MeshRequirement::Required,
                discover_lan: false,
                peer_root: Some(temp.path().join("absent-peers")),
                ..MeshExecutionConfig::default()
            },
        )
        .unwrap_err();
        let observed = error
            .downcast_ref::<MeshExecutionError>()
            .expect("post-preflight mesh failure must retain its trace");
        assert!(observed.message().contains("mesh placement is required"));
        assert_eq!(observed.trace.target, "run");
        assert_eq!(observed.trace.policy, "explicit:run");
        assert!(observed.trace.candidates.is_empty());
        observed.trace.validate().unwrap();
    }

    #[test]
    fn validated_selection_survives_mesh_fallback_and_uses_complete_branch_time() {
        let shell = which::which("sh").expect("test host must provide sh");
        let shell = shell.to_string_lossy().into_owned();
        let mut prerequisite = RouteSpec::new("slow-prep", RouteProvenance::CliOverride);
        prerequisite.command = vec![shell.clone(), "-c".to_string(), "sleep 0.80".to_string()];
        let mut reference = RouteSpec::new("reference", RouteProvenance::CliOverride);
        reference.command = vec![shell.clone(), "-c".to_string(), "printf same".to_string()];
        reference.prerequisites = vec!["slow-prep".to_string()];
        let mut candidate = RouteSpec::new("candidate", RouteProvenance::CliOverride);
        candidate.command = vec![
            shell,
            "-c".to_string(),
            "sleep 0.25; printf same".to_string(),
        ];
        let mut bundle = ProjectBundle::empty("mesh-validated-selection");
        bundle.routes = vec![prerequisite, reference, candidate];
        bundle.route_sets = vec![RouteSet {
            provides: "main".to_string(),
            alternatives: vec!["reference".to_string(), "candidate".to_string()],
            policy: RoutePolicy::BenchmarkValidateAndSelect,
        }];
        let peer_root = tempfile::tempdir().unwrap().path().join("no-peers");
        let outcome = execute_mesh_selection_observed(
            &bundle,
            Some("main"),
            None,
            &RunOptions::default(),
            &MeshExecutionConfig {
                discover_lan: false,
                discovery_timeout: Duration::from_millis(1),
                peer_root: Some(peer_root),
                ..MeshExecutionConfig::default()
            },
        )
        .unwrap();

        let receipt = outcome
            .execution
            .validated_selection_receipt
            .expect("mesh validated selection must retain its receipt");
        receipt.validate().unwrap();
        assert_eq!(receipt.selected_route_id, "candidate");
        let reference_result = outcome
            .execution
            .results
            .iter()
            .find(|result| result.route_id == "reference")
            .unwrap();
        let candidate_result = outcome
            .execution
            .results
            .iter()
            .find(|result| result.route_id == "candidate")
            .unwrap();
        assert!(
            reference_result.duration_ns < candidate_result.duration_ns,
            "terminal timing alone should prefer the reference"
        );
        assert!(
            receipt.candidates[0]
                .branch_elapsed_ns
                .parse::<u128>()
                .unwrap()
                > receipt.candidates[1]
                    .branch_elapsed_ns
                    .parse::<u128>()
                    .unwrap(),
            "complete branch timing must include the reference prerequisite"
        );
        assert_eq!(
            outcome
                .trace
                .events
                .iter()
                .filter(|event| matches!(event, MeshTraceEventV1::LocalFallback { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn replay_contract_covers_the_entire_workspace_island() {
        let bundle = bundle_with_chain();
        assert_eq!(
            branch_replay_contract(&bundle, "run").unwrap(),
            BranchReplayContract::DeclaredIdempotent
        );

        let mut unsafe_bundle = bundle;
        unsafe_bundle.route_sets.clear();
        unsafe_bundle.routes[0].failure_continuation = RouteFailureContinuation::Unproven;
        assert_eq!(
            branch_replay_contract(&unsafe_bundle, "run").unwrap(),
            BranchReplayContract::Unproven
        );
    }

    #[test]
    fn actor_identity_is_execution_and_route_bound() {
        let first = actor_id("run-a", &"a".repeat(64), "route");
        let second = actor_id("run-b", &"a".repeat(64), "route");
        let third = actor_id("run-a", &"a".repeat(64), "other");
        assert_ne!(first, second);
        assert_ne!(first, third);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn retry_and_local_fallback_matrix_fails_closed() {
        let replay = BranchReplayContract::DeclaredIdempotent;
        assert!(!may_migrate_after(AttemptDeliveryState::Ambiguous, replay));
        assert!(may_migrate_after(AttemptDeliveryState::Executed, replay));
        assert!(may_fallback_locally(
            MeshRequirement::Prefer,
            MeshLocalFallback::Idempotent,
            AttemptDeliveryState::Executed,
            replay,
        ));
        assert!(!may_migrate_after(
            AttemptDeliveryState::Ambiguous,
            BranchReplayContract::Unproven,
        ));
        assert!(!may_fallback_locally(
            MeshRequirement::Prefer,
            MeshLocalFallback::PreSend,
            AttemptDeliveryState::Ambiguous,
            BranchReplayContract::DeclaredIdempotent,
        ));
        assert!(may_fallback_locally(
            MeshRequirement::Prefer,
            MeshLocalFallback::PreSend,
            AttemptDeliveryState::ProvenNotStarted,
            BranchReplayContract::Unproven,
        ));
        assert!(!may_fallback_locally(
            MeshRequirement::Required,
            MeshLocalFallback::Idempotent,
            AttemptDeliveryState::ProvenNotStarted,
            BranchReplayContract::DeclaredIdempotent,
        ));
        assert_eq!(
            merge_delivery_state(
                AttemptDeliveryState::Executed,
                AttemptDeliveryState::ProvenNotStarted,
            ),
            AttemptDeliveryState::Executed
        );
        assert_eq!(
            merge_delivery_state(
                AttemptDeliveryState::Executed,
                AttemptDeliveryState::Ambiguous,
            ),
            AttemptDeliveryState::Ambiguous
        );
    }

    #[test]
    fn authenticated_pre_admission_rejection_is_a_not_started_proof() {
        assert!(submit_failure_proves_not_started(
            Some(MeshClientFailureDisposition::ServerRejected),
            Some(MeshRejectionStageV1::PreAdmission),
        ));
        assert!(submit_failure_proves_not_started(
            Some(MeshClientFailureDisposition::PreSend),
            None,
        ));
        assert!(!submit_failure_proves_not_started(
            Some(MeshClientFailureDisposition::ServerRejected),
            Some(MeshRejectionStageV1::PostAdmissionOrUnknown),
        ));
        assert!(!submit_failure_proves_not_started(
            Some(MeshClientFailureDisposition::Ambiguous),
            None,
        ));
    }

    #[test]
    fn late_race_loser_cannot_replace_the_linearized_winner() {
        let alternatives = vec!["late".to_owned(), "winner".to_owned()];
        let results = run_mesh_race(
            &alternatives,
            &|index, route_id, cancel| {
                if index == 0 {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !cancel.is_cancelled() {
                        assert!(
                            Instant::now() < deadline,
                            "race winner never cancelled loser"
                        );
                        std::thread::yield_now();
                    }
                }
                Ok(successful_result(route_id))
            },
            MeshRaceMode::FirstSuccess,
        )
        .unwrap();
        assert_eq!(results.last().unwrap().route_id, "winner");

        let results = run_mesh_race(
            &alternatives,
            &|index, route_id, cancel| {
                if index == 0 {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !cancel.is_cancelled() {
                        assert!(
                            Instant::now() < deadline,
                            "race winner never cancelled loser"
                        );
                        std::thread::yield_now();
                    }
                    return Err(anyhow::anyhow!("late non-cancellation failure"));
                }
                Ok(successful_result(route_id))
            },
            MeshRaceMode::FirstSettle,
        )
        .unwrap();
        assert_eq!(results.last().unwrap().route_id, "winner");
    }

    #[test]
    fn target_assignment_spreads_each_wave_before_reusing_capacity() {
        let candidates = vec![
            MeshTargetCandidateV1 {
                node_id: "large".into(),
                is_local: false,
                available_slots: 2,
                observed_latency_micros: 20,
            },
            MeshTargetCandidateV1 {
                node_id: "small".into(),
                is_local: false,
                available_slots: 1,
                observed_latency_micros: 1,
            },
        ];
        assert_eq!(target_assignment(&candidates, 5), [0, 1, 0, 0, 1]);
    }

    #[test]
    fn result_manifest_is_rejected_before_download_when_it_exceeds_profile() {
        let bytes = vec![7_u8; 17];
        let upload = mesh_bundle_artifact(&bytes, 9).unwrap();
        let result = MeshActorResultV1 {
            artifact: upload.artifact,
            chunks: upload.chunks.into_iter().map(|chunk| chunk.id).collect(),
            route_succeeded: true,
            exit_code: Some(0),
        };
        assert!(validate_result_receive_manifest(&result, 16, 9).is_err());
        assert!(validate_result_receive_manifest(&result, 17, 8).is_err());
        validate_result_receive_manifest(&result, 17, 9).unwrap();
    }
}
