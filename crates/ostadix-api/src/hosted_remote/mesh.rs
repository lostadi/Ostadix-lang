//! Versioned peer-mesh data plane for content-addressed project actors.
//!
//! The mesh protocol is intentionally separate from frozen hosted V1 and V2.
//! It moves exact project-bundle bytes, probes an exact route contract, and
//! executes one bundle-bound route actor at a time. The canonical Project
//! Logical HGraph is the IR/contract for these foreign-code actors. Frozen V2
//! remains a separate one-shim-Exec authority protocol and does not itself
//! authorize a mesh actor.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::backend_catalog::BackendRegistry;
use crate::executor::CancellationToken;
use crate::project::model::b64_bytes;
use crate::project::runtime::{
    is_cancellation_error, public_route_execution_diagnostic, run_route_cancellable,
    EnvironmentPolicy, ExecutionLimits, GuardBehavior, ProcessTreePolicy, RunOptions,
};
use crate::project::{
    build_project_hgraph, LogicalHGraphV1, ProjectBundle, RouteGuard, RoutePolicy,
    MAX_LOGICAL_HGRAPH_BYTES,
};

use super::protocol::{
    canonical_hosted_bytes, canonical_hosted_sha256, read_hosted_frame,
    truncate_hosted_error_message, unix_time_ms, write_hosted_frame, MAX_HOSTED_FRAME_BYTES,
    MAX_HOSTED_ID_BYTES,
};
use super::tls::{
    connect_mutual_tls_mesh_v1, ClientTlsIdentity, HostedClientStream, HostedServerStream,
};

pub const HOSTED_MESH_PROTOCOL_V1: &str = "ostadix.mesh-transport/v1";
pub const MESH_NODE_PROFILE_SCHEMA_V1: &str = "ostadix.mesh-node-profile/v1";
pub const MESH_CAPACITY_SCHEMA_V1: &str = "ostadix.mesh-capacity/v1";
pub const MESH_ACTOR_STATUS_SCHEMA_V1: &str = "ostadix.mesh-actor-status/v1";
const MESH_ARTIFACT_MANIFEST_SCHEMA_V1: &str = "ostadix.mesh-artifact-manifest/v1";
const MESH_ACTOR_RECORD_SCHEMA_V1: &str = "ostadix.mesh-actor-record/v1";
const MESH_ACTOR_FENCE_SCHEMA_V1: &str = "ostadix.mesh-actor-fence/v1";

pub const MAX_MESH_CHUNK_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MESH_CHUNK_BYTES: u32 = 512 * 1024;
pub const MAX_MESH_ARTIFACT_CHUNKS: usize = 32_768;

const DEFAULT_MESH_STORAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const DEFAULT_MESH_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_MESH_RESULT_BYTES: u64 = 512 * 1024 * 1024;
const ROUTE_CONTRACT_DIGEST_DOMAIN: &[u8] = b"ostadix.mesh.route-contract/v1\0";
const ACTOR_STORAGE_KEY_DOMAIN: &[u8] = b"ostadix.mesh.actor-storage-key/v1\0";

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshArtifactIdV1 {
    pub sha256: String,
    pub bytes: u64,
}

impl MeshArtifactIdV1 {
    pub fn for_bytes(bytes: &[u8]) -> Self {
        Self {
            sha256: hex::encode(Sha256::digest(bytes)),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        }
    }

    pub fn validate(&self) -> Result<()> {
        validate_sha256("artifact sha256", &self.sha256)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshChunkIdV1 {
    pub sha256: String,
    pub bytes: u32,
}

impl MeshChunkIdV1 {
    pub fn for_bytes(bytes: &[u8]) -> Result<Self> {
        let byte_count = u32::try_from(bytes.len()).context("mesh chunk length exceeds u32")?;
        Ok(Self {
            sha256: hex::encode(Sha256::digest(bytes)),
            bytes: byte_count,
        })
    }

    pub fn validate(&self) -> Result<()> {
        validate_sha256("chunk sha256", &self.sha256)?;
        if self.bytes == 0
            || usize::try_from(self.bytes).unwrap_or(usize::MAX) > MAX_MESH_CHUNK_BYTES
        {
            bail!(
                "chunk length {} must be between 1 and {MAX_MESH_CHUNK_BYTES}",
                self.bytes
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshUploadChunkV1 {
    pub id: MeshChunkIdV1,
    #[serde(with = "b64_bytes")]
    pub bytes: Vec<u8>,
}

impl MeshUploadChunkV1 {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        let id = MeshChunkIdV1::for_bytes(&bytes)?;
        id.validate()?;
        Ok(Self { id, bytes })
    }

    pub fn validate(&self) -> Result<()> {
        self.id.validate()?;
        if usize::try_from(self.id.bytes).ok() != Some(self.bytes.len())
            || self.id.sha256 != hex::encode(Sha256::digest(&self.bytes))
        {
            bail!("mesh upload bytes do not match their content-addressed chunk id");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshArtifactUploadV1 {
    pub artifact: MeshArtifactIdV1,
    pub chunks: Vec<MeshUploadChunkV1>,
}

/// Split exact bundle bytes into independently content-addressed upload chunks.
pub fn mesh_bundle_artifact(bytes: &[u8], chunk_bytes: usize) -> Result<MeshArtifactUploadV1> {
    if chunk_bytes == 0 || chunk_bytes > MAX_MESH_CHUNK_BYTES {
        bail!("mesh chunk size must be between 1 and {MAX_MESH_CHUNK_BYTES} bytes");
    }
    let chunk_count = bytes.len().div_ceil(chunk_bytes);
    if chunk_count > MAX_MESH_ARTIFACT_CHUNKS {
        bail!("mesh artifact requires {chunk_count} chunks; maximum is {MAX_MESH_ARTIFACT_CHUNKS}");
    }
    let chunks = bytes
        .chunks(chunk_bytes)
        .map(|chunk| MeshUploadChunkV1::new(chunk.to_vec()))
        .collect::<Result<Vec<_>>>()?;
    Ok(MeshArtifactUploadV1 {
        artifact: MeshArtifactIdV1::for_bytes(bytes),
        chunks,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshChunkReceiptV1 {
    pub chunk: MeshChunkIdV1,
    pub already_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshArtifactCommitV1 {
    pub artifact: MeshArtifactIdV1,
    pub chunks: Vec<MeshChunkIdV1>,
    pub already_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshNodeProfileV1 {
    pub schema: String,
    pub protocol: String,
    pub node_id: String,
    pub platform_os: String,
    pub architecture: String,
    pub cpu_slots: u32,
    pub memory_capacity_bytes: Option<u64>,
    pub gpu_devices: Vec<String>,
    pub backend_catalog_sha256: String,
    pub catalogued_backends: Vec<String>,
    pub max_parallel: u32,
    pub max_storage_bytes: u64,
    pub max_artifact_bytes: u64,
    pub max_result_bytes: u64,
    pub max_chunk_bytes: u32,
    pub max_result_chunk_bytes: u32,
    pub max_project_ir_bytes: u64,
    pub execution_limit_ceiling: MeshExecutionLimitsV1,
}

impl MeshNodeProfileV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != MESH_NODE_PROFILE_SCHEMA_V1 || self.protocol != HOSTED_MESH_PROTOCOL_V1 {
            bail!("mesh node profile has an unsupported schema or protocol");
        }
        validate_identifier("mesh profile node id", &self.node_id)?;
        validate_identifier("mesh profile platform OS", &self.platform_os)?;
        validate_identifier("mesh profile architecture", &self.architecture)?;
        validate_sha256("mesh backend catalog sha256", &self.backend_catalog_sha256)?;
        if self.cpu_slots == 0
            || self.max_parallel == 0
            || self.max_storage_bytes == 0
            || self.max_artifact_bytes == 0
            || self.max_result_bytes == 0
            || self.max_artifact_bytes > self.max_storage_bytes
            || self.max_result_bytes > self.max_artifact_bytes
            || self.max_chunk_bytes == 0
            || self.max_result_chunk_bytes == 0
            || self.max_project_ir_bytes == 0
            || self.max_project_ir_bytes
                > u64::try_from(MAX_LOGICAL_HGRAPH_BYTES).unwrap_or(u64::MAX)
            || usize::try_from(self.max_chunk_bytes).unwrap_or(usize::MAX) > MAX_MESH_CHUNK_BYTES
            || usize::try_from(self.max_result_chunk_bytes).unwrap_or(usize::MAX)
                > MAX_MESH_CHUNK_BYTES
        {
            bail!("mesh node profile advertises invalid capacity bounds");
        }
        validate_sorted_labels("mesh GPU device", &self.gpu_devices)?;
        validate_sorted_labels("mesh backend", &self.catalogued_backends)?;
        self.execution_limit_ceiling.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshCapacityV1 {
    pub schema: String,
    pub node_id: String,
    /// Unix time in milliseconds.
    pub observed_at: u64,
    pub available_slots: u32,
    pub active_actors: u32,
    pub storage_available_bytes: u64,
}

impl MeshCapacityV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != MESH_CAPACITY_SCHEMA_V1 || self.observed_at == 0 {
            bail!("mesh capacity has an unsupported schema or timestamp");
        }
        validate_identifier("mesh capacity node id", &self.node_id)
    }

    pub fn validate_against(&self, profile: &MeshNodeProfileV1) -> Result<()> {
        self.validate()?;
        profile.validate()?;
        if self.node_id != profile.node_id
            || self
                .available_slots
                .checked_add(self.active_actors)
                .is_none_or(|total| total != profile.max_parallel)
            || self.storage_available_bytes > profile.max_storage_bytes
        {
            bail!("mesh capacity is inconsistent with its node profile");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshRouteRequirementsV1 {
    pub bundle: MeshArtifactIdV1,
    pub route_id: String,
    pub logical_graph_sha256: String,
    pub route_contract_sha256: String,
    pub resources: MeshResourceFootprintV1,
    pub execution_limits: MeshExecutionLimitsV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshResourceFootprintV1 {
    /// Exact mesh actor-admission units. V1 actors are unweighted, so this
    /// must be one until the node implements atomic weighted reservations.
    pub actor_slots: u32,
    pub min_memory_bytes: u64,
    pub min_gpu_devices: u32,
    pub required_backends: Vec<String>,
    pub project_ir_bytes: u64,
    /// Declared bundle payload bytes for CAS preflight. This excludes manifest
    /// overhead, is not TempDir scratch capacity, and is waived on CAS hit;
    /// the upload quota check remains authoritative.
    pub bundle_storage_bytes: u64,
}

impl MeshResourceFootprintV1 {
    pub fn minimal_for_bundle(bundle: &MeshArtifactIdV1) -> Self {
        Self {
            actor_slots: 1,
            min_memory_bytes: 0,
            min_gpu_devices: 0,
            required_backends: Vec::new(),
            project_ir_bytes: 0,
            bundle_storage_bytes: bundle.bytes,
        }
    }

    pub fn validate(&self, bundle: &MeshArtifactIdV1) -> Result<()> {
        if self.actor_slots != 1 || self.bundle_storage_bytes < bundle.bytes {
            bail!("mesh resource footprint has invalid actor-slot/CAS bounds");
        }
        validate_sorted_labels("required mesh backend", &self.required_backends)
    }
}

impl MeshRouteRequirementsV1 {
    pub fn new(
        bundle: MeshArtifactIdV1,
        route_id: impl Into<String>,
        logical_graph_sha256: impl Into<String>,
        route_contract_sha256: impl Into<String>,
    ) -> Self {
        Self {
            resources: MeshResourceFootprintV1::minimal_for_bundle(&bundle),
            execution_limits: MeshExecutionLimitsV1::project_defaults(),
            bundle,
            route_id: route_id.into(),
            logical_graph_sha256: logical_graph_sha256.into(),
            route_contract_sha256: route_contract_sha256.into(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.bundle.validate()?;
        validate_identifier("route id", &self.route_id)?;
        validate_sha256("logical graph sha256", &self.logical_graph_sha256)?;
        validate_sha256("route contract sha256", &self.route_contract_sha256)?;
        self.resources.validate(&self.bundle)?;
        self.execution_limits.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshRouteProbeV1 {
    pub node_id: String,
    pub requirements: MeshRouteRequirementsV1,
    pub eligible: bool,
    pub missing: Vec<String>,
    pub available_slots: u32,
}

impl MeshRouteProbeV1 {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("mesh route-probe node id", &self.node_id)?;
        self.requirements.validate()?;
        if self.eligible != self.missing.is_empty() {
            bail!("mesh route probe eligibility disagrees with missing requirements");
        }
        for missing in &self.missing {
            if missing.is_empty() || missing.len() > 4096 || missing.chars().any(char::is_control) {
                bail!("mesh route probe contains an invalid missing-requirement label");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshActorIdV1 {
    pub actor_id: String,
    pub generation: u64,
}

/// Exact actor coordinate plus the digest of the immutable actor spec bound to
/// that coordinate. Lifecycle operations always carry this reference so a
/// valid response for a different spec cannot be accepted by substitution.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshActorRefV1 {
    pub actor: MeshActorIdV1,
    pub spec_sha256: String,
}

impl MeshActorRefV1 {
    pub fn new(actor: MeshActorIdV1, spec_sha256: impl Into<String>) -> Self {
        Self {
            actor,
            spec_sha256: spec_sha256.into(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.actor.validate()?;
        validate_sha256("mesh actor-reference spec sha256", &self.spec_sha256)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MeshEnvironmentPolicyV1 {
    InheritAll,
    Clear,
    AllowList { names: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeshProcessTreePolicyV1 {
    OwnedProcessGroup,
    LeaderOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeshGuardBehaviorV1 {
    Enforce,
    Skip,
}

/// Serializable, actor-digest-bound projection of project `RunOptions`.
/// `project_defaults()` captures every current default explicitly so a node
/// never executes under ambient client-side limits that were not authorized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshExecutionLimitsV1 {
    pub wall_clock_timeout_ms: u64,
    pub termination_grace_period_ms: u64,
    pub max_retained_stdout_bytes: u64,
    pub max_retained_stderr_bytes: u64,
    pub max_routes_per_selection: u64,
    pub max_selection_retained_output_bytes: u64,
    pub max_artifact_count: u64,
    pub max_artifact_scan_entries: u64,
    pub max_aggregate_artifact_bytes: u64,
    pub max_single_artifact_bytes: u64,
    pub environment_policy: MeshEnvironmentPolicyV1,
    pub process_tree_policy: MeshProcessTreePolicyV1,
    pub guard_behavior: MeshGuardBehaviorV1,
}

impl MeshExecutionLimitsV1 {
    pub fn project_defaults() -> Self {
        Self::from_run_options(&RunOptions::default())
    }

    pub fn from_run_options(options: &RunOptions) -> Self {
        let limits = &options.limits;
        Self {
            wall_clock_timeout_ms: u64::try_from(limits.wall_clock_timeout.as_millis())
                .unwrap_or(u64::MAX),
            termination_grace_period_ms: u64::try_from(limits.termination_grace_period.as_millis())
                .unwrap_or(u64::MAX),
            max_retained_stdout_bytes: u64::try_from(limits.max_retained_stdout_bytes)
                .unwrap_or(u64::MAX),
            max_retained_stderr_bytes: u64::try_from(limits.max_retained_stderr_bytes)
                .unwrap_or(u64::MAX),
            max_routes_per_selection: u64::try_from(limits.max_routes_per_selection)
                .unwrap_or(u64::MAX),
            max_selection_retained_output_bytes: limits.max_selection_retained_output_bytes,
            max_artifact_count: u64::try_from(limits.max_artifact_count).unwrap_or(u64::MAX),
            max_artifact_scan_entries: u64::try_from(limits.max_artifact_scan_entries)
                .unwrap_or(u64::MAX),
            max_aggregate_artifact_bytes: limits.max_aggregate_artifact_bytes,
            max_single_artifact_bytes: limits.max_single_artifact_bytes,
            environment_policy: match &limits.environment_policy {
                EnvironmentPolicy::InheritAll => MeshEnvironmentPolicyV1::InheritAll,
                EnvironmentPolicy::Clear => MeshEnvironmentPolicyV1::Clear,
                EnvironmentPolicy::AllowList(names) => MeshEnvironmentPolicyV1::AllowList {
                    names: names.iter().cloned().collect(),
                },
            },
            process_tree_policy: match limits.process_tree_policy {
                ProcessTreePolicy::OwnedProcessGroup => MeshProcessTreePolicyV1::OwnedProcessGroup,
                ProcessTreePolicy::LeaderOnly => MeshProcessTreePolicyV1::LeaderOnly,
            },
            guard_behavior: match options.guard_behavior {
                GuardBehavior::Enforce => MeshGuardBehaviorV1::Enforce,
                GuardBehavior::Skip => MeshGuardBehaviorV1::Skip,
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.wall_clock_timeout_ms == 0 || self.termination_grace_period_ms == 0 {
            bail!("mesh execution timeouts must be nonzero");
        }
        if self.max_routes_per_selection == 0
            || self.max_artifact_count > self.max_artifact_scan_entries
            || self.max_single_artifact_bytes > self.max_aggregate_artifact_bytes
        {
            bail!("mesh execution limits are internally inconsistent");
        }
        if let MeshEnvironmentPolicyV1::AllowList { names } = &self.environment_policy {
            if names.windows(2).any(|pair| pair[0] >= pair[1]) {
                bail!("mesh environment allow-list must be strictly sorted and unique");
            }
            for name in names {
                validate_identifier("environment allow-list name", name)?;
            }
        }
        #[cfg(not(unix))]
        if self.process_tree_policy == MeshProcessTreePolicyV1::OwnedProcessGroup {
            bail!("owned process-group execution is unsupported on this platform");
        }
        for (field, value) in [
            ("max_retained_stdout_bytes", self.max_retained_stdout_bytes),
            ("max_retained_stderr_bytes", self.max_retained_stderr_bytes),
            ("max_routes_per_selection", self.max_routes_per_selection),
            ("max_artifact_count", self.max_artifact_count),
            ("max_artifact_scan_entries", self.max_artifact_scan_entries),
        ] {
            usize::try_from(value)
                .with_context(|| format!("{field} does not fit this node's usize"))?;
        }
        Ok(())
    }

    pub fn to_run_options(&self) -> Result<RunOptions> {
        self.validate()?;
        let environment_policy = match &self.environment_policy {
            MeshEnvironmentPolicyV1::InheritAll => EnvironmentPolicy::InheritAll,
            MeshEnvironmentPolicyV1::Clear => EnvironmentPolicy::Clear,
            MeshEnvironmentPolicyV1::AllowList { names } => {
                EnvironmentPolicy::AllowList(names.iter().cloned().collect())
            }
        };
        Ok(RunOptions {
            guard_behavior: match self.guard_behavior {
                MeshGuardBehaviorV1::Enforce => GuardBehavior::Enforce,
                MeshGuardBehaviorV1::Skip => GuardBehavior::Skip,
            },
            limits: ExecutionLimits {
                wall_clock_timeout: Duration::from_millis(self.wall_clock_timeout_ms),
                termination_grace_period: Duration::from_millis(self.termination_grace_period_ms),
                max_retained_stdout_bytes: usize::try_from(self.max_retained_stdout_bytes)?,
                max_retained_stderr_bytes: usize::try_from(self.max_retained_stderr_bytes)?,
                max_routes_per_selection: usize::try_from(self.max_routes_per_selection)?,
                max_selection_retained_output_bytes: self.max_selection_retained_output_bytes,
                max_artifact_count: usize::try_from(self.max_artifact_count)?,
                max_artifact_scan_entries: usize::try_from(self.max_artifact_scan_entries)?,
                max_aggregate_artifact_bytes: self.max_aggregate_artifact_bytes,
                max_single_artifact_bytes: self.max_single_artifact_bytes,
                environment_policy,
                process_tree_policy: match self.process_tree_policy {
                    MeshProcessTreePolicyV1::OwnedProcessGroup => {
                        ProcessTreePolicy::OwnedProcessGroup
                    }
                    MeshProcessTreePolicyV1::LeaderOnly => ProcessTreePolicy::LeaderOnly,
                },
            },
        })
    }

    /// Return whether this complete, valid execution policy fits within a
    /// complete, valid node ceiling. Environment inheritance uses set
    /// containment; process-tree and guard behavior must match exactly.
    pub fn fits_within(&self, ceiling: &Self) -> bool {
        self.validate().is_ok()
            && ceiling.validate().is_ok()
            && self.wall_clock_timeout_ms <= ceiling.wall_clock_timeout_ms
            && self.termination_grace_period_ms <= ceiling.termination_grace_period_ms
            && self.max_retained_stdout_bytes <= ceiling.max_retained_stdout_bytes
            && self.max_retained_stderr_bytes <= ceiling.max_retained_stderr_bytes
            && self.max_routes_per_selection <= ceiling.max_routes_per_selection
            && self.max_selection_retained_output_bytes
                <= ceiling.max_selection_retained_output_bytes
            && self.max_artifact_count <= ceiling.max_artifact_count
            && self.max_artifact_scan_entries <= ceiling.max_artifact_scan_entries
            && self.max_aggregate_artifact_bytes <= ceiling.max_aggregate_artifact_bytes
            && self.max_single_artifact_bytes <= ceiling.max_single_artifact_bytes
            && mesh_environment_policy_fits(&self.environment_policy, &ceiling.environment_policy)
            && self.process_tree_policy == ceiling.process_tree_policy
            && self.guard_behavior == ceiling.guard_behavior
    }
}

impl MeshActorIdV1 {
    pub fn new(actor_id: impl Into<String>, generation: u64) -> Self {
        Self {
            actor_id: actor_id.into(),
            generation,
        }
    }

    pub fn validate(&self) -> Result<()> {
        validate_identifier("actor id", &self.actor_id)?;
        if self.generation == 0 {
            bail!("mesh actor generation must be greater than zero");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshActorSpecV1 {
    pub actor: MeshActorIdV1,
    pub target_node_id: String,
    pub bundle: MeshArtifactIdV1,
    pub route_id: String,
    pub logical_graph_sha256: String,
    pub route_contract_sha256: String,
    pub resources: MeshResourceFootprintV1,
    pub execution_limits: MeshExecutionLimitsV1,
}

impl MeshActorSpecV1 {
    pub fn new(
        actor: MeshActorIdV1,
        requirements: MeshRouteRequirementsV1,
        target_node_id: impl Into<String>,
    ) -> Self {
        Self {
            actor,
            target_node_id: target_node_id.into(),
            bundle: requirements.bundle,
            route_id: requirements.route_id,
            logical_graph_sha256: requirements.logical_graph_sha256,
            route_contract_sha256: requirements.route_contract_sha256,
            resources: requirements.resources,
            execution_limits: requirements.execution_limits,
        }
    }

    pub fn requirements(&self) -> MeshRouteRequirementsV1 {
        MeshRouteRequirementsV1 {
            bundle: self.bundle.clone(),
            route_id: self.route_id.clone(),
            logical_graph_sha256: self.logical_graph_sha256.clone(),
            route_contract_sha256: self.route_contract_sha256.clone(),
            resources: self.resources.clone(),
            execution_limits: self.execution_limits.clone(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.actor.validate()?;
        validate_identifier("target node id", &self.target_node_id)?;
        self.requirements().validate()?;
        self.execution_limits.validate()
    }

    pub fn sha256(&self) -> Result<String> {
        self.validate()?;
        canonical_hosted_sha256(self)
    }

    pub fn actor_ref(&self) -> Result<MeshActorRefV1> {
        Ok(MeshActorRefV1::new(self.actor.clone(), self.sha256()?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshActorResultV1 {
    pub artifact: MeshArtifactIdV1,
    pub chunks: Vec<MeshChunkIdV1>,
    pub route_succeeded: bool,
    pub exit_code: Option<i32>,
}

impl MeshActorResultV1 {
    pub fn validate(&self) -> Result<()> {
        validate_artifact_manifest(&self.artifact, &self.chunks)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MeshActorPhaseV1 {
    Running,
    /// A durable admission survived but this process no longer owns its worker.
    /// It is unsafe to replay opaque route effects automatically.
    Indeterminate,
    Succeeded {
        result: MeshActorResultV1,
    },
    Failed {
        code: String,
        message: String,
        retryable: bool,
    },
}

impl MeshActorPhaseV1 {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded { .. } | Self::Failed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshActorStatusV1 {
    pub schema: String,
    pub actor: MeshActorIdV1,
    pub spec_sha256: String,
    pub phase: MeshActorPhaseV1,
    /// Unix time in milliseconds.
    pub updated_at: u64,
}

impl MeshActorStatusV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != MESH_ACTOR_STATUS_SCHEMA_V1 || self.updated_at == 0 {
            bail!("mesh actor status has an unsupported schema or timestamp");
        }
        self.actor.validate()?;
        validate_sha256("mesh actor spec sha256", &self.spec_sha256)?;
        match &self.phase {
            MeshActorPhaseV1::Succeeded { result } => result.validate(),
            MeshActorPhaseV1::Failed { code, message, .. } => {
                validate_identifier("mesh actor failure code", code)?;
                if message.len() > super::protocol::MAX_HOSTED_ERROR_BYTES {
                    bail!("mesh actor failure message exceeds protocol maximum");
                }
                Ok(())
            }
            MeshActorPhaseV1::Running | MeshActorPhaseV1::Indeterminate => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshActorCancellationV1 {
    pub status: MeshActorStatusV1,
    pub cancellation_requested: bool,
}

/// Atomic answer from `FenceActorIfAbsent`. `FencedAbsent` is a durable
/// not-started proof for this node: every later ExecuteActor carrying the same
/// actor id/generation is rejected, including a delayed ambiguous request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeshActorFenceV1 {
    Existing(MeshActorStatusV1),
    FencedAbsent(MeshActorRefV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshResultChunkV1 {
    pub actor: MeshActorIdV1,
    pub spec_sha256: String,
    pub index: u32,
    pub total_chunks: u32,
    pub chunk: MeshChunkIdV1,
    #[serde(with = "b64_bytes")]
    pub bytes: Vec<u8>,
}

impl MeshResultChunkV1 {
    pub fn validate(&self) -> Result<()> {
        self.actor.validate()?;
        validate_sha256("mesh result actor-spec sha256", &self.spec_sha256)?;
        self.chunk.validate()?;
        if self.total_chunks == 0 || self.index >= self.total_chunks {
            bail!("mesh result chunk coordinate is outside its result");
        }
        if usize::try_from(self.chunk.bytes).ok() != Some(self.bytes.len())
            || self.chunk.sha256 != hex::encode(Sha256::digest(&self.bytes))
        {
            bail!("mesh result chunk bytes do not match their content id");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeshRejectionStageV1 {
    /// The node durably promises that this request did not admit/start the
    /// actor coordinate named by the request.
    PreAdmission,
    /// Effects may have begun, an older admission may exist, or the node cannot
    /// prove otherwise. Reconciliation/fencing is required before replay.
    PostAdmissionOrUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(deny_unknown_fields)]
#[error("[{code}] {message}")]
pub struct MeshProtocolErrorV1 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub stage: MeshRejectionStageV1,
}

impl MeshProtocolErrorV1 {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: truncate_hosted_error_message(message.into()),
            retryable,
            stage: MeshRejectionStageV1::PostAdmissionOrUnknown,
        }
    }

    pub fn pre_admission(
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            message: truncate_hosted_error_message(message.into()),
            retryable,
            stage: MeshRejectionStageV1::PreAdmission,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MeshRequestV1 {
    Profile {
        protocol: String,
    },
    Capacity {
        protocol: String,
    },
    ProbeRoute {
        protocol: String,
        requirements: MeshRouteRequirementsV1,
    },
    HasArtifact {
        protocol: String,
        artifact: MeshArtifactIdV1,
    },
    PutChunk {
        protocol: String,
        chunk: MeshUploadChunkV1,
    },
    CommitArtifact {
        protocol: String,
        artifact: MeshArtifactIdV1,
        chunks: Vec<MeshChunkIdV1>,
    },
    ExecuteActor {
        protocol: String,
        actor: MeshActorSpecV1,
    },
    ActorStatus {
        protocol: String,
        actor_ref: MeshActorRefV1,
    },
    CancelActor {
        protocol: String,
        actor_ref: MeshActorRefV1,
    },
    FenceActorIfAbsent {
        protocol: String,
        actor_ref: MeshActorRefV1,
    },
    ResultChunk {
        protocol: String,
        actor_ref: MeshActorRefV1,
        index: u32,
    },
}

impl MeshRequestV1 {
    pub fn validate(&self) -> Result<()> {
        let protocol = match self {
            Self::Profile { protocol }
            | Self::Capacity { protocol }
            | Self::ProbeRoute { protocol, .. }
            | Self::HasArtifact { protocol, .. }
            | Self::PutChunk { protocol, .. }
            | Self::CommitArtifact { protocol, .. }
            | Self::ExecuteActor { protocol, .. }
            | Self::ActorStatus { protocol, .. }
            | Self::CancelActor { protocol, .. }
            | Self::FenceActorIfAbsent { protocol, .. }
            | Self::ResultChunk { protocol, .. } => protocol,
        };
        if protocol != HOSTED_MESH_PROTOCOL_V1 {
            bail!("unsupported mesh protocol `{protocol}`");
        }
        match self {
            Self::Profile { .. } | Self::Capacity { .. } => Ok(()),
            Self::ProbeRoute { requirements, .. } => requirements.validate(),
            Self::HasArtifact { artifact, .. } => artifact.validate(),
            Self::PutChunk { chunk, .. } => chunk.validate(),
            Self::CommitArtifact {
                artifact, chunks, ..
            } => validate_artifact_manifest(artifact, chunks),
            Self::ExecuteActor { actor, .. } => actor.validate(),
            Self::ActorStatus { actor_ref, .. }
            | Self::CancelActor { actor_ref, .. }
            | Self::FenceActorIfAbsent { actor_ref, .. }
            | Self::ResultChunk { actor_ref, .. } => actor_ref.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MeshResponseV1 {
    Profile {
        profile: MeshNodeProfileV1,
    },
    Capacity {
        capacity: MeshCapacityV1,
    },
    RouteProbe {
        probe: MeshRouteProbeV1,
    },
    ArtifactPresence {
        artifact: MeshArtifactIdV1,
        present: bool,
    },
    ChunkStored {
        receipt: MeshChunkReceiptV1,
    },
    ArtifactCommitted {
        commit: MeshArtifactCommitV1,
    },
    ActorStatus {
        status: MeshActorStatusV1,
    },
    ActorCancellation {
        cancellation: MeshActorCancellationV1,
    },
    ActorFence {
        fence: MeshActorFenceV1,
    },
    ResultChunk {
        result: MeshResultChunkV1,
    },
    Error {
        error: MeshProtocolErrorV1,
    },
}

/// Derive the exact, domain-separated route-contract digest shared by clients
/// and nodes. The bundle digest separately binds all files and sibling routes.
pub fn mesh_route_contract_sha256(bundle: &ProjectBundle, route_id: &str) -> Result<String> {
    let route = bundle
        .route(route_id)
        .with_context(|| format!("project bundle has no route `{route_id}`"))?;
    let bytes = serde_json::to_vec(route).context("failed to encode exact route contract")?;
    let length = u64::try_from(bytes.len()).context("route contract length exceeds u64")?;
    let mut hasher = Sha256::new();
    hasher.update(ROUTE_CONTRACT_DIGEST_DOMAIN);
    hasher.update(length.to_le_bytes());
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

/// Derive the canonical logical HGraph digest for one explicit route actor.
pub fn mesh_logical_graph_sha256(bundle: &ProjectBundle, route_id: &str) -> Result<String> {
    Ok(mesh_project_ir_projection(bundle, route_id)?.sha256)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshProjectIrProjectionV1 {
    pub sha256: String,
    pub canonical_bytes_len: u64,
}

/// Build the exact Project Logical HGraph actor IR once and return both its
/// domain-separated digest and canonical JSON byte length. Clients and nodes
/// share this projection so neither digest nor footprint is client-asserted.
pub fn mesh_project_ir_projection(
    bundle: &ProjectBundle,
    route_id: &str,
) -> Result<MeshProjectIrProjectionV1> {
    let project = build_project_hgraph(
        bundle,
        Some(route_id),
        Some(RoutePolicy::Explicit(route_id.to_owned())),
    )
    .map_err(anyhow::Error::msg)?;
    let logical = LogicalHGraphV1::from_project(&project)?;
    let bytes = logical.canonical_bytes()?;
    Ok(MeshProjectIrProjectionV1 {
        sha256: logical.digest()?.as_sha256().to_owned(),
        canonical_bytes_len: u64::try_from(bytes.len())
            .context("canonical Project Logical HGraph length exceeds u64")?,
    })
}

#[derive(Debug, Clone)]
pub struct MeshNodeRuntimeConfig {
    pub node_id: String,
    pub state_dir: PathBuf,
    pub max_storage_bytes: u64,
    pub max_artifact_bytes: u64,
    pub max_result_bytes: u64,
    pub max_chunk_bytes: u32,
    pub max_result_chunk_bytes: u32,
    pub max_concurrent_actors: u32,
    pub memory_capacity_bytes: Option<u64>,
    pub gpu_devices: Vec<String>,
    pub available_backends: Vec<String>,
    pub max_project_ir_bytes: u64,
    pub execution_limit_ceiling: MeshExecutionLimitsV1,
}

impl MeshNodeRuntimeConfig {
    pub fn new(node_id: impl Into<String>, state_dir: impl Into<PathBuf>) -> Self {
        let parallel = std::thread::available_parallelism()
            .ok()
            .and_then(|value| u32::try_from(value.get()).ok())
            .unwrap_or(1)
            .max(1);
        let registry = BackendRegistry::global();
        let mut available_backends = registry
            .canonical_specs()
            .iter()
            .map(|spec| spec.name.to_owned())
            .collect::<Vec<_>>();
        available_backends.sort();
        available_backends.dedup();
        Self {
            node_id: node_id.into(),
            state_dir: state_dir.into(),
            max_storage_bytes: DEFAULT_MESH_STORAGE_BYTES,
            max_artifact_bytes: DEFAULT_MESH_ARTIFACT_BYTES,
            max_result_bytes: DEFAULT_MESH_RESULT_BYTES,
            max_chunk_bytes: DEFAULT_MESH_CHUNK_BYTES,
            max_result_chunk_bytes: DEFAULT_MESH_CHUNK_BYTES,
            max_concurrent_actors: parallel,
            memory_capacity_bytes: None,
            gpu_devices: Vec::new(),
            available_backends,
            max_project_ir_bytes: u64::try_from(MAX_LOGICAL_HGRAPH_BYTES).unwrap_or(u64::MAX),
            execution_limit_ceiling: MeshExecutionLimitsV1::project_defaults(),
        }
    }

    fn validate(&self) -> Result<()> {
        validate_identifier("node id", &self.node_id)?;
        if self.max_storage_bytes == 0
            || self.max_artifact_bytes == 0
            || self.max_result_bytes == 0
            || self.max_concurrent_actors == 0
        {
            bail!("mesh storage, artifact, result, and parallel limits must be nonzero");
        }
        if self.max_artifact_bytes > self.max_storage_bytes
            || self.max_result_bytes > self.max_storage_bytes
            || self.max_result_bytes > self.max_artifact_bytes
        {
            bail!("mesh artifact/result limits exceed their storage hierarchy");
        }
        for (name, value) in [
            ("max_chunk_bytes", self.max_chunk_bytes),
            ("max_result_chunk_bytes", self.max_result_chunk_bytes),
        ] {
            if value == 0 || usize::try_from(value).unwrap_or(usize::MAX) > MAX_MESH_CHUNK_BYTES {
                bail!("{name} must be between 1 and {MAX_MESH_CHUNK_BYTES}");
            }
        }
        if self.max_project_ir_bytes == 0
            || self.max_project_ir_bytes
                > u64::try_from(MAX_LOGICAL_HGRAPH_BYTES).unwrap_or(u64::MAX)
        {
            bail!(
                "mesh Project Logical HGraph IR ceiling must be between 1 and {MAX_LOGICAL_HGRAPH_BYTES} bytes"
            );
        }
        validate_sorted_labels("configured mesh GPU device", &self.gpu_devices)?;
        validate_sorted_labels("configured mesh backend", &self.available_backends)?;
        self.execution_limit_ceiling.validate()?;
        Ok(())
    }
}

#[derive(Debug)]
struct ActiveActorV1 {
    spec_sha256: String,
    cancellation: CancellationToken,
}

#[derive(Debug)]
struct MeshNodeRuntimeInner {
    config: MeshNodeRuntimeConfig,
    #[allow(dead_code)]
    state_lock: File,
    storage_used: Mutex<u64>,
    active_count: AtomicU32,
    active_actors: Mutex<BTreeMap<String, ActiveActorV1>>,
    actor_coordinates: Mutex<()>,
    accepting_actors: AtomicBool,
    drain_gate: Mutex<()>,
    drain_condvar: Condvar,
    worker_failures: Mutex<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct MeshNodeRuntime {
    inner: Arc<MeshNodeRuntimeInner>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MeshArtifactManifestV1 {
    schema: String,
    artifact: MeshArtifactIdV1,
    chunks: Vec<MeshChunkIdV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MeshActorRecordV1 {
    schema: String,
    spec: MeshActorSpecV1,
    status: MeshActorStatusV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MeshActorFenceRecordV1 {
    schema: String,
    actor_ref: MeshActorRefV1,
    fenced_at: u64,
}

pub type MeshNodeResult<T> = std::result::Result<T, MeshProtocolErrorV1>;

impl MeshNodeRuntime {
    pub fn open(config: MeshNodeRuntimeConfig) -> Result<Self> {
        config.validate()?;
        ensure_directory(&config.state_dir)?;
        let state_lock = open_state_lock(&config.state_dir)?;
        for name in ["chunks", "artifacts", "actors", "tmp"] {
            ensure_directory(&config.state_dir.join(name))?;
        }
        let storage_used = directory_bytes(&config.state_dir)?;
        if storage_used > config.max_storage_bytes {
            bail!(
                "mesh state already uses {storage_used} bytes; configured maximum is {}",
                config.max_storage_bytes
            );
        }
        Ok(Self {
            inner: Arc::new(MeshNodeRuntimeInner {
                config,
                state_lock,
                storage_used: Mutex::new(storage_used),
                active_count: AtomicU32::new(0),
                active_actors: Mutex::new(BTreeMap::new()),
                actor_coordinates: Mutex::new(()),
                accepting_actors: AtomicBool::new(true),
                drain_gate: Mutex::new(()),
                drain_condvar: Condvar::new(),
                worker_failures: Mutex::new(Vec::new()),
            }),
        })
    }

    pub fn config(&self) -> &MeshNodeRuntimeConfig {
        &self.inner.config
    }

    pub fn is_accepting_actors(&self) -> bool {
        self.inner.accepting_actors.load(Ordering::Acquire)
    }

    pub fn profile(&self) -> MeshNodeProfileV1 {
        let config = &self.inner.config;
        let registry = BackendRegistry::global();
        MeshNodeProfileV1 {
            schema: MESH_NODE_PROFILE_SCHEMA_V1.to_owned(),
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            node_id: config.node_id.clone(),
            platform_os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            cpu_slots: std::thread::available_parallelism()
                .ok()
                .and_then(|value| u32::try_from(value.get()).ok())
                .unwrap_or(1),
            memory_capacity_bytes: config.memory_capacity_bytes,
            gpu_devices: config.gpu_devices.clone(),
            backend_catalog_sha256: registry.catalog_sha256(),
            catalogued_backends: config.available_backends.clone(),
            max_parallel: config.max_concurrent_actors,
            max_storage_bytes: config.max_storage_bytes,
            max_artifact_bytes: config.max_artifact_bytes,
            max_result_bytes: config.max_result_bytes,
            max_chunk_bytes: config.max_chunk_bytes,
            max_result_chunk_bytes: config.max_result_chunk_bytes,
            max_project_ir_bytes: config.max_project_ir_bytes,
            execution_limit_ceiling: config.execution_limit_ceiling.clone(),
        }
    }

    pub fn capacity(&self) -> MeshNodeResult<MeshCapacityV1> {
        let active = self.inner.active_count.load(Ordering::Acquire);
        let used = *self
            .inner
            .storage_used
            .lock()
            .map_err(|_| mesh_error("internal-lock", "mesh storage lock is poisoned", true))?;
        Ok(MeshCapacityV1 {
            schema: MESH_CAPACITY_SCHEMA_V1.to_owned(),
            node_id: self.inner.config.node_id.clone(),
            observed_at: unix_time_ms()
                .map_err(|error| mesh_error("clock", format!("{error:#}"), true))?,
            available_slots: self
                .inner
                .config
                .max_concurrent_actors
                .saturating_sub(active),
            active_actors: active,
            storage_available_bytes: self.inner.config.max_storage_bytes.saturating_sub(used),
        })
    }

    pub fn has_artifact(&self, artifact: &MeshArtifactIdV1) -> MeshNodeResult<bool> {
        artifact
            .validate()
            .map_err(|error| mesh_error("invalid-artifact", format!("{error:#}"), false))?;
        let path = self.artifact_path(artifact);
        if !path.is_file() {
            return Ok(false);
        }
        Ok(self
            .validate_stored_artifact_manifest(&path, artifact)
            .is_ok())
    }

    pub fn put_chunk(&self, chunk: MeshUploadChunkV1) -> MeshNodeResult<MeshChunkReceiptV1> {
        chunk
            .validate()
            .map_err(|error| mesh_error("invalid-chunk", format!("{error:#}"), false))?;
        if chunk.bytes.len() > usize::try_from(self.inner.config.max_chunk_bytes).unwrap_or(0) {
            return Err(mesh_error(
                "chunk-too-large",
                format!(
                    "chunk is {} bytes; node maximum is {}",
                    chunk.bytes.len(),
                    self.inner.config.max_chunk_bytes
                ),
                false,
            ));
        }
        let path = self.chunk_path(&chunk.id);
        let already_present = self.store_immutable(&path, &chunk.bytes)?;
        if already_present && self.verify_chunk_file(&chunk.id).is_err() {
            self.store_replace(&path, &chunk.bytes)?;
            self.verify_chunk_file(&chunk.id)?;
            return Ok(MeshChunkReceiptV1 {
                chunk: chunk.id,
                already_present: false,
            });
        }
        Ok(MeshChunkReceiptV1 {
            chunk: chunk.id,
            already_present,
        })
    }

    pub fn commit_artifact(
        &self,
        artifact: MeshArtifactIdV1,
        chunks: Vec<MeshChunkIdV1>,
    ) -> MeshNodeResult<MeshArtifactCommitV1> {
        validate_artifact_manifest(&artifact, &chunks)
            .map_err(|error| mesh_error("invalid-artifact", format!("{error:#}"), false))?;
        if artifact.bytes > self.inner.config.max_artifact_bytes {
            return Err(mesh_error(
                "artifact-too-large",
                format!(
                    "artifact is {} bytes; node maximum is {}",
                    artifact.bytes, self.inner.config.max_artifact_bytes
                ),
                false,
            ));
        }
        let path = self.artifact_path(&artifact);
        if path.is_file() {
            if let Ok(existing) = self.validate_stored_artifact_manifest(&path, &artifact) {
                return Ok(MeshArtifactCommitV1 {
                    artifact: existing.artifact,
                    chunks: existing.chunks,
                    already_present: true,
                });
            }
        }
        self.verify_artifact_content(&artifact, &chunks)?;
        let manifest = MeshArtifactManifestV1 {
            schema: MESH_ARTIFACT_MANIFEST_SCHEMA_V1.to_owned(),
            artifact: artifact.clone(),
            chunks: chunks.clone(),
        };
        let bytes = encode_record(&manifest)?;
        if path.exists() {
            // A malformed/truncated/cross-linked manifest is recoverable because
            // the caller supplied an independently verified ordered chunk set.
            self.store_replace(&path, &bytes)?;
        } else {
            self.store_immutable(&path, &bytes)?;
        }
        Ok(MeshArtifactCommitV1 {
            artifact,
            chunks,
            already_present: false,
        })
    }

    pub fn probe_route(
        &self,
        requirements: MeshRouteRequirementsV1,
    ) -> MeshNodeResult<MeshRouteProbeV1> {
        requirements
            .validate()
            .map_err(|error| mesh_error("invalid-requirements", format!("{error:#}"), false))?;
        let mut missing = Vec::new();
        let bundle = match self.load_bundle(&requirements.bundle) {
            Ok(bundle) => Some(bundle),
            Err(error) if error.code == "artifact-not-found" => {
                missing.push(format!("bundle:{}", requirements.bundle.sha256));
                None
            }
            Err(error) => {
                missing.push(format!("bundle-invalid:{}", error.message));
                None
            }
        };
        if let Some(bundle) = &bundle {
            if bundle.route(&requirements.route_id).is_none() {
                missing.push(format!("route:{}", requirements.route_id));
            } else {
                match mesh_route_contract_sha256(bundle, &requirements.route_id) {
                    Ok(actual) if actual == requirements.route_contract_sha256 => {}
                    Ok(_) => missing.push("route-contract-digest".to_owned()),
                    Err(error) => missing.push(format!("route-contract:{error:#}")),
                }
                match mesh_project_ir_projection(bundle, &requirements.route_id) {
                    Ok(actual) => {
                        if actual.sha256 != requirements.logical_graph_sha256 {
                            missing.push("logical-graph-digest".to_owned());
                        }
                        if actual.canonical_bytes_len != requirements.resources.project_ir_bytes {
                            missing.push(format!(
                                "project-ir-bytes-mismatch:{}!={}",
                                requirements.resources.project_ir_bytes, actual.canonical_bytes_len
                            ));
                        }
                        if actual.canonical_bytes_len > self.inner.config.max_project_ir_bytes {
                            missing.push(format!(
                                "project-ir-bytes:{}>{}",
                                actual.canonical_bytes_len, self.inner.config.max_project_ir_bytes
                            ));
                        }
                    }
                    Err(error) => missing.push(format!("logical-graph:{error:#}")),
                }
                collect_missing_runtime_requirements(
                    bundle,
                    &requirements.route_id,
                    &requirements.execution_limits.environment_policy,
                    &mut missing,
                );
            }
        }
        let capacity = self.capacity()?;
        let profile = self.profile();
        if !requirements
            .execution_limits
            .fits_within(&profile.execution_limit_ceiling)
        {
            missing.push("execution-limits-policy".to_owned());
        }
        let resources = &requirements.resources;
        if resources.actor_slots > capacity.available_slots {
            missing.push(format!(
                "available-slots:{}>{}",
                resources.actor_slots, capacity.available_slots
            ));
        }
        if resources.min_memory_bytes > 0 {
            match profile.memory_capacity_bytes {
                Some(available) if available >= resources.min_memory_bytes => {}
                Some(available) => missing.push(format!(
                    "memory-bytes:{}>{available}",
                    resources.min_memory_bytes
                )),
                None => missing.push("memory-capacity-unobserved".to_owned()),
            }
            missing.push("memory-reservation-unsupported".to_owned());
        }
        if resources.min_gpu_devices > 0 {
            if usize::try_from(resources.min_gpu_devices).unwrap_or(usize::MAX)
                > profile.gpu_devices.len()
            {
                missing.push(format!(
                    "gpu-devices:{}>{}",
                    resources.min_gpu_devices,
                    profile.gpu_devices.len()
                ));
            }
            missing.push("gpu-reservation-unsupported".to_owned());
        }
        for backend in &resources.required_backends {
            if profile.catalogued_backends.binary_search(backend).is_err() {
                missing.push(format!("backend:{backend}"));
            }
        }
        if bundle.is_none() && resources.project_ir_bytes > profile.max_project_ir_bytes {
            missing.push(format!(
                "declared-project-ir-bytes:{}>{}",
                resources.project_ir_bytes, profile.max_project_ir_bytes
            ));
        }
        if bundle.is_none() && resources.bundle_storage_bytes > capacity.storage_available_bytes {
            missing.push(format!(
                "bundle-storage-bytes:{}>{}",
                resources.bundle_storage_bytes, capacity.storage_available_bytes
            ));
        }
        missing.sort();
        missing.dedup();
        Ok(MeshRouteProbeV1 {
            node_id: self.inner.config.node_id.clone(),
            requirements,
            eligible: missing.is_empty(),
            missing,
            available_slots: capacity.available_slots,
        })
    }

    pub fn actor_status(&self, actor_ref: &MeshActorRefV1) -> MeshNodeResult<MeshActorStatusV1> {
        actor_ref
            .validate()
            .map_err(|error| mesh_error("invalid-actor-reference", format!("{error:#}"), false))?;
        let key = actor_storage_key(&actor_ref.actor)?;
        let coordinate =
            self.inner.actor_coordinates.lock().map_err(|_| {
                mesh_error("internal-lock", "mesh coordinate lock is poisoned", true)
            })?;
        let record = self.read_actor_record(&self.actor_path(&key), actor_ref)?;
        drop(coordinate);
        let mut status = record.status;
        if matches!(status.phase, MeshActorPhaseV1::Running) {
            let active =
                self.inner.active_actors.lock().map_err(|_| {
                    mesh_error("internal-lock", "mesh actor lock is poisoned", true)
                })?;
            match active.get(&key) {
                Some(actor) if actor.spec_sha256 == actor_ref.spec_sha256 => return Ok(status),
                Some(_) => {
                    return Err(mesh_error(
                        "actor-corrupt",
                        "active actor binding disagrees with durable actor status",
                        false,
                    ));
                }
                None => {}
            }
            drop(active);
            status = self.reconcile_inactive_running_status(actor_ref, &key)?;
        }
        Ok(status)
    }

    fn reconcile_inactive_running_status(
        &self,
        actor_ref: &MeshActorRefV1,
        key: &str,
    ) -> MeshNodeResult<MeshActorStatusV1> {
        // Terminal workers persist under this coordinate lock before removing
        // their active entry. Re-read after observing no active owner so a
        // stale Running read cannot mask a durable terminal.
        let coordinate =
            self.inner.actor_coordinates.lock().map_err(|_| {
                mesh_error("internal-lock", "mesh coordinate lock is poisoned", true)
            })?;
        let mut status = self
            .read_actor_record(&self.actor_path(key), actor_ref)?
            .status;
        drop(coordinate);
        if matches!(status.phase, MeshActorPhaseV1::Running) {
            status.phase = MeshActorPhaseV1::Indeterminate;
        }
        Ok(status)
    }

    pub fn actor_status_optional(
        &self,
        actor_ref: &MeshActorRefV1,
    ) -> MeshNodeResult<Option<MeshActorStatusV1>> {
        actor_ref
            .validate()
            .map_err(|error| mesh_error("invalid-actor-reference", format!("{error:#}"), false))?;
        let key = actor_storage_key(&actor_ref.actor)?;
        let coordinate =
            self.inner.actor_coordinates.lock().map_err(|_| {
                mesh_error("internal-lock", "mesh coordinate lock is poisoned", true)
            })?;
        let exists = self.actor_path(&key).is_file();
        drop(coordinate);
        if !exists {
            return Ok(None);
        }
        self.actor_status(actor_ref).map(Some)
    }

    pub fn execute_actor(&self, spec: MeshActorSpecV1) -> MeshNodeResult<MeshActorStatusV1> {
        spec.validate().map_err(|error| {
            mesh_pre_admission_error("invalid-actor", format!("{error:#}"), false)
        })?;
        if spec.target_node_id != self.inner.config.node_id {
            return Err(mesh_pre_admission_error(
                "wrong-target-node",
                format!(
                    "actor targets node `{}` but this node is `{}`",
                    spec.target_node_id, self.inner.config.node_id
                ),
                false,
            ));
        }
        if !spec
            .execution_limits
            .fits_within(&self.inner.config.execution_limit_ceiling)
        {
            return Err(mesh_pre_admission_error(
                "execution-limits-exceed-node-policy",
                "actor execution limits exceed or differ from this node's declared policy ceiling",
                false,
            ));
        }
        let spec_sha256 = spec.sha256().map_err(|error| {
            mesh_pre_admission_error("invalid-actor", format!("{error:#}"), false)
        })?;
        let actor_ref = MeshActorRefV1::new(spec.actor.clone(), spec_sha256.clone());
        let key = actor_storage_key(&spec.actor)?;
        let path = self.actor_path(&key);
        let probe = self.probe_route(spec.requirements())?;
        if !probe.eligible {
            return Err(mesh_pre_admission_error(
                "route-ineligible",
                format!(
                    "node rejected route requirements: {}",
                    probe.missing.join(", ")
                ),
                false,
            ));
        }
        let coordinate =
            self.inner.actor_coordinates.lock().map_err(|_| {
                mesh_error("internal-lock", "mesh coordinate lock is poisoned", true)
            })?;
        let fence_path = self.actor_fence_path(&key);
        if fence_path.is_file() {
            let fence: MeshActorFenceRecordV1 = self.read_record(&fence_path)?;
            validate_actor_fence_record(&fence).map_err(|error| {
                mesh_error(
                    "actor-fence-corrupt",
                    format!("stored actor fence is internally inconsistent: {error:#}"),
                    false,
                )
            })?;
            if fence.actor_ref != actor_ref {
                return Err(mesh_error(
                    "actor-conflict",
                    "actor coordinate is fenced for a different exact actor spec",
                    false,
                ));
            }
            return Err(mesh_pre_admission_error(
                "actor-fenced",
                "actor id/generation was durably fenced absent and cannot be executed",
                false,
            ));
        }
        if path.is_file() {
            let record = self.read_actor_record(&path, &actor_ref)?;
            if record.spec != spec {
                return Err(mesh_error(
                    "actor-conflict",
                    "actor id/generation is already bound to a different exact actor spec",
                    false,
                ));
            }
            drop(coordinate);
            return self.actor_status(&actor_ref);
        }
        let cancellation = CancellationToken::new();
        self.acquire_actor_slot(&key, &spec_sha256, cancellation.clone())?;
        let active_guard = ActiveActorGuard {
            runtime: self.clone(),
            key: key.clone(),
        };

        let running = MeshActorStatusV1 {
            schema: MESH_ACTOR_STATUS_SCHEMA_V1.to_owned(),
            actor: spec.actor.clone(),
            spec_sha256: spec_sha256.clone(),
            phase: MeshActorPhaseV1::Running,
            updated_at: now_ms_for_node()?,
        };
        let admitted = MeshActorRecordV1 {
            schema: MESH_ACTOR_RECORD_SCHEMA_V1.to_owned(),
            spec: spec.clone(),
            status: running.clone(),
        };
        if let Err(error) = self.store_actor_record(&path, &admitted, false) {
            drop(active_guard);
            return Err(error);
        }
        let worker_runtime = self.clone();
        let worker_spec = spec.clone();
        let worker_path = path.clone();
        let spawn = std::thread::Builder::new()
            .name(format!("ostadix-mesh-actor-{}", &key[..12]))
            .spawn(move || {
                let execution_runtime = worker_runtime.clone();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    execution_runtime.run_actor_worker(worker_spec, worker_path, cancellation)
                }));
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        let message = format!("mesh actor worker failed durably: {error}");
                        eprintln!("o-node: {message}");
                        worker_runtime.record_worker_failure(message);
                    }
                    Err(payload) => {
                        let detail = panic_payload_message(&payload);
                        let message = format!("mesh actor worker panicked: {detail}");
                        eprintln!("o-node: {message}");
                        worker_runtime.record_worker_failure(message);
                    }
                }
                drop(active_guard);
            });
        if let Err(error) = spawn {
            let status = MeshActorStatusV1 {
                schema: MESH_ACTOR_STATUS_SCHEMA_V1.to_owned(),
                actor: spec.actor.clone(),
                spec_sha256,
                phase: MeshActorPhaseV1::Failed {
                    code: "worker-spawn".to_owned(),
                    message: truncate_hosted_error_message(format!(
                        "failed to spawn mesh actor worker: {error}"
                    )),
                    retryable: true,
                },
                updated_at: now_ms_for_node()?,
            };
            let terminal = MeshActorRecordV1 {
                schema: MESH_ACTOR_RECORD_SCHEMA_V1.to_owned(),
                spec,
                status: status.clone(),
            };
            self.store_actor_record(&path, &terminal, true)?;
            drop(coordinate);
            return Ok(status);
        }
        drop(coordinate);
        Ok(running)
    }

    /// Atomically observe an existing actor admission or durably fence an
    /// absent coordinate. Once `FencedAbsent` is returned, a delayed
    /// ExecuteActor for the same id/generation cannot begin on this runtime or
    /// after reopening its state directory.
    pub fn fence_actor_if_absent(
        &self,
        actor_ref: &MeshActorRefV1,
    ) -> MeshNodeResult<MeshActorFenceV1> {
        actor_ref
            .validate()
            .map_err(|error| mesh_error("invalid-actor-reference", format!("{error:#}"), false))?;
        let key = actor_storage_key(&actor_ref.actor)?;
        let coordinate =
            self.inner.actor_coordinates.lock().map_err(|_| {
                mesh_error("internal-lock", "mesh coordinate lock is poisoned", true)
            })?;
        if self.actor_path(&key).is_file() {
            drop(coordinate);
            return self.actor_status(actor_ref).map(MeshActorFenceV1::Existing);
        }
        let path = self.actor_fence_path(&key);
        if path.is_file() {
            let existing: MeshActorFenceRecordV1 = self.read_record(&path)?;
            validate_actor_fence_record(&existing).map_err(|error| {
                mesh_error(
                    "actor-fence-corrupt",
                    format!("stored actor fence is internally inconsistent: {error:#}"),
                    false,
                )
            })?;
            if existing.actor_ref != *actor_ref {
                return Err(mesh_error(
                    "actor-conflict",
                    "actor coordinate is already fenced for a different exact actor spec",
                    false,
                ));
            }
            return Ok(MeshActorFenceV1::FencedAbsent(actor_ref.clone()));
        }
        let record = MeshActorFenceRecordV1 {
            schema: MESH_ACTOR_FENCE_SCHEMA_V1.to_owned(),
            actor_ref: actor_ref.clone(),
            fenced_at: now_ms_for_node()?,
        };
        validate_actor_fence_record(&record).map_err(|error| {
            mesh_error(
                "actor-fence-corrupt",
                format!("refusing to persist an inconsistent actor fence: {error:#}"),
                false,
            )
        })?;
        let bytes = encode_record(&record)?;
        self.store_immutable(&path, &bytes)?;
        Ok(MeshActorFenceV1::FencedAbsent(actor_ref.clone()))
    }

    fn run_actor_worker(
        &self,
        spec: MeshActorSpecV1,
        path: PathBuf,
        cancellation: CancellationToken,
    ) -> MeshNodeResult<()> {
        let spec_sha256 = spec.sha256().map_err(|error| {
            mesh_error(
                "invalid-actor",
                format!("worker spec became invalid: {error:#}"),
                false,
            )
        })?;
        let run_options = spec.execution_limits.to_run_options().map_err(|error| {
            mesh_error(
                "invalid-execution-limits",
                format!("failed to realize bound execution limits: {error:#}"),
                false,
            )
        })?;
        let execution = match self.load_bundle(&spec.bundle) {
            Ok(bundle) => {
                run_route_cancellable(&bundle, &spec.route_id, &run_options, cancellation).map_err(
                    |error| {
                        if is_cancellation_error(&error) {
                            mesh_error(
                                "route-cancelled",
                                public_route_execution_diagnostic(&error),
                                false,
                            )
                        } else {
                            mesh_error(
                                "route-execution",
                                public_route_execution_diagnostic(&error),
                                false,
                            )
                        }
                    },
                )
            }
            Err(error) => Err(error),
        };
        let terminal_phase = match execution {
            Ok(result) => match serde_json::to_vec(&result) {
                Ok(bytes) => match self.store_result_bytes(&bytes) {
                    Ok(commit) => MeshActorPhaseV1::Succeeded {
                        result: MeshActorResultV1 {
                            artifact: commit.artifact,
                            chunks: commit.chunks,
                            route_succeeded: result.succeeded(),
                            exit_code: result.exit_code,
                        },
                    },
                    Err(error) => MeshActorPhaseV1::Failed {
                        code: error.code,
                        message: error.message,
                        retryable: error.retryable,
                    },
                },
                Err(error) => MeshActorPhaseV1::Failed {
                    code: "result-encode".to_owned(),
                    message: truncate_hosted_error_message(format!(
                        "failed to encode route result: {error}"
                    )),
                    retryable: false,
                },
            },
            Err(error) => MeshActorPhaseV1::Failed {
                code: error.code,
                message: error.message,
                retryable: false,
            },
        };
        let status = MeshActorStatusV1 {
            schema: MESH_ACTOR_STATUS_SCHEMA_V1.to_owned(),
            actor: spec.actor.clone(),
            spec_sha256,
            phase: terminal_phase,
            updated_at: now_ms_for_node()?,
        };
        let terminal = MeshActorRecordV1 {
            schema: MESH_ACTOR_RECORD_SCHEMA_V1.to_owned(),
            spec,
            status: status.clone(),
        };
        self.store_terminal_actor_record(&path, &terminal)?;
        Ok(())
    }

    pub fn cancel_actor(
        &self,
        actor_ref: &MeshActorRefV1,
    ) -> MeshNodeResult<MeshActorCancellationV1> {
        actor_ref
            .validate()
            .map_err(|error| mesh_error("invalid-actor-reference", format!("{error:#}"), false))?;
        let key = actor_storage_key(&actor_ref.actor)?;
        let cancellation_requested = {
            let active =
                self.inner.active_actors.lock().map_err(|_| {
                    mesh_error("internal-lock", "mesh actor lock is poisoned", true)
                })?;
            if let Some(actor) = active.get(&key) {
                if actor.spec_sha256 != actor_ref.spec_sha256 {
                    return Err(mesh_error(
                        "actor-conflict",
                        "active actor coordinate carries a different exact actor spec",
                        false,
                    ));
                }
                actor.cancellation.cancel();
                true
            } else {
                false
            }
        };
        let status = self.actor_status(actor_ref)?;
        Ok(MeshActorCancellationV1 {
            status,
            cancellation_requested,
        })
    }

    pub fn result_chunk(
        &self,
        actor_ref: &MeshActorRefV1,
        index: u32,
    ) -> MeshNodeResult<MeshResultChunkV1> {
        let status = self.actor_status(actor_ref)?;
        let MeshActorPhaseV1::Succeeded { result } = status.phase else {
            return Err(mesh_error(
                "result-unavailable",
                "actor has no successful terminal result",
                false,
            ));
        };
        let total_chunks = u32::try_from(result.chunks.len()).map_err(|_| {
            mesh_error(
                "result-corrupt",
                "result chunk count exceeds protocol range",
                false,
            )
        })?;
        let chunk = result
            .chunks
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .cloned()
            .ok_or_else(|| {
                mesh_error(
                    "result-chunk-range",
                    format!("result chunk index {index} is outside 0..{total_chunks}"),
                    false,
                )
            })?;
        let bytes = self.read_chunk_bytes(&chunk)?;
        Ok(MeshResultChunkV1 {
            actor: actor_ref.actor.clone(),
            spec_sha256: actor_ref.spec_sha256.clone(),
            index,
            total_chunks,
            chunk,
            bytes,
        })
    }

    pub fn handle_request(&self, request: MeshRequestV1) -> MeshResponseV1 {
        if let Err(error) = request.validate() {
            return MeshResponseV1::Error {
                error: mesh_pre_admission_error("invalid-request", format!("{error:#}"), false),
            };
        }
        let response = match request {
            MeshRequestV1::Profile { .. } => Ok(MeshResponseV1::Profile {
                profile: self.profile(),
            }),
            MeshRequestV1::Capacity { .. } => self
                .capacity()
                .map(|capacity| MeshResponseV1::Capacity { capacity }),
            MeshRequestV1::ProbeRoute { requirements, .. } => self
                .probe_route(requirements)
                .map(|probe| MeshResponseV1::RouteProbe { probe }),
            MeshRequestV1::HasArtifact { artifact, .. } => self
                .has_artifact(&artifact)
                .map(|present| MeshResponseV1::ArtifactPresence { artifact, present }),
            MeshRequestV1::PutChunk { chunk, .. } => self
                .put_chunk(chunk)
                .map(|receipt| MeshResponseV1::ChunkStored { receipt }),
            MeshRequestV1::CommitArtifact {
                artifact, chunks, ..
            } => self
                .commit_artifact(artifact, chunks)
                .map(|commit| MeshResponseV1::ArtifactCommitted { commit }),
            MeshRequestV1::ExecuteActor { actor, .. } => self
                .execute_actor(actor)
                .map(|status| MeshResponseV1::ActorStatus { status }),
            MeshRequestV1::ActorStatus { actor_ref, .. } => self
                .actor_status(&actor_ref)
                .map(|status| MeshResponseV1::ActorStatus { status }),
            MeshRequestV1::CancelActor { actor_ref, .. } => self
                .cancel_actor(&actor_ref)
                .map(|cancellation| MeshResponseV1::ActorCancellation { cancellation }),
            MeshRequestV1::FenceActorIfAbsent { actor_ref, .. } => self
                .fence_actor_if_absent(&actor_ref)
                .map(|fence| MeshResponseV1::ActorFence { fence }),
            MeshRequestV1::ResultChunk {
                actor_ref, index, ..
            } => self
                .result_chunk(&actor_ref, index)
                .map(|result| MeshResponseV1::ResultChunk { result }),
        };
        response.unwrap_or_else(|error| MeshResponseV1::Error { error })
    }

    /// Stop admitting actor executions, request cooperative cancellation, and
    /// wait until every already-admitted background worker has persisted a
    /// terminal record (or failed). This is a monotonic lifecycle barrier used
    /// by the dual hosted listener.
    pub fn shutdown(&self) -> Result<()> {
        let mut gate = self
            .inner
            .drain_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("mesh drain lock is poisoned"))?;
        self.inner.accepting_actors.store(false, Ordering::Release);
        {
            let active = self
                .inner
                .active_actors
                .lock()
                .map_err(|_| anyhow::anyhow!("mesh actor lock is poisoned"))?;
            for actor in active.values() {
                actor.cancellation.cancel();
            }
        }
        while self.inner.active_count.load(Ordering::Acquire) != 0 {
            gate = self
                .inner
                .drain_condvar
                .wait(gate)
                .map_err(|_| anyhow::anyhow!("mesh drain lock is poisoned"))?;
        }
        drop(gate);
        let mut failures = self
            .inner
            .worker_failures
            .lock()
            .map_err(|_| anyhow::anyhow!("mesh worker-failure lock is poisoned"))?;
        if failures.is_empty() {
            return Ok(());
        }
        failures.sort();
        failures.dedup();
        bail!(
            "mesh shutdown completed with actor worker failures: {}",
            failures.join("; ")
        )
    }

    fn record_worker_failure(&self, message: String) {
        if let Ok(mut failures) = self.inner.worker_failures.lock() {
            failures.push(message);
        }
    }

    fn acquire_actor_slot(
        &self,
        key: &str,
        spec_sha256: &str,
        cancellation: CancellationToken,
    ) -> MeshNodeResult<()> {
        let gate = self
            .inner
            .drain_gate
            .lock()
            .map_err(|_| mesh_error("internal-lock", "mesh drain lock is poisoned", true))?;
        if !self.inner.accepting_actors.load(Ordering::Acquire) {
            return Err(mesh_pre_admission_error(
                "node-shutting-down",
                "node is no longer admitting mesh actors",
                true,
            ));
        }
        let mut active = self
            .inner
            .active_actors
            .lock()
            .map_err(|_| mesh_error("internal-lock", "mesh actor lock is poisoned", true))?;
        if active.len()
            >= usize::try_from(self.inner.config.max_concurrent_actors).unwrap_or(usize::MAX)
        {
            return Err(mesh_pre_admission_error(
                "capacity-exhausted",
                "node has no available mesh actor slots",
                true,
            ));
        }
        if active.contains_key(key) {
            return Err(mesh_error(
                "actor-conflict",
                "actor coordinate is already active without a durable record",
                false,
            ));
        }
        active.insert(
            key.to_owned(),
            ActiveActorV1 {
                spec_sha256: spec_sha256.to_owned(),
                cancellation,
            },
        );
        self.inner.active_count.store(
            u32::try_from(active.len()).unwrap_or(u32::MAX),
            Ordering::Release,
        );
        drop(active);
        drop(gate);
        Ok(())
    }

    fn load_bundle(&self, artifact: &MeshArtifactIdV1) -> MeshNodeResult<ProjectBundle> {
        let bytes = self.read_artifact_bytes(artifact)?;
        crate::project::bundle::deserialize(&bytes)
            .map_err(|error| mesh_error("bundle-invalid", format!("{error:#}"), false))
    }

    fn read_artifact_bytes(&self, artifact: &MeshArtifactIdV1) -> MeshNodeResult<Vec<u8>> {
        artifact
            .validate()
            .map_err(|error| mesh_error("invalid-artifact", format!("{error:#}"), false))?;
        if artifact.bytes > self.inner.config.max_artifact_bytes {
            return Err(mesh_error(
                "artifact-too-large",
                "artifact exceeds this node's in-memory materialization bound",
                false,
            ));
        }
        let path = self.artifact_path(artifact);
        if !path.is_file() {
            return Err(mesh_error(
                "artifact-not-found",
                format!("artifact {} is not committed on this node", artifact.sha256),
                true,
            ));
        }
        let manifest: MeshArtifactManifestV1 = self.read_record(&path)?;
        if manifest.schema != MESH_ARTIFACT_MANIFEST_SCHEMA_V1 || manifest.artifact != *artifact {
            return Err(mesh_error(
                "artifact-corrupt",
                "stored artifact manifest does not match the requested identity",
                false,
            ));
        }
        let capacity = usize::try_from(artifact.bytes).map_err(|_| {
            mesh_error(
                "artifact-too-large",
                "artifact length does not fit this platform",
                false,
            )
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        for chunk in &manifest.chunks {
            bytes.extend_from_slice(&self.read_chunk_bytes(chunk)?);
        }
        if bytes.len() != capacity || hex::encode(Sha256::digest(&bytes)) != artifact.sha256 {
            return Err(mesh_error(
                "artifact-corrupt",
                "materialized artifact bytes do not match the committed identity",
                false,
            ));
        }
        Ok(bytes)
    }

    fn store_result_bytes(&self, bytes: &[u8]) -> MeshNodeResult<MeshArtifactCommitV1> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.inner.config.max_result_bytes {
            return Err(mesh_error(
                "result-too-large",
                format!(
                    "serialized route result is {} bytes; node maximum is {}",
                    bytes.len(),
                    self.inner.config.max_result_bytes
                ),
                false,
            ));
        }
        let upload = mesh_bundle_artifact(
            bytes,
            usize::try_from(self.inner.config.max_result_chunk_bytes).unwrap_or(0),
        )
        .map_err(|error| mesh_error("result-chunking", format!("{error:#}"), false))?;
        for chunk in &upload.chunks {
            self.put_chunk(chunk.clone())?;
        }
        self.commit_artifact(
            upload.artifact,
            upload.chunks.into_iter().map(|chunk| chunk.id).collect(),
        )
    }

    fn verify_artifact_content(
        &self,
        artifact: &MeshArtifactIdV1,
        chunks: &[MeshChunkIdV1],
    ) -> MeshNodeResult<()> {
        let mut hasher = Sha256::new();
        let mut byte_count = 0_u64;
        for chunk in chunks {
            let bytes = self.read_chunk_bytes(chunk)?;
            byte_count = byte_count.checked_add(bytes.len() as u64).ok_or_else(|| {
                mesh_error(
                    "artifact-too-large",
                    "artifact byte count overflowed",
                    false,
                )
            })?;
            hasher.update(bytes);
        }
        if byte_count != artifact.bytes || hex::encode(hasher.finalize()) != artifact.sha256 {
            return Err(mesh_error(
                "artifact-digest-mismatch",
                "ordered chunk content does not match the proposed artifact identity",
                false,
            ));
        }
        Ok(())
    }

    fn validate_stored_artifact_manifest(
        &self,
        path: &Path,
        expected: &MeshArtifactIdV1,
    ) -> MeshNodeResult<MeshArtifactManifestV1> {
        let manifest: MeshArtifactManifestV1 = self.read_record(path).map_err(|error| {
            mesh_error(
                "artifact-corrupt",
                format!(
                    "stored artifact manifest cannot be decoded: {}",
                    error.message
                ),
                false,
            )
        })?;
        if manifest.schema != MESH_ARTIFACT_MANIFEST_SCHEMA_V1 || manifest.artifact != *expected {
            return Err(mesh_error(
                "artifact-corrupt",
                "stored artifact manifest schema or identity is incorrect",
                false,
            ));
        }
        validate_artifact_manifest(&manifest.artifact, &manifest.chunks)
            .map_err(|error| mesh_error("artifact-corrupt", format!("{error:#}"), false))?;
        self.verify_artifact_content(&manifest.artifact, &manifest.chunks)
            .map_err(|error| {
                mesh_error(
                    "artifact-corrupt",
                    format!("stored artifact content is invalid: {}", error.message),
                    false,
                )
            })?;
        Ok(manifest)
    }

    fn verify_chunk_file(&self, chunk: &MeshChunkIdV1) -> MeshNodeResult<()> {
        let _ = self.read_chunk_bytes(chunk)?;
        Ok(())
    }

    fn read_chunk_bytes(&self, chunk: &MeshChunkIdV1) -> MeshNodeResult<Vec<u8>> {
        chunk
            .validate()
            .map_err(|error| mesh_error("invalid-chunk", format!("{error:#}"), false))?;
        let path = self.chunk_path(chunk);
        let bytes = fs::read(&path).map_err(|error| {
            mesh_error(
                "chunk-not-found",
                format!("failed to read chunk {}: {error}", chunk.sha256),
                true,
            )
        })?;
        if usize::try_from(chunk.bytes).ok() != Some(bytes.len())
            || hex::encode(Sha256::digest(&bytes)) != chunk.sha256
        {
            return Err(mesh_error(
                "chunk-corrupt",
                format!("stored chunk {} failed content verification", chunk.sha256),
                false,
            ));
        }
        Ok(bytes)
    }

    fn store_actor_record(
        &self,
        path: &Path,
        record: &MeshActorRecordV1,
        replace: bool,
    ) -> MeshNodeResult<()> {
        validate_actor_record_integrity(record).map_err(|error| {
            mesh_error(
                "actor-corrupt",
                format!("refusing to persist an inconsistent actor record: {error:#}"),
                false,
            )
        })?;
        let bytes = encode_record(record)?;
        if replace {
            self.store_replace(path, &bytes)
        } else {
            self.store_immutable(path, &bytes).and_then(|already| {
                if already {
                    Err(mesh_error(
                        "actor-conflict",
                        "actor id/generation was concurrently admitted",
                        false,
                    ))
                } else {
                    Ok(())
                }
            })
        }
    }

    fn store_terminal_actor_record(
        &self,
        path: &Path,
        record: &MeshActorRecordV1,
    ) -> MeshNodeResult<()> {
        let _coordinate =
            self.inner.actor_coordinates.lock().map_err(|_| {
                mesh_error("internal-lock", "mesh coordinate lock is poisoned", true)
            })?;
        self.store_actor_record(path, record, true)
    }

    fn read_record<T>(&self, path: &Path) -> MeshNodeResult<T>
    where
        T: DeserializeOwned + Serialize,
    {
        let bytes = fs::read(path).map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::NotFound {
                "actor-not-found"
            } else {
                "storage-read"
            };
            mesh_error(
                code,
                format!("failed to read {}: {error}", path.display()),
                false,
            )
        })?;
        decode_record(&bytes)
    }

    fn read_actor_record(
        &self,
        path: &Path,
        expected: &MeshActorRefV1,
    ) -> MeshNodeResult<MeshActorRecordV1> {
        let record: MeshActorRecordV1 = self.read_record(path)?;
        let actual = validate_actor_record_integrity(&record).map_err(|error| {
            mesh_error(
                "actor-corrupt",
                format!("stored actor record is internally inconsistent: {error:#}"),
                false,
            )
        })?;
        if &actual != expected {
            return Err(mesh_error(
                "actor-conflict",
                "actor coordinate is durably bound to a different exact actor spec",
                false,
            ));
        }
        Ok(record)
    }

    fn store_immutable(&self, path: &Path, bytes: &[u8]) -> MeshNodeResult<bool> {
        let mut used = self
            .inner
            .storage_used
            .lock()
            .map_err(|_| mesh_error("internal-lock", "mesh storage lock is poisoned", true))?;
        if path.exists() {
            return Ok(true);
        }
        let byte_count = u64::try_from(bytes.len())
            .map_err(|_| mesh_error("storage-full", "record size exceeds u64", false))?;
        ensure_storage_capacity(&self.inner.config, *used, byte_count)?;
        atomic_write(path, bytes, false)?;
        *used = used.saturating_add(byte_count);
        Ok(false)
    }

    fn store_replace(&self, path: &Path, bytes: &[u8]) -> MeshNodeResult<()> {
        let mut used = self
            .inner
            .storage_used
            .lock()
            .map_err(|_| mesh_error("internal-lock", "mesh storage lock is poisoned", true))?;
        let previous = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let next = u64::try_from(bytes.len())
            .map_err(|_| mesh_error("storage-full", "record size exceeds u64", false))?;
        let base = used.saturating_sub(previous);
        ensure_storage_capacity(&self.inner.config, base, next)?;
        atomic_write(path, bytes, true)?;
        *used = base.saturating_add(next);
        Ok(())
    }

    fn chunk_path(&self, chunk: &MeshChunkIdV1) -> PathBuf {
        self.inner
            .config
            .state_dir
            .join("chunks")
            .join(format!("{}.chunk", chunk.sha256))
    }

    fn artifact_path(&self, artifact: &MeshArtifactIdV1) -> PathBuf {
        self.inner
            .config
            .state_dir
            .join("artifacts")
            .join(format!("{}.manifest", artifact.sha256))
    }

    fn actor_path(&self, key: &str) -> PathBuf {
        self.inner
            .config
            .state_dir
            .join("actors")
            .join(format!("{key}.record"))
    }

    fn actor_fence_path(&self, key: &str) -> PathBuf {
        self.inner
            .config
            .state_dir
            .join("actors")
            .join(format!("{key}.fence"))
    }
}

struct ActiveActorGuard {
    runtime: MeshNodeRuntime,
    key: String,
}

impl Drop for ActiveActorGuard {
    fn drop(&mut self) {
        let inner = &self.runtime.inner;
        if let Ok(_gate) = inner.drain_gate.lock() {
            if let Ok(mut active) = inner.active_actors.lock() {
                active.remove(&self.key);
                inner.active_count.store(
                    u32::try_from(active.len()).unwrap_or(u32::MAX),
                    Ordering::Release,
                );
            } else {
                inner.active_count.fetch_sub(1, Ordering::AcqRel);
            }
        } else {
            inner.active_count.fetch_sub(1, Ordering::AcqRel);
        }
        inner.drain_condvar.notify_all();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshClientFailureDisposition {
    PreSend,
    ServerRejected,
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct MeshClientRequestError {
    disposition: MeshClientFailureDisposition,
    code: Option<String>,
    rejection_stage: Option<MeshRejectionStageV1>,
    message: String,
}

impl MeshClientRequestError {
    pub fn disposition(&self) -> MeshClientFailureDisposition {
        self.disposition
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    pub fn rejection_stage(&self) -> Option<MeshRejectionStageV1> {
        self.rejection_stage
    }
}

pub fn mesh_client_failure_disposition(
    error: &anyhow::Error,
) -> Option<MeshClientFailureDisposition> {
    error
        .downcast_ref::<MeshClientRequestError>()
        .map(MeshClientRequestError::disposition)
}

pub fn mesh_client_rejection_stage(error: &anyhow::Error) -> Option<MeshRejectionStageV1> {
    error
        .downcast_ref::<MeshClientRequestError>()
        .and_then(MeshClientRequestError::rejection_stage)
}

#[derive(Debug, Clone)]
pub struct MeshNodeClient {
    pub address: String,
    pub tls_identity: ClientTlsIdentity,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
}

impl MeshNodeClient {
    pub fn new(
        address: String,
        tls_identity: ClientTlsIdentity,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Self {
        Self {
            address,
            tls_identity,
            connect_timeout,
            io_timeout,
        }
    }

    pub fn profile(&self) -> Result<MeshNodeProfileV1> {
        match self.request(MeshRequestV1::Profile {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
        })? {
            MeshResponseV1::Profile { profile } => Ok(profile),
            _ => unreachable!("response correlation validated profile response"),
        }
    }

    pub fn capacity(&self) -> Result<MeshCapacityV1> {
        match self.request(MeshRequestV1::Capacity {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
        })? {
            MeshResponseV1::Capacity { capacity } => Ok(capacity),
            _ => unreachable!("response correlation validated capacity response"),
        }
    }

    pub fn probe_route(&self, requirements: MeshRouteRequirementsV1) -> Result<MeshRouteProbeV1> {
        match self.request(MeshRequestV1::ProbeRoute {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            requirements,
        })? {
            MeshResponseV1::RouteProbe { probe } => Ok(probe),
            _ => unreachable!("response correlation validated probe response"),
        }
    }

    pub fn has_artifact(&self, artifact: MeshArtifactIdV1) -> Result<bool> {
        match self.request(MeshRequestV1::HasArtifact {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            artifact,
        })? {
            MeshResponseV1::ArtifactPresence { present, .. } => Ok(present),
            _ => unreachable!("response correlation validated artifact response"),
        }
    }

    pub fn put_chunk(&self, chunk: MeshUploadChunkV1) -> Result<MeshChunkReceiptV1> {
        match self.request(MeshRequestV1::PutChunk {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            chunk,
        })? {
            MeshResponseV1::ChunkStored { receipt } => Ok(receipt),
            _ => unreachable!("response correlation validated chunk response"),
        }
    }

    pub fn commit_artifact(
        &self,
        artifact: MeshArtifactIdV1,
        chunks: Vec<MeshChunkIdV1>,
    ) -> Result<MeshArtifactCommitV1> {
        match self.request(MeshRequestV1::CommitArtifact {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            artifact,
            chunks,
        })? {
            MeshResponseV1::ArtifactCommitted { commit } => Ok(commit),
            _ => unreachable!("response correlation validated commit response"),
        }
    }

    pub fn upload_artifact(&self, upload: &MeshArtifactUploadV1) -> Result<MeshArtifactCommitV1> {
        if self.has_artifact(upload.artifact.clone())? {
            return self.commit_artifact(
                upload.artifact.clone(),
                upload.chunks.iter().map(|chunk| chunk.id.clone()).collect(),
            );
        }
        for chunk in &upload.chunks {
            self.put_chunk(chunk.clone())?;
        }
        self.commit_artifact(
            upload.artifact.clone(),
            upload.chunks.iter().map(|chunk| chunk.id.clone()).collect(),
        )
    }

    pub fn execute_actor(&self, actor: MeshActorSpecV1) -> Result<MeshActorStatusV1> {
        match self.request(MeshRequestV1::ExecuteActor {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor,
        })? {
            MeshResponseV1::ActorStatus { status } => Ok(status),
            _ => unreachable!("response correlation validated actor response"),
        }
    }

    pub fn actor_status(&self, actor_ref: MeshActorRefV1) -> Result<MeshActorStatusV1> {
        match self.request(MeshRequestV1::ActorStatus {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
        })? {
            MeshResponseV1::ActorStatus { status } => Ok(status),
            _ => unreachable!("response correlation validated actor-status response"),
        }
    }

    /// Query one exact actor coordinate. An authenticated `actor-not-found`
    /// refusal becomes `None`; transport ambiguity and every other rejection
    /// remain errors.
    pub fn actor_status_optional(
        &self,
        actor_ref: MeshActorRefV1,
    ) -> Result<Option<MeshActorStatusV1>> {
        match self.actor_status(actor_ref) {
            Ok(status) => Ok(Some(status)),
            Err(error)
                if error
                    .downcast_ref::<MeshClientRequestError>()
                    .and_then(MeshClientRequestError::code)
                    == Some("actor-not-found") =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn cancel_actor(&self, actor_ref: MeshActorRefV1) -> Result<MeshActorCancellationV1> {
        match self.request(MeshRequestV1::CancelActor {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
        })? {
            MeshResponseV1::ActorCancellation { cancellation } => Ok(cancellation),
            _ => unreachable!("response correlation validated cancellation response"),
        }
    }

    pub fn fence_actor_if_absent(&self, actor_ref: MeshActorRefV1) -> Result<MeshActorFenceV1> {
        match self.request(MeshRequestV1::FenceActorIfAbsent {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
        })? {
            MeshResponseV1::ActorFence { fence } => Ok(fence),
            _ => unreachable!("response correlation validated actor-fence response"),
        }
    }

    pub fn result_chunk(&self, actor_ref: MeshActorRefV1, index: u32) -> Result<MeshResultChunkV1> {
        match self.request(MeshRequestV1::ResultChunk {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
            index,
        })? {
            MeshResponseV1::ResultChunk { result } => Ok(result),
            _ => unreachable!("response correlation validated result-chunk response"),
        }
    }

    pub fn request(&self, request: MeshRequestV1) -> Result<MeshResponseV1> {
        validate_client_request(&request)?;
        let mut connection = self.connect()?;
        connection.request_validated(request)
    }

    /// Establish one mutually authenticated mesh stream that can carry many
    /// sequential requests. Polling/status/result retrieval should reuse this
    /// connection rather than repeat a TLS handshake for every observation.
    pub fn connect(&self) -> Result<MeshNodeConnection> {
        let stream = connect_mutual_tls_mesh_v1(
            &self.address,
            &self.tls_identity,
            self.connect_timeout,
            self.io_timeout,
        )
        .map_err(|error| {
            client_error(
                MeshClientFailureDisposition::PreSend,
                None,
                format!(
                    "failed to connect to mesh node `{}` before sending request: {error:#}",
                    self.address
                ),
            )
        })?;
        Ok(MeshNodeConnection {
            address: self.address.clone(),
            stream,
        })
    }
}

pub struct MeshNodeConnection {
    address: String,
    stream: HostedClientStream,
}

impl MeshNodeConnection {
    pub fn request(&mut self, request: MeshRequestV1) -> Result<MeshResponseV1> {
        validate_client_request(&request)?;
        self.request_validated(request)
    }

    fn request_validated(&mut self, request: MeshRequestV1) -> Result<MeshResponseV1> {
        write_hosted_frame(&mut self.stream, &request).map_err(|error| {
            client_error(
                MeshClientFailureDisposition::Ambiguous,
                None,
                format!(
                    "mesh request write to `{}` may have been partially delivered: {error:#}",
                    self.address,
                ),
            )
        })?;
        let response: MeshResponseV1 = read_hosted_frame(&mut self.stream)
            .map_err(|error| {
                client_error(
                    MeshClientFailureDisposition::Ambiguous,
                    None,
                    format!(
                        "mesh node `{}` did not return a valid response after delivery: {error:#}",
                        self.address
                    ),
                )
            })?
            .ok_or_else(|| {
                client_error(
                    MeshClientFailureDisposition::Ambiguous,
                    None,
                    format!(
                        "mesh node `{}` closed after delivery without a response",
                        self.address
                    ),
                )
            })?;
        validate_mesh_response(&request, &response).map_err(|error| {
            client_error(
                MeshClientFailureDisposition::Ambiguous,
                None,
                format!("mesh response does not correlate to its request: {error:#}"),
            )
        })?;
        if let MeshResponseV1::Error { error } = response {
            return Err(MeshClientRequestError {
                disposition: MeshClientFailureDisposition::ServerRejected,
                code: Some(error.code.clone()),
                rejection_stage: Some(error.stage),
                message: format!(
                    "mesh node rejected request [{}]{}: {}",
                    error.code,
                    if error.retryable { " (retryable)" } else { "" },
                    error.message
                ),
            }
            .into());
        }
        Ok(response)
    }

    pub fn profile(&mut self) -> Result<MeshNodeProfileV1> {
        match self.request(MeshRequestV1::Profile {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
        })? {
            MeshResponseV1::Profile { profile } => Ok(profile),
            _ => unreachable!("response correlation validated profile response"),
        }
    }

    pub fn probe_route(
        &mut self,
        requirements: MeshRouteRequirementsV1,
    ) -> Result<MeshRouteProbeV1> {
        match self.request(MeshRequestV1::ProbeRoute {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            requirements,
        })? {
            MeshResponseV1::RouteProbe { probe } => Ok(probe),
            _ => unreachable!("response correlation validated probe response"),
        }
    }

    pub fn has_artifact(&mut self, artifact: MeshArtifactIdV1) -> Result<bool> {
        match self.request(MeshRequestV1::HasArtifact {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            artifact,
        })? {
            MeshResponseV1::ArtifactPresence { present, .. } => Ok(present),
            _ => unreachable!("response correlation validated artifact response"),
        }
    }

    pub fn put_chunk(&mut self, chunk: MeshUploadChunkV1) -> Result<MeshChunkReceiptV1> {
        match self.request(MeshRequestV1::PutChunk {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            chunk,
        })? {
            MeshResponseV1::ChunkStored { receipt } => Ok(receipt),
            _ => unreachable!("response correlation validated chunk response"),
        }
    }

    pub fn commit_artifact(
        &mut self,
        artifact: MeshArtifactIdV1,
        chunks: Vec<MeshChunkIdV1>,
    ) -> Result<MeshArtifactCommitV1> {
        match self.request(MeshRequestV1::CommitArtifact {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            artifact,
            chunks,
        })? {
            MeshResponseV1::ArtifactCommitted { commit } => Ok(commit),
            _ => unreachable!("response correlation validated commit response"),
        }
    }

    pub fn upload_artifact(
        &mut self,
        upload: &MeshArtifactUploadV1,
    ) -> Result<MeshArtifactCommitV1> {
        if self.has_artifact(upload.artifact.clone())? {
            return self.commit_artifact(
                upload.artifact.clone(),
                upload.chunks.iter().map(|chunk| chunk.id.clone()).collect(),
            );
        }
        for chunk in &upload.chunks {
            self.put_chunk(chunk.clone())?;
        }
        self.commit_artifact(
            upload.artifact.clone(),
            upload.chunks.iter().map(|chunk| chunk.id.clone()).collect(),
        )
    }

    pub fn execute_actor(&mut self, actor: MeshActorSpecV1) -> Result<MeshActorStatusV1> {
        match self.request(MeshRequestV1::ExecuteActor {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor,
        })? {
            MeshResponseV1::ActorStatus { status } => Ok(status),
            _ => unreachable!("response correlation validated actor response"),
        }
    }

    pub fn capacity(&mut self) -> Result<MeshCapacityV1> {
        match self.request(MeshRequestV1::Capacity {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
        })? {
            MeshResponseV1::Capacity { capacity } => Ok(capacity),
            _ => unreachable!("response correlation validated capacity response"),
        }
    }

    pub fn actor_status(&mut self, actor_ref: MeshActorRefV1) -> Result<MeshActorStatusV1> {
        match self.request(MeshRequestV1::ActorStatus {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
        })? {
            MeshResponseV1::ActorStatus { status } => Ok(status),
            _ => unreachable!("response correlation validated actor-status response"),
        }
    }

    pub fn actor_status_optional(
        &mut self,
        actor_ref: MeshActorRefV1,
    ) -> Result<Option<MeshActorStatusV1>> {
        match self.actor_status(actor_ref) {
            Ok(status) => Ok(Some(status)),
            Err(error)
                if error
                    .downcast_ref::<MeshClientRequestError>()
                    .and_then(MeshClientRequestError::code)
                    == Some("actor-not-found") =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn cancel_actor(&mut self, actor_ref: MeshActorRefV1) -> Result<MeshActorCancellationV1> {
        match self.request(MeshRequestV1::CancelActor {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
        })? {
            MeshResponseV1::ActorCancellation { cancellation } => Ok(cancellation),
            _ => unreachable!("response correlation validated cancellation response"),
        }
    }

    pub fn fence_actor_if_absent(&mut self, actor_ref: MeshActorRefV1) -> Result<MeshActorFenceV1> {
        match self.request(MeshRequestV1::FenceActorIfAbsent {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
        })? {
            MeshResponseV1::ActorFence { fence } => Ok(fence),
            _ => unreachable!("response correlation validated actor-fence response"),
        }
    }

    pub fn result_chunk(
        &mut self,
        actor_ref: MeshActorRefV1,
        index: u32,
    ) -> Result<MeshResultChunkV1> {
        match self.request(MeshRequestV1::ResultChunk {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref,
            index,
        })? {
            MeshResponseV1::ResultChunk { result } => Ok(result),
            _ => unreachable!("response correlation validated result-chunk response"),
        }
    }
}

fn validate_client_request(request: &MeshRequestV1) -> Result<()> {
    request.validate().map_err(|error| {
        client_error(
            MeshClientFailureDisposition::PreSend,
            None,
            format!("invalid mesh request before send: {error:#}"),
        )
    })?;
    canonical_hosted_bytes(request).map_err(|error| {
        client_error(
            MeshClientFailureDisposition::PreSend,
            None,
            format!("mesh request cannot fit a canonical hosted frame: {error:#}"),
        )
    })?;
    Ok(())
}

pub fn serve_mesh_stream(stream: &mut HostedServerStream, runtime: &MeshNodeRuntime) -> Result<()> {
    loop {
        if !runtime.is_accepting_actors() {
            return Ok(());
        }
        let request = match read_hosted_frame::<_, MeshRequestV1>(stream) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => {
                write_hosted_frame(
                    stream,
                    &MeshResponseV1::Error {
                        error: mesh_error("invalid-frame", format!("{error:#}"), false),
                    },
                )?;
                return Ok(());
            }
        };
        let response = runtime.handle_request(request);
        write_hosted_frame(stream, &response).context("failed to write mesh response")?;
    }
}

fn validate_mesh_response(request: &MeshRequestV1, response: &MeshResponseV1) -> Result<()> {
    if let MeshResponseV1::Error { error } = response {
        validate_identifier("mesh error code", &error.code)?;
        if error.message.len() > super::protocol::MAX_HOSTED_ERROR_BYTES {
            bail!("mesh error message exceeds protocol maximum");
        }
        return Ok(());
    }
    match (request, response) {
        (MeshRequestV1::Profile { .. }, MeshResponseV1::Profile { profile }) => profile.validate(),
        (MeshRequestV1::Capacity { .. }, MeshResponseV1::Capacity { capacity }) => {
            capacity.validate()
        }
        (MeshRequestV1::ProbeRoute { requirements, .. }, MeshResponseV1::RouteProbe { probe })
            if probe.requirements == *requirements =>
        {
            probe.validate()
        }
        (
            MeshRequestV1::HasArtifact { artifact, .. },
            MeshResponseV1::ArtifactPresence {
                artifact: response_artifact,
                ..
            },
        ) if response_artifact == artifact => response_artifact.validate(),
        (MeshRequestV1::PutChunk { chunk, .. }, MeshResponseV1::ChunkStored { receipt })
            if receipt.chunk == chunk.id =>
        {
            receipt.chunk.validate()
        }
        (
            MeshRequestV1::CommitArtifact { artifact, .. },
            MeshResponseV1::ArtifactCommitted { commit },
        ) if commit.artifact == *artifact => {
            validate_artifact_manifest(&commit.artifact, &commit.chunks)
        }
        (MeshRequestV1::ExecuteActor { actor, .. }, MeshResponseV1::ActorStatus { status })
            if status.actor == actor.actor && status.spec_sha256 == actor.sha256()? =>
        {
            status.validate()
        }
        (MeshRequestV1::ActorStatus { actor_ref, .. }, MeshResponseV1::ActorStatus { status })
            if status.actor == actor_ref.actor && status.spec_sha256 == actor_ref.spec_sha256 =>
        {
            status.validate()
        }
        (
            MeshRequestV1::CancelActor { actor_ref, .. },
            MeshResponseV1::ActorCancellation { cancellation },
        ) if cancellation.status.actor == actor_ref.actor
            && cancellation.status.spec_sha256 == actor_ref.spec_sha256 =>
        {
            cancellation.status.validate()
        }
        (
            MeshRequestV1::FenceActorIfAbsent { actor_ref, .. },
            MeshResponseV1::ActorFence { fence },
        ) => match fence {
            MeshActorFenceV1::Existing(status)
                if status.actor == actor_ref.actor
                    && status.spec_sha256 == actor_ref.spec_sha256 =>
            {
                status.validate()
            }
            MeshActorFenceV1::FencedAbsent(response_ref) if response_ref == actor_ref => {
                response_ref.validate()
            }
            _ => bail!("mesh actor-fence response names a different actor"),
        },
        (
            MeshRequestV1::ResultChunk {
                actor_ref, index, ..
            },
            MeshResponseV1::ResultChunk { result },
        ) if result.actor == actor_ref.actor
            && result.spec_sha256 == actor_ref.spec_sha256
            && result.index == *index =>
        {
            result.validate()
        }
        _ => bail!("mesh response kind or identity does not match request"),
    }
}

fn client_error(
    disposition: MeshClientFailureDisposition,
    code: Option<String>,
    message: impl Into<String>,
) -> anyhow::Error {
    MeshClientRequestError {
        disposition,
        code,
        rejection_stage: None,
        message: message.into(),
    }
    .into()
}

fn mesh_error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
) -> MeshProtocolErrorV1 {
    MeshProtocolErrorV1::new(code, message, retryable)
}

fn mesh_pre_admission_error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
) -> MeshProtocolErrorV1 {
    MeshProtocolErrorV1::pre_admission(code, message, retryable)
}

fn now_ms_for_node() -> MeshNodeResult<u64> {
    unix_time_ms().map_err(|error| mesh_error("clock", format!("{error:#}"), true))
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_HOSTED_ID_BYTES {
        bail!("{field} length must be between 1 and {MAX_HOSTED_ID_BYTES} bytes");
    }
    if value.chars().any(char::is_control) {
        bail!("{field} contains a control character");
    }
    Ok(())
}

fn validate_sorted_labels(field: &str, values: &[String]) -> Result<()> {
    for value in values {
        validate_identifier(field, value)?;
    }
    if values
        .windows(2)
        .any(|window| window[0].as_str() >= window[1].as_str())
    {
        bail!("{field} values must be strictly sorted and unique");
    }
    Ok(())
}

fn mesh_environment_policy_fits(
    requested: &MeshEnvironmentPolicyV1,
    ceiling: &MeshEnvironmentPolicyV1,
) -> bool {
    match (requested, ceiling) {
        (MeshEnvironmentPolicyV1::Clear, _) => true,
        (MeshEnvironmentPolicyV1::InheritAll, MeshEnvironmentPolicyV1::InheritAll) => true,
        (MeshEnvironmentPolicyV1::AllowList { .. }, MeshEnvironmentPolicyV1::InheritAll) => true,
        (
            MeshEnvironmentPolicyV1::AllowList { names: requested },
            MeshEnvironmentPolicyV1::AllowList { names: ceiling },
        ) => requested
            .iter()
            .all(|name| ceiling.binary_search(name).is_ok()),
        _ => false,
    }
}

fn validate_sha256(field: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{field} must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn validate_artifact_manifest(artifact: &MeshArtifactIdV1, chunks: &[MeshChunkIdV1]) -> Result<()> {
    artifact.validate()?;
    if chunks.len() > MAX_MESH_ARTIFACT_CHUNKS {
        bail!(
            "artifact has {} chunks; maximum is {MAX_MESH_ARTIFACT_CHUNKS}",
            chunks.len()
        );
    }
    let mut total = 0_u64;
    for chunk in chunks {
        chunk.validate()?;
        total = total
            .checked_add(u64::from(chunk.bytes))
            .context("artifact chunk byte count overflowed")?;
    }
    if total != artifact.bytes {
        bail!(
            "artifact declares {} bytes but its chunks declare {total}",
            artifact.bytes
        );
    }
    Ok(())
}

fn actor_storage_key(actor: &MeshActorIdV1) -> MeshNodeResult<String> {
    actor
        .validate()
        .map_err(|error| mesh_error("invalid-actor", format!("{error:#}"), false))?;
    let bytes = canonical_hosted_bytes(actor)
        .map_err(|error| mesh_error("invalid-actor", format!("{error:#}"), false))?;
    let mut hasher = Sha256::new();
    hasher.update(ACTOR_STORAGE_KEY_DOMAIN);
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn validate_actor_record_integrity(record: &MeshActorRecordV1) -> Result<MeshActorRefV1> {
    if record.schema != MESH_ACTOR_RECORD_SCHEMA_V1 {
        bail!("actor record has an unsupported schema");
    }
    record.spec.validate()?;
    record.status.validate()?;
    let actor_ref = record.spec.actor_ref()?;
    if record.status.actor != actor_ref.actor || record.status.spec_sha256 != actor_ref.spec_sha256
    {
        bail!("actor record status does not match its exact stored spec");
    }
    Ok(actor_ref)
}

fn validate_actor_fence_record(record: &MeshActorFenceRecordV1) -> Result<()> {
    if record.schema != MESH_ACTOR_FENCE_SCHEMA_V1 || record.fenced_at == 0 {
        bail!("actor fence has an unsupported schema or timestamp");
    }
    record.actor_ref.validate()
}

fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn collect_missing_runtime_requirements(
    bundle: &ProjectBundle,
    route_id: &str,
    environment_policy: &MeshEnvironmentPolicyV1,
    missing: &mut Vec<String>,
) {
    fn visit(
        bundle: &ProjectBundle,
        route_id: &str,
        environment_policy: &MeshEnvironmentPolicyV1,
        seen: &mut BTreeSet<String>,
        missing: &mut Vec<String>,
    ) {
        if !seen.insert(route_id.to_owned()) {
            return;
        }
        let Some(route) = bundle.route(route_id) else {
            missing.push(format!("route:{route_id}"));
            return;
        };
        if let Some(command) = route.command.first() {
            if command_requirement_missing(command, route, environment_policy) {
                missing.push(format!("command:{command}"));
            }
        } else if route.evaluator.is_none() {
            missing.push(format!("entrypoint:{route_id}"));
        }
        for guard in &route.guards {
            match guard {
                RouteGuard::PlatformOs(expected)
                    if !std::env::consts::OS.eq_ignore_ascii_case(expected) =>
                {
                    missing.push(format!("platform-os:{expected}"));
                }
                RouteGuard::CommandAvailable(command)
                    if command_requirement_missing(command, route, environment_policy) =>
                {
                    missing.push(format!("command:{command}"));
                }
                RouteGuard::EnvVarSet(name)
                    if effective_mesh_environment_value(route, environment_policy, name)
                        .is_none_or(|value| value.is_empty()) =>
                {
                    missing.push(format!("environment:{name}"));
                }
                _ => {}
            }
        }
        for prerequisite in &route.prerequisites {
            visit(bundle, prerequisite, environment_policy, seen, missing);
        }
    }
    visit(
        bundle,
        route_id,
        environment_policy,
        &mut BTreeSet::new(),
        missing,
    );
}

fn command_requirement_missing(
    command: &str,
    route: &crate::project::RouteSpec,
    environment_policy: &MeshEnvironmentPolicyV1,
) -> bool {
    let path = Path::new(command);
    if path.is_absolute() {
        return !path.exists();
    }
    // Bundle-relative commands (for example `./target/app`) may be present only
    // after materialization or a prerequisite build. The route runtime owns
    // that exact workspace check; a host-PATH probe must not reject them.
    if command.contains('/') || command.contains('\\') {
        return false;
    }
    let Some(path) = effective_mesh_environment_value(route, environment_policy, "PATH") else {
        return true;
    };
    !std::env::split_paths(&path).any(|directory| {
        let candidate = directory.join(command);
        candidate.is_file() && mesh_path_is_executable(&candidate)
    })
}

fn effective_mesh_environment_value(
    route: &crate::project::RouteSpec,
    policy: &MeshEnvironmentPolicyV1,
    key: &str,
) -> Option<std::ffi::OsString> {
    if let Some(value) = route.environment.get(key) {
        return Some(value.into());
    }
    match policy {
        MeshEnvironmentPolicyV1::InheritAll => std::env::var_os(key),
        MeshEnvironmentPolicyV1::Clear => None,
        MeshEnvironmentPolicyV1::AllowList { names } if names.iter().any(|name| name == key) => {
            std::env::var_os(key)
        }
        MeshEnvironmentPolicyV1::AllowList { .. } => None,
    }
}

#[cfg(unix)]
fn mesh_path_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn mesh_path_is_executable(path: &Path) -> bool {
    path.is_file()
}

fn encode_record<T: Serialize>(record: &T) -> MeshNodeResult<Vec<u8>> {
    canonical_hosted_bytes(record)
        .map_err(|error| mesh_error("storage-encode", format!("{error:#}"), false))
}

fn decode_record<T>(bytes: &[u8]) -> MeshNodeResult<T>
where
    T: DeserializeOwned + Serialize,
{
    if bytes.len() > MAX_HOSTED_FRAME_BYTES {
        return Err(mesh_error(
            "storage-corrupt",
            "stored mesh record exceeds the canonical frame bound",
            false,
        ));
    }
    let record: T = crate::wire::decode_message(bytes)
        .map_err(|error| mesh_error("storage-corrupt", format!("{error:#}"), false))?;
    let canonical = crate::wire::encode_message(&record)
        .map_err(|error| mesh_error("storage-corrupt", format!("{error:#}"), false))?;
    if canonical != bytes {
        return Err(mesh_error(
            "storage-corrupt",
            "stored mesh record is not canonical Ostadix CBOR",
            false,
        ));
    }
    Ok(record)
}

fn ensure_storage_capacity(
    config: &MeshNodeRuntimeConfig,
    used: u64,
    additional: u64,
) -> MeshNodeResult<()> {
    if used
        .checked_add(additional)
        .is_none_or(|total| total > config.max_storage_bytes)
    {
        return Err(mesh_error(
            "storage-full",
            format!(
                "mesh storage quota {} cannot admit {additional} bytes after {used} bytes",
                config.max_storage_bytes
            ),
            true,
        ));
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            bail!("mesh state path {} must not be a symlink", path.display());
        }
        if !metadata.is_dir() {
            bail!("mesh state path {} is not a directory", path.display());
        }
        return Ok(());
    }
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create mesh state directory {}", path.display()))
}

fn open_state_lock(state_dir: &Path) -> Result<File> {
    let path = state_dir.join("mesh-runtime.lock");
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("mesh state lock {} must be a regular file", path.display());
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("failed to open mesh state lock {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("mesh state lock {} is not a regular file", path.display());
    }
    file.try_lock_exclusive().with_context(|| {
        format!(
            "mesh state root {} is already owned by another runtime",
            state_dir.display()
        )
    })?;
    Ok(file)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)
        .with_context(|| format!("failed to read mesh state directory {}", path.display()))?
    {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!(
                "mesh state contains unsupported symlink {}",
                entry.path().display()
            );
        }
        if metadata.is_dir() {
            total = total
                .checked_add(directory_bytes(&entry.path())?)
                .context("mesh storage usage overflowed u64")?;
        } else if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .context("mesh storage usage overflowed u64")?;
        }
    }
    Ok(total)
}

fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> MeshNodeResult<()> {
    let parent = path.parent().ok_or_else(|| {
        mesh_error(
            "storage-write",
            "mesh record has no parent directory",
            false,
        )
    })?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".mesh-tmp-{}-{sequence}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if !replace && path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "mesh destination already exists",
            ));
        }
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(mesh_error(
            "storage-write",
            format!("failed to persist {}: {error}", path.display()),
            error.kind() == std::io::ErrorKind::Interrupted,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{bundle, ProjectBundle, RouteProvenance, RouteSpec};

    fn fixture_bundle(side_effect: &Path) -> ProjectBundle {
        let mut bundle = ProjectBundle::empty("mesh-fixture");
        let mut route = RouteSpec::new(
            "run",
            RouteProvenance::Manifest {
                path: "test".to_owned(),
            },
        );
        route.command = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!(
                "printf x >> '{}' && printf mesh-result",
                side_effect.display()
            ),
        ];
        route.is_default = true;
        bundle.routes.push(route);
        bundle.default_route = Some("run".to_owned());
        bundle
    }

    fn stage_bundle(
        runtime: &MeshNodeRuntime,
        bundle: &ProjectBundle,
    ) -> (MeshArtifactUploadV1, MeshRouteRequirementsV1) {
        let bytes = bundle::serialize(bundle).unwrap();
        let upload = mesh_bundle_artifact(&bytes, 17).unwrap();
        for chunk in &upload.chunks {
            runtime.put_chunk(chunk.clone()).unwrap();
        }
        runtime
            .commit_artifact(
                upload.artifact.clone(),
                upload.chunks.iter().map(|chunk| chunk.id.clone()).collect(),
            )
            .unwrap();
        let mut requirements = MeshRouteRequirementsV1::new(
            upload.artifact.clone(),
            "run",
            mesh_logical_graph_sha256(bundle, "run").unwrap(),
            mesh_route_contract_sha256(bundle, "run").unwrap(),
        );
        requirements.resources.project_ir_bytes = mesh_project_ir_projection(bundle, "run")
            .unwrap()
            .canonical_bytes_len;
        (upload, requirements)
    }

    fn wait_terminal(runtime: &MeshNodeRuntime, actor_ref: &MeshActorRefV1) -> MeshActorStatusV1 {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let status = runtime.actor_status(actor_ref).unwrap();
            if status.phase.is_terminal() {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "mesh actor did not settle"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn chunked_artifact_commit_is_content_addressed_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let runtime =
            MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", temp.path())).unwrap();
        let upload = mesh_bundle_artifact(b"abcdefghij", 3).unwrap();
        for chunk in &upload.chunks {
            assert!(!runtime.put_chunk(chunk.clone()).unwrap().already_present);
            assert!(runtime.put_chunk(chunk.clone()).unwrap().already_present);
        }
        let chunks = upload
            .chunks
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();
        assert!(
            !runtime
                .commit_artifact(upload.artifact.clone(), chunks.clone())
                .unwrap()
                .already_present
        );
        assert!(
            runtime
                .commit_artifact(upload.artifact.clone(), chunks)
                .unwrap()
                .already_present
        );
        assert!(runtime.has_artifact(&upload.artifact).unwrap());
    }

    #[test]
    fn route_probe_and_actor_execution_recompute_contracts_and_dedupe_restart() {
        let temp = tempfile::tempdir().unwrap();
        let side_effect = temp.path().join("count");
        let state = temp.path().join("state");
        let runtime =
            MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", &state)).unwrap();
        let bundle = fixture_bundle(&side_effect);
        let (_upload, requirements) = stage_bundle(&runtime, &bundle);
        let probe = runtime.probe_route(requirements.clone()).unwrap();
        assert!(probe.eligible, "{:?}", probe.missing);
        let spec =
            MeshActorSpecV1::new(MeshActorIdV1::new("actor-a", 1), requirements, "mesh-node");
        let actor_ref = spec.actor_ref().unwrap();
        let admitted = runtime.execute_actor(spec.clone()).unwrap();
        assert!(matches!(admitted.phase, MeshActorPhaseV1::Running));
        let first = wait_terminal(&runtime, &actor_ref);
        assert!(matches!(first.phase, MeshActorPhaseV1::Succeeded { .. }));
        assert_eq!(fs::read(&side_effect).unwrap(), b"x");

        runtime.shutdown().unwrap();
        drop(runtime);

        let reopened =
            MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", &state)).unwrap();
        let second = reopened.execute_actor(spec.clone()).unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read(&side_effect).unwrap(), b"x");

        let MeshActorPhaseV1::Succeeded { result } = second.phase else {
            unreachable!()
        };
        let mut encoded = Vec::new();
        for index in 0..result.chunks.len() {
            encoded.extend_from_slice(
                &reopened
                    .result_chunk(&actor_ref, u32::try_from(index).unwrap())
                    .unwrap()
                    .bytes,
            );
        }
        let decoded: crate::project::OExecutionResult = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.stdout, b"mesh-result");
    }

    #[test]
    fn durable_absence_fence_rejects_delayed_execution() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let runtime =
            MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", &state)).unwrap();
        let bundle = fixture_bundle(&temp.path().join("must-not-run"));
        let (_upload, requirements) = stage_bundle(&runtime, &bundle);
        let actor = MeshActorIdV1::new("ambiguous-actor", 1);
        let spec = MeshActorSpecV1::new(actor, requirements, "mesh-node");
        let actor_ref = spec.actor_ref().unwrap();
        assert_eq!(
            runtime.fence_actor_if_absent(&actor_ref).unwrap(),
            MeshActorFenceV1::FencedAbsent(actor_ref.clone())
        );
        let error = runtime.execute_actor(spec).unwrap_err();
        assert_eq!(error.code, "actor-fenced");
        assert_eq!(error.stage, MeshRejectionStageV1::PreAdmission);
        runtime.shutdown().unwrap();
        drop(runtime);

        let reopened =
            MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", &state)).unwrap();
        assert_eq!(
            reopened.fence_actor_if_absent(&actor_ref).unwrap(),
            MeshActorFenceV1::FencedAbsent(actor_ref)
        );
    }

    #[test]
    fn route_probe_rejects_substituted_contract_digest() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = MeshNodeRuntime::open(MeshNodeRuntimeConfig::new(
            "mesh-node",
            temp.path().join("state"),
        ))
        .unwrap();
        let bundle = fixture_bundle(&temp.path().join("unused"));
        let (_upload, mut requirements) = stage_bundle(&runtime, &bundle);
        requirements.route_contract_sha256 = "0".repeat(64);
        let probe = runtime.probe_route(requirements).unwrap();
        assert!(!probe.eligible);
        assert!(probe.missing.contains(&"route-contract-digest".to_owned()));
    }

    #[test]
    fn route_probe_recomputes_project_ir_size_and_rejects_underdeclaration() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = fixture_bundle(&temp.path().join("unused"));
        let projection = mesh_project_ir_projection(&bundle, "run").unwrap();
        assert!(projection.canonical_bytes_len > 1);
        let mut config = MeshNodeRuntimeConfig::new("mesh-node", temp.path().join("state"));
        config.max_project_ir_bytes = projection.canonical_bytes_len - 1;
        let runtime = MeshNodeRuntime::open(config).unwrap();
        let (_upload, mut requirements) = stage_bundle(&runtime, &bundle);
        requirements.resources.project_ir_bytes = 1;

        let probe = runtime.probe_route(requirements).unwrap();
        assert!(probe
            .missing
            .iter()
            .any(|missing| missing.starts_with("project-ir-bytes-mismatch:1!=")));
        assert!(probe.missing.contains(&format!(
            "project-ir-bytes:{}>{}",
            projection.canonical_bytes_len,
            projection.canonical_bytes_len - 1
        )));
    }

    #[test]
    fn client_error_disposition_is_recoverable_from_anyhow() {
        let error = client_error(MeshClientFailureDisposition::Ambiguous, None, "injected");
        assert_eq!(
            mesh_client_failure_disposition(&error),
            Some(MeshClientFailureDisposition::Ambiguous)
        );
    }

    #[test]
    fn corrupt_cas_entries_are_detected_and_repaired_from_exact_content() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = MeshNodeRuntime::open(MeshNodeRuntimeConfig::new(
            "mesh-node",
            temp.path().join("state"),
        ))
        .unwrap();
        let upload = mesh_bundle_artifact(b"abcdefghij", 3).unwrap();
        for chunk in &upload.chunks {
            runtime.put_chunk(chunk.clone()).unwrap();
        }
        let chunk_ids = upload
            .chunks
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();
        runtime
            .commit_artifact(upload.artifact.clone(), chunk_ids.clone())
            .unwrap();

        let manifest_path = runtime.artifact_path(&upload.artifact);
        let manifest_len = fs::metadata(&manifest_path).unwrap().len();
        fs::write(&manifest_path, vec![0_u8; manifest_len as usize]).unwrap();
        assert!(!runtime.has_artifact(&upload.artifact).unwrap());
        assert!(
            !runtime
                .commit_artifact(upload.artifact.clone(), chunk_ids.clone())
                .unwrap()
                .already_present
        );
        assert!(runtime.has_artifact(&upload.artifact).unwrap());

        let first = &upload.chunks[0];
        let chunk_path = runtime.chunk_path(&first.id);
        fs::write(&chunk_path, vec![b'z'; first.bytes.len()]).unwrap();
        assert!(!runtime.has_artifact(&upload.artifact).unwrap());
        assert!(!runtime.put_chunk(first.clone()).unwrap().already_present);
        assert!(runtime.has_artifact(&upload.artifact).unwrap());
    }

    #[test]
    fn probe_fails_closed_for_declared_resource_requirements() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = MeshNodeRuntimeConfig::new("mesh-node", temp.path().join("state"));
        config.memory_capacity_bytes = None;
        config.gpu_devices.clear();
        config.max_project_ir_bytes = 128;
        let storage_ceiling = config.max_storage_bytes;
        let runtime = MeshNodeRuntime::open(config).unwrap();
        let bundle = fixture_bundle(&temp.path().join("unused"));
        let (_upload, mut requirements) = stage_bundle(&runtime, &bundle);
        requirements.resources.min_memory_bytes = 1;
        requirements.resources.min_gpu_devices = 1;
        requirements.resources.required_backends = vec!["unavailable-backend".to_owned()];
        requirements.resources.project_ir_bytes = 129;

        let probe = runtime.probe_route(requirements).unwrap();
        assert!(!probe.eligible);
        assert!(probe
            .missing
            .contains(&"memory-capacity-unobserved".to_owned()));
        assert!(probe
            .missing
            .contains(&"memory-reservation-unsupported".to_owned()));
        assert!(probe
            .missing
            .iter()
            .any(|value| value.starts_with("gpu-devices:")));
        assert!(probe
            .missing
            .contains(&"gpu-reservation-unsupported".to_owned()));
        assert!(probe
            .missing
            .contains(&"backend:unavailable-backend".to_owned()));
        assert!(probe
            .missing
            .iter()
            .any(|value| value.starts_with("project-ir-bytes:")));
        let mut absent = MeshRouteRequirementsV1::new(
            MeshArtifactIdV1::for_bytes(b"absent"),
            "run",
            "0".repeat(64),
            "1".repeat(64),
        );
        absent.resources.bundle_storage_bytes = storage_ceiling.saturating_add(1);
        let absent_probe = runtime.probe_route(absent).unwrap();
        assert!(absent_probe
            .missing
            .iter()
            .any(|value| value.starts_with("bundle-storage-bytes:")));
    }

    #[test]
    fn actor_limits_are_bound_to_spec_and_checked_before_admission() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = MeshNodeRuntime::open(MeshNodeRuntimeConfig::new(
            "mesh-node",
            temp.path().join("state"),
        ))
        .unwrap();
        let bundle = fixture_bundle(&temp.path().join("must-not-run"));
        let (_upload, requirements) = stage_bundle(&runtime, &bundle);
        let mut spec = MeshActorSpecV1::new(
            MeshActorIdV1::new("limited-actor", 1),
            requirements,
            "mesh-node",
        );
        spec.execution_limits.wall_clock_timeout_ms = spec
            .execution_limits
            .wall_clock_timeout_ms
            .saturating_add(1);
        let error = runtime.execute_actor(spec).unwrap_err();
        assert_eq!(error.code, "execution-limits-exceed-node-policy");
        assert_eq!(error.stage, MeshRejectionStageV1::PreAdmission);
    }

    #[test]
    fn runtime_config_enforces_result_hierarchy_and_exclusive_state_owner() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let mut invalid = MeshNodeRuntimeConfig::new("mesh-node", &state);
        invalid.max_storage_bytes = 20;
        invalid.max_artifact_bytes = 10;
        invalid.max_result_bytes = 11;
        assert!(MeshNodeRuntime::open(invalid).is_err());

        let runtime =
            MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", &state)).unwrap();
        assert!(MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", &state)).is_err());
        runtime.shutdown().unwrap();
        drop(runtime);
        let reopened = MeshNodeRuntime::open(MeshNodeRuntimeConfig::new("mesh-node", &state));
        assert!(reopened.is_ok());
    }

    #[test]
    fn lifecycle_reads_reject_spec_substitution_and_corrupt_durable_status() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = MeshNodeRuntime::open(MeshNodeRuntimeConfig::new(
            "mesh-node",
            temp.path().join("state"),
        ))
        .unwrap();
        let bundle = fixture_bundle(&temp.path().join("count"));
        let (_upload, requirements) = stage_bundle(&runtime, &bundle);
        let spec = MeshActorSpecV1::new(
            MeshActorIdV1::new("bound-actor", 1),
            requirements,
            "mesh-node",
        );
        let actor_ref = spec.actor_ref().unwrap();
        runtime.execute_actor(spec).unwrap();
        let terminal = wait_terminal(&runtime, &actor_ref);

        // Model the race where the first status read observed Running but the
        // worker persisted terminal and removed its active entry before the
        // active-map observation. The reconciliation read must return terminal,
        // never synthesize Indeterminate over it.
        let key = actor_storage_key(&actor_ref.actor).unwrap();
        let reconciled = runtime
            .reconcile_inactive_running_status(&actor_ref, &key)
            .unwrap();
        assert_eq!(reconciled, terminal);

        let wrong_ref = MeshActorRefV1::new(actor_ref.actor.clone(), "0".repeat(64));
        let conflict = runtime.actor_status(&wrong_ref).unwrap_err();
        assert_eq!(conflict.code, "actor-conflict");

        let path = runtime.actor_path(&key);
        let mut record: MeshActorRecordV1 = runtime.read_record(&path).unwrap();
        record.status.spec_sha256 = "0".repeat(64);
        let corrupted = encode_record(&record).unwrap();
        runtime.store_replace(&path, &corrupted).unwrap();
        let error = runtime.actor_status(&actor_ref).unwrap_err();
        assert_eq!(error.code, "actor-corrupt");
    }

    #[test]
    fn client_response_validation_binds_every_actor_lifecycle_reply() {
        let actor_ref = MeshActorRefV1::new(MeshActorIdV1::new("actor", 1), "1".repeat(64));
        let substituted = MeshActorStatusV1 {
            schema: MESH_ACTOR_STATUS_SCHEMA_V1.to_owned(),
            actor: actor_ref.actor.clone(),
            spec_sha256: "2".repeat(64),
            phase: MeshActorPhaseV1::Running,
            updated_at: 1,
        };
        let status_request = MeshRequestV1::ActorStatus {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref: actor_ref.clone(),
        };
        assert!(validate_mesh_response(
            &status_request,
            &MeshResponseV1::ActorStatus {
                status: substituted.clone(),
            },
        )
        .is_err());
        let fence_request = MeshRequestV1::FenceActorIfAbsent {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref: actor_ref.clone(),
        };
        assert!(validate_mesh_response(
            &fence_request,
            &MeshResponseV1::ActorFence {
                fence: MeshActorFenceV1::FencedAbsent(MeshActorRefV1::new(
                    actor_ref.actor.clone(),
                    "2".repeat(64),
                )),
            },
        )
        .is_err());
        let cancel_request = MeshRequestV1::CancelActor {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref: actor_ref.clone(),
        };
        assert!(validate_mesh_response(
            &cancel_request,
            &MeshResponseV1::ActorCancellation {
                cancellation: MeshActorCancellationV1 {
                    status: substituted,
                    cancellation_requested: true,
                },
            },
        )
        .is_err());
        let result_request = MeshRequestV1::ResultChunk {
            protocol: HOSTED_MESH_PROTOCOL_V1.to_owned(),
            actor_ref: actor_ref.clone(),
            index: 0,
        };
        assert!(validate_mesh_response(
            &result_request,
            &MeshResponseV1::ResultChunk {
                result: MeshResultChunkV1 {
                    actor: actor_ref.actor,
                    spec_sha256: "2".repeat(64),
                    index: 0,
                    total_chunks: 1,
                    chunk: MeshChunkIdV1::for_bytes(b"x").unwrap(),
                    bytes: b"x".to_vec(),
                },
            },
        )
        .is_err());
    }

    #[test]
    fn cancellation_and_shutdown_persist_route_cancelled_terminal_status() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = MeshNodeRuntime::open(MeshNodeRuntimeConfig::new(
            "mesh-node",
            temp.path().join("state"),
        ))
        .unwrap();
        let mut bundle = ProjectBundle::empty("slow-mesh-fixture");
        let mut route = RouteSpec::new(
            "run",
            RouteProvenance::Manifest {
                path: "test".to_owned(),
            },
        );
        route.command = vec!["sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()];
        route.is_default = true;
        bundle.routes.push(route);
        bundle.default_route = Some("run".to_owned());
        let (_upload, mut requirements) = stage_bundle(&runtime, &bundle);
        requirements.execution_limits.termination_grace_period_ms = 25;

        let first_spec = MeshActorSpecV1::new(
            MeshActorIdV1::new("cancel-actor", 1),
            requirements.clone(),
            "mesh-node",
        );
        let first_ref = first_spec.actor_ref().unwrap();
        runtime.execute_actor(first_spec).unwrap();
        let wrong_ref = MeshActorRefV1::new(first_ref.actor.clone(), "0".repeat(64));
        assert_eq!(
            runtime.cancel_actor(&wrong_ref).unwrap_err().code,
            "actor-conflict"
        );
        let cancellation = runtime.cancel_actor(&first_ref).unwrap();
        assert!(cancellation.cancellation_requested);
        assert_eq!(cancellation.status.spec_sha256, first_ref.spec_sha256);
        let first_terminal = wait_terminal(&runtime, &first_ref);
        assert!(matches!(
            first_terminal.phase,
            MeshActorPhaseV1::Failed { ref code, .. } if code == "route-cancelled"
        ));

        let second_spec = MeshActorSpecV1::new(
            MeshActorIdV1::new("shutdown-actor", 1),
            requirements,
            "mesh-node",
        );
        let second_ref = second_spec.actor_ref().unwrap();
        runtime.execute_actor(second_spec).unwrap();
        runtime.shutdown().unwrap();
        let second_terminal = runtime.actor_status(&second_ref).unwrap();
        assert!(matches!(
            second_terminal.phase,
            MeshActorPhaseV1::Failed { ref code, .. } if code == "route-cancelled"
        ));
    }

    #[test]
    fn concurrent_distinct_admissions_cannot_oversubscribe_one_actor_slot() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = MeshNodeRuntimeConfig::new("mesh-node", temp.path().join("state"));
        config.max_concurrent_actors = 1;
        let runtime = MeshNodeRuntime::open(config).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for key in ["concurrent-a", "concurrent-b"] {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            let key = key.to_owned();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let outcome =
                    runtime.acquire_actor_slot(&key, &"1".repeat(64), CancellationToken::new());
                (key, outcome)
            }));
        }
        barrier.wait();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, outcome)| outcome.is_ok())
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, outcome)| outcome.as_ref().is_err_and(|error| {
                    error.code == "capacity-exhausted"
                        && error.stage == MeshRejectionStageV1::PreAdmission
                }))
                .count(),
            1
        );
        assert_eq!(runtime.capacity().unwrap().active_actors, 1);
        let admitted_key = outcomes
            .into_iter()
            .find_map(|(key, outcome)| outcome.is_ok().then_some(key))
            .unwrap();
        drop(ActiveActorGuard {
            runtime: runtime.clone(),
            key: admitted_key,
        });
        runtime.shutdown().unwrap();
        assert_eq!(runtime.capacity().unwrap().active_actors, 0);
    }

    #[test]
    fn route_probe_uses_exact_bound_environment_policy_and_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = MeshNodeRuntime::open(MeshNodeRuntimeConfig::new(
            "mesh-node",
            temp.path().join("state"),
        ))
        .unwrap();

        let bundle = fixture_bundle(&temp.path().join("clear-unused"));
        let (_upload, mut clear) = stage_bundle(&runtime, &bundle);
        clear.execution_limits.environment_policy = MeshEnvironmentPolicyV1::Clear;
        let clear_probe = runtime.probe_route(clear).unwrap();
        assert!(clear_probe.missing.contains(&"command:sh".to_owned()));

        let (_upload, mut allow_list) = stage_bundle(&runtime, &bundle);
        allow_list.execution_limits.environment_policy = MeshEnvironmentPolicyV1::AllowList {
            names: vec!["HOME".to_owned()],
        };
        let allow_probe = runtime.probe_route(allow_list).unwrap();
        assert!(allow_probe.missing.contains(&"command:sh".to_owned()));

        let mut shadowed_bundle = fixture_bundle(&temp.path().join("shadow-unused"));
        shadowed_bundle.routes[0].environment.insert(
            "PATH".to_owned(),
            "/ostadix/mesh/definitely-missing".to_owned(),
        );
        let (_upload, shadowed) = stage_bundle(&runtime, &shadowed_bundle);
        let shadowed_probe = runtime.probe_route(shadowed).unwrap();
        assert!(shadowed_probe.missing.contains(&"command:sh".to_owned()));

        let sh = which::which("sh").unwrap();
        let mut overlaid_bundle = fixture_bundle(&temp.path().join("overlay-unused"));
        let route = &mut overlaid_bundle.routes[0];
        route.environment.insert(
            "PATH".to_owned(),
            sh.parent().unwrap().to_string_lossy().into_owned(),
        );
        route
            .environment
            .insert("MESH_OVERLAY".to_owned(), "yes".to_owned());
        route
            .guards
            .push(RouteGuard::EnvVarSet("MESH_OVERLAY".to_owned()));
        route.guards.push(RouteGuard::PlatformOs(
            std::env::consts::OS.to_ascii_uppercase(),
        ));
        let (_upload, mut overlaid) = stage_bundle(&runtime, &overlaid_bundle);
        overlaid.execution_limits.environment_policy = MeshEnvironmentPolicyV1::Clear;
        let overlaid_probe = runtime.probe_route(overlaid).unwrap();
        assert!(overlaid_probe.eligible, "{:?}", overlaid_probe.missing);
    }

    #[test]
    fn execution_limits_fits_within_validates_bounds_and_environment_sets() {
        let ceiling = MeshExecutionLimitsV1::project_defaults();
        let mut requested = ceiling.clone();
        requested.wall_clock_timeout_ms -= 1;
        requested.environment_policy = MeshEnvironmentPolicyV1::Clear;
        assert!(requested.fits_within(&ceiling));

        requested.wall_clock_timeout_ms = ceiling.wall_clock_timeout_ms.saturating_add(1);
        assert!(!requested.fits_within(&ceiling));
        requested.wall_clock_timeout_ms = 0;
        assert!(!requested.fits_within(&ceiling));

        let mut allow_ceiling = ceiling;
        allow_ceiling.environment_policy = MeshEnvironmentPolicyV1::AllowList {
            names: vec!["HOME".to_owned(), "PATH".to_owned()],
        };
        let mut allow_requested = allow_ceiling.clone();
        allow_requested.environment_policy = MeshEnvironmentPolicyV1::AllowList {
            names: vec!["PATH".to_owned()],
        };
        assert!(allow_requested.fits_within(&allow_ceiling));
    }
}
