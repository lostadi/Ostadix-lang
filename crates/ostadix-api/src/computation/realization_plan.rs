//! Authority-free operation-realization planning records.
//!
//! This module composes the semantic records in [`crate::computation_core`]
//! with stable target facts from [`crate::placement`].  It deliberately does
//! not admit work, authenticate a target, reserve capacity, dispatch an
//! implementation, or claim that a cost estimate predicts a future run.
//!
//! The first planner profile accepts one logical operation and an explicit,
//! bounded set of realization/target/representation tuple offers.  Keeping
//! tuple construction outside the planner avoids an attacker-controlled
//! Cartesian expansion over descriptor representation sets.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::canonical_cbor::{decode_bounded, encode, DecodeLimits};
use crate::computation_core::{
    artifact_id_for_bytes, verify_realization_set_v1, ComputationTokenV1, FacetIdV1, FacetKindV1,
    FacetRefV1, OComputationErrorV1, OperationContractIdV1, OperationContractV1,
    OperationInterfaceIdV1, OperationInterfaceV1, RealizationDescriptorIdV1,
    RealizationDescriptorV1, RealizationIdV1, RealizationSetIdV1, RealizationSetV1,
    SemanticArtifactRefV1,
};
use crate::placement::{
    CanonicalPlacementRecordV1, RequirementAtomV1, RequirementFootprintV1, SemanticDigestV1,
    TargetDescriptorV1,
};
use crate::resource_identity::{ArtifactId, WorldIdentityError};

pub const PHYSICAL_REPRESENTATION_SCHEMA_V1: &str = "ostadix.physical-representation/v1";
pub const TRANSFER_PLAN_SCHEMA_V1: &str = "ostadix.transfer-plan/v1";
pub const COST_PROFILE_SCHEMA_V1: &str = "ostadix.cost-profile/v1";
pub const OBJECTIVE_SCHEMA_V1: &str = "ostadix.objective/v1";
pub const LOGICAL_HGRAPH_SCHEMA_V2: &str = "ostadix.logical-hgraph/v2";
pub const DEPLOYMENT_PLAN_SCHEMA_V2: &str = "ostadix.deployment-plan/v2";
pub const RUNTIME_GRAPH_SCHEMA_V2: &str = "ostadix.runtime-graph/v2";
pub const RECOVERY_PLAN_SCHEMA_V1: &str = "ostadix.recovery-plan/v1";
pub const OPERATION_PLANNING_REQUEST_SCHEMA_V1: &str = "ostadix.operation-planning-request/v1";

/// The exact semantic schema expected when a realization descriptor resolves
/// its target-requirements content to a placement footprint.
pub const REQUIREMENT_FOOTPRINT_CONTENT_SCHEMA_V1: &str =
    "ostadix.placement.requirement-footprint/v1";

pub const MAX_REALIZATION_PLAN_RECORD_BYTES_V1: usize = 4 * 1024 * 1024;
pub const MAX_REALIZATION_PLAN_DECODE_ITEMS_V1: usize = 1_000_000;
pub const MAX_REALIZATION_PLAN_DECODE_DEPTH_V1: usize = 128;
pub const MAX_REALIZATION_PLAN_OPERATIONS_V1: usize = 65_536;
pub const MAX_REALIZATION_PLAN_EDGES_V1: usize = 262_144;
pub const MAX_REALIZATION_PLAN_CANDIDATES_V1: usize = 65_536;
pub const MAX_REALIZATION_PLAN_PORTS_V1: usize = 4_096;
pub const MAX_REALIZATION_PLAN_REFERENCES_V1: usize = 65_536;
pub const MAX_REALIZATION_PLAN_EXPLANATIONS_V1: usize = 262_144;
const MAX_REALIZATION_PLAN_TEXT_BYTES_V1: usize = 4_096;

const PHYSICAL_REPRESENTATION_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/PHYSICAL-REPRESENTATION/V1\0";
const TRANSFER_PLAN_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/TRANSFER-PLAN/V1\0";
const COST_PROFILE_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/COST-PROFILE/V1\0";
const OBJECTIVE_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/OBJECTIVE/V1\0";
const LOGICAL_HGRAPH_DIGEST_DOMAIN_V2: &[u8] = b"OSTADIX/LOGICAL-HGRAPH/V2\0";
const DEPLOYMENT_PLAN_DIGEST_DOMAIN_V2: &[u8] = b"OSTADIX/DEPLOYMENT-PLAN/V2\0";
const RUNTIME_GRAPH_DIGEST_DOMAIN_V2: &[u8] = b"OSTADIX/RUNTIME-GRAPH/V2\0";
const RECOVERY_PLAN_DIGEST_DOMAIN_V1: &[u8] = b"OSTADIX/RECOVERY-PLAN/V1\0";
const OPERATION_PLANNING_REQUEST_DIGEST_DOMAIN_V1: &[u8] =
    b"OSTADIX/OPERATION-PLANNING-REQUEST/V1\0";

#[derive(Debug, Error)]
pub enum RealizationPlanErrorV1 {
    #[error("invalid {record}: {reason}")]
    Invalid {
        record: &'static str,
        reason: String,
    },
    #[error("{record} is {actual} bytes; maximum is {maximum}")]
    RecordTooLarge {
        record: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("{record} canonical encoding failed: {reason}")]
    Canonical {
        record: &'static str,
        reason: String,
    },
    #[error("{record} JSON is invalid: {reason}")]
    Json {
        record: &'static str,
        reason: String,
    },
    #[error("{record} bytes are not the canonical CBOR encoding")]
    NonCanonicalEncoding { record: &'static str },
    #[error(transparent)]
    Computation(#[from] OComputationErrorV1),
    #[error(transparent)]
    Identity(#[from] WorldIdentityError),
    #[error("placement record is invalid: {0}")]
    Placement(String),
}

fn invalid(record: &'static str, reason: impl Into<String>) -> RealizationPlanErrorV1 {
    RealizationPlanErrorV1::Invalid {
        record,
        reason: reason.into(),
    }
}

fn validate_text(
    record: &'static str,
    field: &'static str,
    value: &str,
) -> Result<(), RealizationPlanErrorV1> {
    if value.is_empty() {
        return Err(invalid(record, format!("{field} must not be empty")));
    }
    if value.len() > MAX_REALIZATION_PLAN_TEXT_BYTES_V1 {
        return Err(invalid(
            record,
            format!("{field} exceeds {MAX_REALIZATION_PLAN_TEXT_BYTES_V1} bytes"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(
            record,
            format!("{field} contains a control character"),
        ));
    }
    Ok(())
}

fn validate_artifact(
    record: &'static str,
    field: &'static str,
    value: &ArtifactId,
) -> Result<(), RealizationPlanErrorV1> {
    if value.as_sha256().bytes().all(|byte| byte == b'0') {
        return Err(invalid(
            record,
            format!("{field} uses the reserved all-zero digest"),
        ));
    }
    Ok(())
}

fn validate_semantic_digest(
    record: &'static str,
    field: &'static str,
    value: &SemanticDigestV1,
) -> Result<(), RealizationPlanErrorV1> {
    if value.as_sha256().bytes().all(|byte| byte == b'0') {
        return Err(invalid(
            record,
            format!("{field} uses the reserved all-zero digest"),
        ));
    }
    Ok(())
}

fn validate_semantic_ref(
    record: &'static str,
    field: &'static str,
    value: &SemanticArtifactRefV1,
) -> Result<(), RealizationPlanErrorV1> {
    validate_text(record, field, value.schema.as_str())?;
    validate_artifact(record, field, &value.content)
}

fn ensure_strict_order<T: Ord>(
    record: &'static str,
    field: &'static str,
    values: &[T],
) -> Result<(), RealizationPlanErrorV1> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid(
            record,
            format!("{field} must be strictly ordered without duplicates"),
        ));
    }
    Ok(())
}

trait CanonicalRealizationPlanRecordV1:
    Clone + PartialEq + Serialize + DeserializeOwned + Sized
{
    const RECORD: &'static str;
    const SCHEMA: &'static str;
    const DIGEST_DOMAIN: &'static [u8];

    fn schema(&self) -> &str;
    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1>;
    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1>;
}

fn verify_record<T: CanonicalRealizationPlanRecordV1>(
    mut record: T,
) -> Result<T, RealizationPlanErrorV1> {
    if record.schema() != T::SCHEMA {
        return Err(invalid(
            T::RECORD,
            format!(
                "unsupported schema `{}`; expected `{}`",
                record.schema(),
                T::SCHEMA
            ),
        ));
    }
    record.canonicalize()?;
    record.validate_body()?;
    let _ = encode_record(&record)?;
    Ok(record)
}

fn encode_record<T: CanonicalRealizationPlanRecordV1>(
    record: &T,
) -> Result<Vec<u8>, RealizationPlanErrorV1> {
    let bytes = encode(record).map_err(|error| RealizationPlanErrorV1::Canonical {
        record: T::RECORD,
        reason: error.to_string(),
    })?;
    if bytes.len() > MAX_REALIZATION_PLAN_RECORD_BYTES_V1 {
        return Err(RealizationPlanErrorV1::RecordTooLarge {
            record: T::RECORD,
            actual: bytes.len(),
            maximum: MAX_REALIZATION_PLAN_RECORD_BYTES_V1,
        });
    }
    Ok(bytes)
}

fn canonical_bytes<T: CanonicalRealizationPlanRecordV1>(
    record: &T,
) -> Result<Vec<u8>, RealizationPlanErrorV1> {
    encode_record(&verify_record(record.clone())?)
}

fn canonical_json<T: CanonicalRealizationPlanRecordV1>(
    record: &T,
    pretty: bool,
) -> Result<Vec<u8>, RealizationPlanErrorV1> {
    let record = verify_record(record.clone())?;
    let mut bytes = if pretty {
        serde_json::to_vec_pretty(&record)
    } else {
        serde_json::to_vec(&record)
    }
    .map_err(|error| RealizationPlanErrorV1::Json {
        record: T::RECORD,
        reason: error.to_string(),
    })?;
    if pretty {
        bytes.push(b'\n');
    }
    if bytes.len() > MAX_REALIZATION_PLAN_RECORD_BYTES_V1 {
        return Err(RealizationPlanErrorV1::RecordTooLarge {
            record: T::RECORD,
            actual: bytes.len(),
            maximum: MAX_REALIZATION_PLAN_RECORD_BYTES_V1,
        });
    }
    Ok(bytes)
}

fn decode_json<T: CanonicalRealizationPlanRecordV1>(
    bytes: &[u8],
) -> Result<T, RealizationPlanErrorV1> {
    if bytes.len() > MAX_REALIZATION_PLAN_RECORD_BYTES_V1 {
        return Err(RealizationPlanErrorV1::RecordTooLarge {
            record: T::RECORD,
            actual: bytes.len(),
            maximum: MAX_REALIZATION_PLAN_RECORD_BYTES_V1,
        });
    }
    let record = serde_json::from_slice(bytes).map_err(|error| RealizationPlanErrorV1::Json {
        record: T::RECORD,
        reason: error.to_string(),
    })?;
    verify_record(record)
}

fn decode_canonical<T: CanonicalRealizationPlanRecordV1>(
    bytes: &[u8],
) -> Result<T, RealizationPlanErrorV1> {
    if bytes.len() > MAX_REALIZATION_PLAN_RECORD_BYTES_V1 {
        return Err(RealizationPlanErrorV1::RecordTooLarge {
            record: T::RECORD,
            actual: bytes.len(),
            maximum: MAX_REALIZATION_PLAN_RECORD_BYTES_V1,
        });
    }
    let record: T = decode_bounded(
        bytes,
        DecodeLimits {
            max_bytes: MAX_REALIZATION_PLAN_RECORD_BYTES_V1,
            max_items: MAX_REALIZATION_PLAN_DECODE_ITEMS_V1,
            max_depth: MAX_REALIZATION_PLAN_DECODE_DEPTH_V1,
        },
    )
    .map_err(|error| RealizationPlanErrorV1::Canonical {
        record: T::RECORD,
        reason: error.to_string(),
    })?;
    let record = verify_record(record)?;
    if encode_record(&record)? != bytes {
        return Err(RealizationPlanErrorV1::NonCanonicalEncoding { record: T::RECORD });
    }
    Ok(record)
}

fn record_artifact<T: CanonicalRealizationPlanRecordV1>(
    record: &T,
) -> Result<ArtifactId, RealizationPlanErrorV1> {
    let bytes = canonical_bytes(record)?;
    let length = u64::try_from(bytes.len())
        .map_err(|_| invalid(T::RECORD, "canonical length does not fit u64"))?;
    let mut hasher = Sha256::new();
    hasher.update(T::DIGEST_DOMAIN);
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
    Ok(ArtifactId::from_sha256(hex::encode(hasher.finalize()))?)
}

fn record_facet_ref<T: CanonicalRealizationPlanRecordV1>(
    record: &T,
    id: FacetIdV1,
    kind: FacetKindV1,
) -> Result<FacetRefV1, RealizationPlanErrorV1> {
    Ok(FacetRefV1::new(
        id,
        kind,
        ComputationTokenV1::new(T::SCHEMA)?,
        artifact_id_for_bytes(&canonical_bytes(record)?),
    ))
}

macro_rules! record_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(ArtifactId);

        impl $name {
            pub fn from_artifact(value: ArtifactId) -> Result<Self, RealizationPlanErrorV1> {
                validate_artifact(stringify!($name), "record identity", &value)?;
                Ok(Self(value))
            }

            pub fn from_sha256(value: impl Into<String>) -> Result<Self, RealizationPlanErrorV1> {
                Self::from_artifact(ArtifactId::from_sha256(value)?)
            }

            pub fn artifact(&self) -> &ArtifactId {
                &self.0
            }

            pub fn as_sha256(&self) -> &str {
                self.0.as_sha256()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!($prefix, "{}"), self.as_sha256())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = ArtifactId::deserialize(deserializer)?;
                Self::from_artifact(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

record_id!(
    PhysicalRepresentationIdV1,
    "physical-representation:sha256:"
);
record_id!(TransferPlanIdV1, "transfer-plan:sha256:");
record_id!(CostProfileIdV1, "cost-profile:sha256:");
record_id!(ObjectiveIdV1, "objective:sha256:");
record_id!(LogicalHGraphIdV2, "logical-hgraph-v2:sha256:");
record_id!(DeploymentPlanIdV2, "deployment-plan-v2:sha256:");
record_id!(RuntimeGraphIdV2, "runtime-graph-v2:sha256:");
record_id!(RecoveryPlanIdV1, "recovery-plan:sha256:");
record_id!(
    OperationPlanningRequestIdV1,
    "operation-planning-request:sha256:"
);

macro_rules! record_api {
    ($record:ident, $id:ident) => {
        impl $record {
            pub fn validate(&self) -> Result<(), RealizationPlanErrorV1> {
                let _ = verify_record(self.clone())?;
                Ok(())
            }

            pub fn canonical_bytes(&self) -> Result<Vec<u8>, RealizationPlanErrorV1> {
                canonical_bytes(self)
            }

            pub fn canonical_json(&self) -> Result<Vec<u8>, RealizationPlanErrorV1> {
                canonical_json(self, false)
            }

            pub fn canonical_json_pretty(&self) -> Result<Vec<u8>, RealizationPlanErrorV1> {
                canonical_json(self, true)
            }

            pub fn decode_json(bytes: &[u8]) -> Result<Self, RealizationPlanErrorV1> {
                decode_json(bytes)
            }

            pub fn decode_canonical(bytes: &[u8]) -> Result<Self, RealizationPlanErrorV1> {
                decode_canonical(bytes)
            }

            pub fn id(&self) -> Result<$id, RealizationPlanErrorV1> {
                $id::from_artifact(record_artifact(self)?)
            }
        }
    };
}

/// Storage/transport family of one physical representation.  This names a
/// representation class; it does not claim that a live instance is resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalStorageV1 {
    PortableValue,
    HostMemory,
    SharedMemory,
    MemoryMappedArtifact,
    WasmLinearMemory,
    DeviceMemory,
    RemoteStream,
    ContentAddressedArtifact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalOwnershipV1 {
    Owned,
    Borrowed,
    Shared,
    Streamed,
}

/// Immutable description of one physical spelling of a logical value type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalRepresentationV1 {
    pub schema: String,
    pub name: ComputationTokenV1,
    pub value_type: SemanticArtifactRefV1,
    pub format: SemanticArtifactRefV1,
    pub storage: PhysicalStorageV1,
    pub ownership: PhysicalOwnershipV1,
    pub mutable: bool,
}

impl PhysicalRepresentationV1 {
    pub fn new(
        name: ComputationTokenV1,
        value_type: SemanticArtifactRefV1,
        format: SemanticArtifactRefV1,
        storage: PhysicalStorageV1,
        ownership: PhysicalOwnershipV1,
        mutable: bool,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: PHYSICAL_REPRESENTATION_SCHEMA_V1.to_owned(),
            name,
            value_type,
            format,
            storage,
            ownership,
            mutable,
        })
    }

    /// Raw canonical-content reference used by `RealizationDescriptorV1`.
    pub fn semantic_ref(&self) -> Result<SemanticArtifactRefV1, RealizationPlanErrorV1> {
        Ok(SemanticArtifactRefV1::new(
            ComputationTokenV1::new(PHYSICAL_REPRESENTATION_SCHEMA_V1)?,
            artifact_id_for_bytes(&self.canonical_bytes()?),
        )?)
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::PhysicalRepresentation)
    }
}

impl CanonicalRealizationPlanRecordV1 for PhysicalRepresentationV1 {
    const RECORD: &'static str = "PhysicalRepresentationV1";
    const SCHEMA: &'static str = PHYSICAL_REPRESENTATION_SCHEMA_V1;
    const DIGEST_DOMAIN: &'static [u8] = PHYSICAL_REPRESENTATION_DIGEST_DOMAIN_V1;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_semantic_ref(Self::RECORD, "representation value type", &self.value_type)?;
        validate_semantic_ref(Self::RECORD, "representation format", &self.format)
    }
}

record_api!(PhysicalRepresentationV1, PhysicalRepresentationIdV1);

/// Port-local representation selected by a concrete planning tuple.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRepresentationSelectionV1 {
    pub port: ComputationTokenV1,
    pub representation: PhysicalRepresentationIdV1,
    pub residency: ValueResidencyV1,
}

/// Descriptive location class for an offered value.  Residency is deliberately
/// separate from its physical representation and conveys no access authority.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ValueResidencyV1 {
    Portable,
    ContentArtifact(ArtifactId),
    Target(SemanticDigestV1),
}

/// Logical-operation node identity. V2 node IDs are dense and graph-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalOperationNodeIdV2(pub u64);

/// Logical edge identity. V2 edge IDs are dense and graph-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalEdgeIdV2(pub u64);

/// One representation/target transfer selected for a logical graph edge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferPlanV1 {
    pub schema: String,
    pub logical_hgraph: LogicalHGraphIdV2,
    pub edge: LogicalEdgeIdV2,
    pub source_target: SemanticDigestV1,
    pub destination_target: SemanticDigestV1,
    pub source_representation: PhysicalRepresentationIdV1,
    pub destination_representation: PhysicalRepresentationIdV1,
    pub adapter: ArtifactId,
    pub mechanism: ComputationTokenV1,
    pub estimated_bytes: u64,
    pub estimated_cost_ns: u64,
}

impl TransferPlanV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        logical_hgraph: LogicalHGraphIdV2,
        edge: LogicalEdgeIdV2,
        source_target: SemanticDigestV1,
        destination_target: SemanticDigestV1,
        source_representation: PhysicalRepresentationIdV1,
        destination_representation: PhysicalRepresentationIdV1,
        adapter: ArtifactId,
        mechanism: ComputationTokenV1,
        estimated_bytes: u64,
        estimated_cost_ns: u64,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: TRANSFER_PLAN_SCHEMA_V1.to_owned(),
            logical_hgraph,
            edge,
            source_target,
            destination_target,
            source_representation,
            destination_representation,
            adapter,
            mechanism,
            estimated_bytes,
            estimated_cost_ns,
        })
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::TransferPlan)
    }
}

impl CanonicalRealizationPlanRecordV1 for TransferPlanV1 {
    const RECORD: &'static str = "TransferPlanV1";
    const SCHEMA: &'static str = TRANSFER_PLAN_SCHEMA_V1;
    const DIGEST_DOMAIN: &'static [u8] = TRANSFER_PLAN_DIGEST_DOMAIN_V1;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_artifact(
            Self::RECORD,
            "logical HGraph identity",
            self.logical_hgraph.artifact(),
        )?;
        validate_semantic_digest(Self::RECORD, "source target digest", &self.source_target)?;
        validate_semantic_digest(
            Self::RECORD,
            "destination target digest",
            &self.destination_target,
        )?;
        validate_artifact(Self::RECORD, "transfer adapter", &self.adapter)?;
        validate_artifact(
            Self::RECORD,
            "source representation identity",
            self.source_representation.artifact(),
        )?;
        validate_artifact(
            Self::RECORD,
            "destination representation identity",
            self.destination_representation.artifact(),
        )
    }
}

record_api!(TransferPlanV1, TransferPlanIdV1);

/// Checked additive cost components for the first explainable objective.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct CostComponentsV1 {
    pub compute_ns: u64,
    pub startup_ns: u64,
    pub conversion_ns: u64,
    pub transfer_ns: u64,
    pub queue_ns: u64,
    pub checkpoint_ns: u64,
}

impl CostComponentsV1 {
    pub fn checked_total(self) -> Option<u64> {
        self.compute_ns
            .checked_add(self.startup_ns)?
            .checked_add(self.conversion_ns)?
            .checked_add(self.transfer_ns)?
            .checked_add(self.queue_ns)?
            .checked_add(self.checkpoint_ns)
    }
}

/// Descriptive cost profile for one stable realization on one target and one
/// explicit representation tuple. It is not a prediction guarantee.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostProfileV1 {
    pub schema: String,
    pub descriptor: RealizationDescriptorIdV1,
    pub realization: RealizationIdV1,
    pub interface: OperationInterfaceIdV1,
    pub contract: OperationContractIdV1,
    pub target: SemanticDigestV1,
    pub input_geometry: SemanticArtifactRefV1,
    pub inputs: Vec<PortRepresentationSelectionV1>,
    pub outputs: Vec<PortRepresentationSelectionV1>,
    pub components: CostComponentsV1,
    pub uncertainty_ns: u64,
    pub sample_count: u64,
    pub evidence: Vec<SemanticArtifactRefV1>,
}

impl CostProfileV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        descriptor: RealizationDescriptorIdV1,
        realization: RealizationIdV1,
        interface: OperationInterfaceIdV1,
        contract: OperationContractIdV1,
        target: SemanticDigestV1,
        input_geometry: SemanticArtifactRefV1,
        inputs: Vec<PortRepresentationSelectionV1>,
        outputs: Vec<PortRepresentationSelectionV1>,
        components: CostComponentsV1,
        uncertainty_ns: u64,
        sample_count: u64,
        evidence: Vec<SemanticArtifactRefV1>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: COST_PROFILE_SCHEMA_V1.to_owned(),
            descriptor,
            realization,
            interface,
            contract,
            target,
            input_geometry,
            inputs,
            outputs,
            components,
            uncertainty_ns,
            sample_count,
            evidence,
        })
    }

    pub fn checked_total_ns(&self) -> Option<u64> {
        self.components
            .checked_total()?
            .checked_add(self.uncertainty_ns)
    }

    pub fn semantic_ref(&self) -> Result<SemanticArtifactRefV1, RealizationPlanErrorV1> {
        Ok(SemanticArtifactRefV1::new(
            ComputationTokenV1::new(COST_PROFILE_SCHEMA_V1)?,
            artifact_id_for_bytes(&self.canonical_bytes()?),
        )?)
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::CostProfile)
    }
}

fn canonicalize_port_selections(values: &mut [PortRepresentationSelectionV1]) {
    values.sort();
}

fn validate_residency(
    record: &'static str,
    residency: &ValueResidencyV1,
) -> Result<(), RealizationPlanErrorV1> {
    match residency {
        ValueResidencyV1::Portable => Ok(()),
        ValueResidencyV1::ContentArtifact(artifact) => {
            validate_artifact(record, "content residency artifact", artifact)
        }
        ValueResidencyV1::Target(target) => {
            validate_semantic_digest(record, "residency target", target)
        }
    }
}

fn validate_port_selections(
    record: &'static str,
    field: &'static str,
    values: &[PortRepresentationSelectionV1],
) -> Result<(), RealizationPlanErrorV1> {
    if values.len() > MAX_REALIZATION_PLAN_PORTS_V1 {
        return Err(invalid(record, format!("{field} exceeds the port limit")));
    }
    ensure_strict_order(record, field, values)?;
    let mut ports = BTreeSet::new();
    for value in values {
        if !ports.insert(&value.port) {
            return Err(invalid(
                record,
                format!("{field} repeats port `{}`", value.port),
            ));
        }
        validate_artifact(
            record,
            "representation identity",
            value.representation.artifact(),
        )?;
        validate_residency(record, &value.residency)?;
    }
    Ok(())
}

impl CanonicalRealizationPlanRecordV1 for CostProfileV1 {
    const RECORD: &'static str = "CostProfileV1";
    const SCHEMA: &'static str = COST_PROFILE_SCHEMA_V1;
    const DIGEST_DOMAIN: &'static [u8] = COST_PROFILE_DIGEST_DOMAIN_V1;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        canonicalize_port_selections(&mut self.inputs);
        canonicalize_port_selections(&mut self.outputs);
        self.evidence.sort();
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_artifact(
            Self::RECORD,
            "descriptor identity",
            self.descriptor.artifact(),
        )?;
        validate_artifact(
            Self::RECORD,
            "interface identity",
            self.interface.artifact(),
        )?;
        validate_artifact(Self::RECORD, "contract identity", self.contract.artifact())?;
        validate_semantic_digest(Self::RECORD, "target descriptor digest", &self.target)?;
        validate_semantic_ref(Self::RECORD, "input geometry", &self.input_geometry)?;
        validate_port_selections(
            Self::RECORD,
            "input representation selections",
            &self.inputs,
        )?;
        validate_port_selections(
            Self::RECORD,
            "output representation selections",
            &self.outputs,
        )?;
        if self.sample_count == 0 {
            return Err(invalid(Self::RECORD, "sample_count must be positive"));
        }
        if self.checked_total_ns().is_none() {
            return Err(invalid(Self::RECORD, "cost component sum overflows u64"));
        }
        if self.evidence.len() > MAX_REALIZATION_PLAN_REFERENCES_V1 {
            return Err(invalid(Self::RECORD, "evidence count exceeds the limit"));
        }
        ensure_strict_order(Self::RECORD, "cost evidence", &self.evidence)?;
        for evidence in &self.evidence {
            validate_semantic_ref(Self::RECORD, "cost evidence", evidence)?;
        }
        Ok(())
    }
}

record_api!(CostProfileV1, CostProfileIdV1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveKindV1 {
    MinimizePredictedTotalNanoseconds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveTieBreakV1 {
    CanonicalCandidateTuple,
}

/// Explicit objective used to rank already-bounded candidate tuple offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveV1 {
    pub schema: String,
    pub kind: ObjectiveKindV1,
    pub ruleset: SemanticArtifactRefV1,
    pub maximum_total_ns: Option<u64>,
    pub tie_break: ObjectiveTieBreakV1,
}

impl ObjectiveV1 {
    pub fn new_minimize_predicted_total_ns(
        ruleset: SemanticArtifactRefV1,
        maximum_total_ns: Option<u64>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: OBJECTIVE_SCHEMA_V1.to_owned(),
            kind: ObjectiveKindV1::MinimizePredictedTotalNanoseconds,
            ruleset,
            maximum_total_ns,
            tie_break: ObjectiveTieBreakV1::CanonicalCandidateTuple,
        })
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::Objective)
    }
}

impl CanonicalRealizationPlanRecordV1 for ObjectiveV1 {
    const RECORD: &'static str = "ObjectiveV1";
    const SCHEMA: &'static str = OBJECTIVE_SCHEMA_V1;
    const DIGEST_DOMAIN: &'static [u8] = OBJECTIVE_DIGEST_DOMAIN_V1;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_semantic_ref(Self::RECORD, "objective ruleset", &self.ruleset)
    }
}

record_api!(ObjectiveV1, ObjectiveIdV1);

/// One operation node in the general logical HGraph schema.  The node refers
/// to semantic operation records; it contains no implementation or target.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalOperationNodeV2 {
    pub id: LogicalOperationNodeIdV2,
    pub interface: OperationInterfaceIdV1,
    pub contract: OperationContractIdV1,
    pub realization_set: RealizationSetIdV1,
    pub input_geometry: SemanticArtifactRefV1,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalEdgeEndpointV2 {
    pub operation: LogicalOperationNodeIdV2,
    pub port: ComputationTokenV1,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalEdgeV2 {
    pub id: LogicalEdgeIdV2,
    pub producer: LogicalEdgeEndpointV2,
    pub consumer: LogicalEdgeEndpointV2,
    pub value_type: SemanticArtifactRefV1,
}

/// A general, immutable operation graph.  V2 graph-local node and edge IDs
/// are dense, while ordering is derived canonically and cycles fail closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalHGraphV2 {
    pub schema: String,
    pub operations: Vec<LogicalOperationNodeV2>,
    /// Directed value-flow edges from an output port to an input port.
    pub edges: Vec<LogicalEdgeV2>,
    /// Exact set of terminal operations (nodes with no outgoing edge).
    pub roots: Vec<LogicalOperationNodeIdV2>,
}

impl LogicalHGraphV2 {
    pub fn new(
        operations: Vec<LogicalOperationNodeV2>,
        edges: Vec<LogicalEdgeV2>,
        roots: Vec<LogicalOperationNodeIdV2>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: LOGICAL_HGRAPH_SCHEMA_V2.to_owned(),
            operations,
            edges,
            roots,
        })
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::LogicalHgraph)
    }
}

impl CanonicalRealizationPlanRecordV1 for LogicalHGraphV2 {
    const RECORD: &'static str = "LogicalHGraphV2";
    const SCHEMA: &'static str = LOGICAL_HGRAPH_SCHEMA_V2;
    const DIGEST_DOMAIN: &'static [u8] = LOGICAL_HGRAPH_DIGEST_DOMAIN_V2;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        self.operations.sort();
        self.edges.sort();
        self.roots.sort();
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        if self.operations.is_empty() {
            return Err(invalid(Self::RECORD, "operation graph must not be empty"));
        }
        if self.operations.len() > MAX_REALIZATION_PLAN_OPERATIONS_V1 {
            return Err(invalid(Self::RECORD, "operation count exceeds the limit"));
        }
        if self.edges.len() > MAX_REALIZATION_PLAN_EDGES_V1 {
            return Err(invalid(Self::RECORD, "edge count exceeds the limit"));
        }
        ensure_strict_order(Self::RECORD, "operations", &self.operations)?;
        ensure_strict_order(Self::RECORD, "edges", &self.edges)?;
        ensure_strict_order(Self::RECORD, "roots", &self.roots)?;

        let mut indegrees = BTreeMap::<LogicalOperationNodeIdV2, usize>::new();
        let mut outgoing =
            BTreeMap::<LogicalOperationNodeIdV2, Vec<LogicalOperationNodeIdV2>>::new();
        for (index, operation) in self.operations.iter().enumerate() {
            let expected = u64::try_from(index)
                .map_err(|_| invalid(Self::RECORD, "operation index does not fit u64"))?;
            if operation.id.0 != expected {
                return Err(invalid(
                    Self::RECORD,
                    format!("operation IDs must be dense from zero; expected {expected}"),
                ));
            }
            validate_artifact(
                Self::RECORD,
                "operation interface",
                operation.interface.artifact(),
            )?;
            validate_artifact(
                Self::RECORD,
                "operation contract",
                operation.contract.artifact(),
            )?;
            validate_artifact(
                Self::RECORD,
                "operation realization set",
                operation.realization_set.artifact(),
            )?;
            validate_semantic_ref(
                Self::RECORD,
                "operation input geometry",
                &operation.input_geometry,
            )?;
            indegrees.insert(operation.id, 0);
            outgoing.insert(operation.id, Vec::new());
        }

        let mut has_outgoing = BTreeSet::new();
        let mut consumers = BTreeSet::new();
        for (index, edge) in self.edges.iter().enumerate() {
            let expected = u64::try_from(index)
                .map_err(|_| invalid(Self::RECORD, "edge index does not fit u64"))?;
            if edge.id.0 != expected {
                return Err(invalid(
                    Self::RECORD,
                    format!("edge IDs must be dense from zero; expected {expected}"),
                ));
            }
            if edge.producer.operation == edge.consumer.operation {
                return Err(invalid(Self::RECORD, "self edges are not allowed"));
            }
            if !consumers.insert((edge.consumer.operation, edge.consumer.port.clone())) {
                return Err(invalid(
                    Self::RECORD,
                    "multiple edges bind the same consumer operation port",
                ));
            }
            if !indegrees.contains_key(&edge.producer.operation) {
                return Err(invalid(
                    Self::RECORD,
                    "edge producer names an unknown operation",
                ));
            }
            let Some(indegree) = indegrees.get_mut(&edge.consumer.operation) else {
                return Err(invalid(
                    Self::RECORD,
                    "edge consumer names an unknown operation",
                ));
            };
            *indegree = indegree
                .checked_add(1)
                .ok_or_else(|| invalid(Self::RECORD, "edge indegree overflows usize"))?;
            outgoing
                .get_mut(&edge.producer.operation)
                .expect("validated operation map")
                .push(edge.consumer.operation);
            has_outgoing.insert(edge.producer.operation);
            validate_semantic_ref(Self::RECORD, "edge value type", &edge.value_type)?;
        }

        let expected_roots = self
            .operations
            .iter()
            .map(|operation| operation.id)
            .filter(|id| !has_outgoing.contains(id))
            .collect::<Vec<_>>();
        if self.roots != expected_roots {
            return Err(invalid(
                Self::RECORD,
                "roots must exactly equal the graph's terminal operations",
            ));
        }

        let mut ready = indegrees
            .iter()
            .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut visited = 0usize;
        while let Some(id) = ready.pop_first() {
            visited += 1;
            for consumer in outgoing.get(&id).into_iter().flatten() {
                let degree = indegrees
                    .get_mut(consumer)
                    .expect("validated operation map");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(*consumer);
                }
            }
        }
        if visited != self.operations.len() {
            return Err(invalid(Self::RECORD, "operation graph contains a cycle"));
        }
        Ok(())
    }
}

record_api!(LogicalHGraphV2, LogicalHGraphIdV2);

/// An explicitly offered representation for one named interface port.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRepresentationOfferV1 {
    pub port: ComputationTokenV1,
    pub representation: PhysicalRepresentationV1,
    pub residency: ValueResidencyV1,
}

fn canonicalize_representation_offers(
    values: &mut [PortRepresentationOfferV1],
) -> Result<(), RealizationPlanErrorV1> {
    for value in values.iter_mut() {
        value.representation = verify_record(value.representation.clone())?;
    }
    values.sort();
    Ok(())
}

fn validate_representation_offers(
    record: &'static str,
    field: &'static str,
    values: &[PortRepresentationOfferV1],
) -> Result<(), RealizationPlanErrorV1> {
    if values.len() > MAX_REALIZATION_PLAN_PORTS_V1 {
        return Err(invalid(record, format!("{field} exceeds the port limit")));
    }
    ensure_strict_order(record, field, values)?;
    let mut ports = BTreeSet::new();
    for value in values {
        if !ports.insert(&value.port) {
            return Err(invalid(
                record,
                format!("{field} repeats port `{}`", value.port),
            ));
        }
        value.representation.validate()?;
        validate_residency(record, &value.residency)?;
    }
    Ok(())
}

fn selections_for_offers(
    offers: &[PortRepresentationOfferV1],
) -> Result<Vec<PortRepresentationSelectionV1>, RealizationPlanErrorV1> {
    offers
        .iter()
        .map(|offer| {
            Ok(PortRepresentationSelectionV1 {
                port: offer.port.clone(),
                representation: offer.representation.id()?,
                residency: offer.residency.clone(),
            })
        })
        .collect()
}

/// Canonical identity of one explicit realization x target x representation
/// tuple.  The exact target digest is the matching identity; names are retained
/// only for human- and manifest-facing joins.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationCandidateTupleV1 {
    pub logical_operation: LogicalOperationNodeIdV2,
    pub descriptor: RealizationDescriptorIdV1,
    pub realization: RealizationIdV1,
    pub target: SemanticDigestV1,
    pub target_node: String,
    pub target_display_name: String,
    pub inputs: Vec<PortRepresentationSelectionV1>,
    pub outputs: Vec<PortRepresentationSelectionV1>,
    pub cost_profile: CostProfileIdV1,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ConceptualCandidateKeyV1 {
    logical_operation: LogicalOperationNodeIdV2,
    descriptor: RealizationDescriptorIdV1,
    target: SemanticDigestV1,
    inputs: Vec<PortRepresentationSelectionV1>,
    outputs: Vec<PortRepresentationSelectionV1>,
}

fn conceptual_candidate_key(candidate: &RealizationCandidateTupleV1) -> ConceptualCandidateKeyV1 {
    ConceptualCandidateKeyV1 {
        logical_operation: candidate.logical_operation,
        descriptor: candidate.descriptor.clone(),
        target: candidate.target.clone(),
        inputs: candidate.inputs.clone(),
        outputs: candidate.outputs.clone(),
    }
}

fn canonicalize_candidate(candidate: &mut RealizationCandidateTupleV1) {
    canonicalize_port_selections(&mut candidate.inputs);
    canonicalize_port_selections(&mut candidate.outputs);
}

fn validate_candidate(
    record: &'static str,
    candidate: &RealizationCandidateTupleV1,
) -> Result<(), RealizationPlanErrorV1> {
    validate_artifact(
        record,
        "candidate descriptor",
        candidate.descriptor.artifact(),
    )?;
    validate_semantic_digest(record, "candidate target", &candidate.target)?;
    validate_text(record, "candidate target node", &candidate.target_node)?;
    validate_text(
        record,
        "candidate target display name",
        &candidate.target_display_name,
    )?;
    validate_port_selections(record, "candidate inputs", &candidate.inputs)?;
    validate_port_selections(record, "candidate outputs", &candidate.outputs)?;
    validate_artifact(
        record,
        "candidate cost profile",
        candidate.cost_profile.artifact(),
    )
}

/// One caller-supplied candidate tuple.  No Cartesian product is constructed
/// by the planner: every tuple considered must appear in this bounded list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateTupleOfferV1 {
    pub logical_operation: LogicalOperationNodeIdV2,
    pub descriptor: RealizationDescriptorIdV1,
    pub target: TargetDescriptorV1,
    pub target_requirements: RequirementFootprintV1,
    pub inputs: Vec<PortRepresentationOfferV1>,
    pub outputs: Vec<PortRepresentationOfferV1>,
    pub cost_profile: CostProfileV1,
}

impl CandidateTupleOfferV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        logical_operation: LogicalOperationNodeIdV2,
        descriptor: RealizationDescriptorIdV1,
        target: TargetDescriptorV1,
        target_requirements: RequirementFootprintV1,
        mut inputs: Vec<PortRepresentationOfferV1>,
        mut outputs: Vec<PortRepresentationOfferV1>,
        cost_profile: CostProfileV1,
    ) -> Result<Self, RealizationPlanErrorV1> {
        canonicalize_representation_offers(&mut inputs)?;
        canonicalize_representation_offers(&mut outputs)?;
        let value = Self {
            logical_operation,
            descriptor,
            target,
            target_requirements,
            inputs,
            outputs,
            cost_profile: verify_record(cost_profile)?,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_artifact(
            "CandidateTupleOfferV1",
            "descriptor",
            self.descriptor.artifact(),
        )?;
        self.target
            .canonical_bytes()
            .map_err(|error| RealizationPlanErrorV1::Placement(error.to_string()))?;
        self.target_requirements
            .canonical_bytes()
            .map_err(|error| RealizationPlanErrorV1::Placement(error.to_string()))?;
        validate_representation_offers("CandidateTupleOfferV1", "inputs", &self.inputs)?;
        validate_representation_offers("CandidateTupleOfferV1", "outputs", &self.outputs)?;
        self.cost_profile.validate()?;
        let candidate = self.candidate()?;
        validate_candidate("CandidateTupleOfferV1", &candidate)
    }

    pub fn target_digest(&self) -> Result<SemanticDigestV1, RealizationPlanErrorV1> {
        self.target
            .semantic_digest()
            .map_err(|error| RealizationPlanErrorV1::Placement(error.to_string()))
    }

    pub fn candidate(&self) -> Result<RealizationCandidateTupleV1, RealizationPlanErrorV1> {
        let mut candidate = RealizationCandidateTupleV1 {
            logical_operation: self.logical_operation,
            descriptor: self.descriptor.clone(),
            realization: self.cost_profile.realization.clone(),
            target: self.target_digest()?,
            target_node: self.target.node_id().to_owned(),
            target_display_name: self.target.display_name().to_owned(),
            inputs: selections_for_offers(&self.inputs)?,
            outputs: selections_for_offers(&self.outputs)?,
            cost_profile: self.cost_profile.id()?,
        };
        canonicalize_candidate(&mut candidate);
        validate_candidate("CandidateTupleOfferV1", &candidate)?;
        Ok(candidate)
    }

    fn conceptual_key(&self) -> Result<ConceptualCandidateKeyV1, RealizationPlanErrorV1> {
        Ok(conceptual_candidate_key(&self.candidate()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDispositionV1 {
    Rejected,
    Rankable,
    Selected,
}

/// The two conservative non-complete states exposed by
/// [`RequirementFootprintV1`].  Keeping this field typed prevents a detached
/// deployment explanation from inventing a third footprint state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementFootprintIncompleteStateV1 {
    ConservativeUnknown,
    Unsatisfiable,
}

/// Machine-readable causal facts emitted for every considered tuple.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlanningReasonV1 {
    ExactFidelityCompatible,
    ExactFidelityMismatch,
    TargetRequirementsContentMatched,
    TargetRequirementsContentMismatch,
    RequirementFootprintIncomplete {
        state: RequirementFootprintIncompleteStateV1,
        reasons: Vec<String>,
    },
    StaticRequirementSatisfied {
        requirement: String,
    },
    StaticRequirementUnsupported {
        requirement: String,
    },
    DynamicRequirementUndischarged {
        requirement: String,
    },
    StateRequirementsDeferred,
    ActorRequirementsDeferred,
    DescriptorOutsideRealizationSet,
    OfferNamesDifferentLogicalOperation,
    InputRepresentationMatched {
        port: ComputationTokenV1,
        representation: PhysicalRepresentationIdV1,
    },
    OutputRepresentationMatched {
        port: ComputationTokenV1,
        representation: PhysicalRepresentationIdV1,
    },
    MissingInputRepresentation {
        port: ComputationTokenV1,
    },
    MissingOutputRepresentation {
        port: ComputationTokenV1,
    },
    UnexpectedInputRepresentation {
        port: ComputationTokenV1,
    },
    UnexpectedOutputRepresentation {
        port: ComputationTokenV1,
    },
    InputValueTypeMismatch {
        port: ComputationTokenV1,
    },
    OutputValueTypeMismatch {
        port: ComputationTokenV1,
    },
    InputRepresentationNotDeclared {
        port: ComputationTokenV1,
    },
    OutputRepresentationNotDeclared {
        port: ComputationTokenV1,
    },
    CostProfileDescriptorMismatch,
    CostProfileRealizationMismatch,
    CostProfileInterfaceMismatch,
    CostProfileContractMismatch,
    CostProfileTargetMismatch,
    CostProfileInputGeometryMismatch,
    CostProfileInputRepresentationsMismatch,
    CostProfileOutputRepresentationsMismatch,
    CostProfileCoordinatesMatched,
    PredictedTotalNanoseconds {
        value: u64,
    },
    ObjectiveMaximumSatisfied {
        maximum: u64,
        actual: u64,
    },
    ObjectiveMaximumExceeded {
        maximum: u64,
        actual: u64,
    },
    StaticallyCompatibleForRanking,
    SelectedByObjective,
    NotSelectedHigherCost {
        selected: u64,
        candidate: u64,
    },
    NotSelectedCanonicalTieBreak,
    NoCandidateOffers,
    NoRankableCandidate,
}

fn validate_reason(reason: &PlanningReasonV1) -> Result<(), RealizationPlanErrorV1> {
    match reason {
        PlanningReasonV1::RequirementFootprintIncomplete { reasons, .. } => {
            if reasons.is_empty() {
                return Err(invalid(
                    "PlanningReasonV1",
                    "incomplete footprint has no reason",
                ));
            }
            if reasons.len() > MAX_REALIZATION_PLAN_EXPLANATIONS_V1 {
                return Err(invalid(
                    "PlanningReasonV1",
                    "footprint reasons exceed the limit",
                ));
            }
            ensure_strict_order("PlanningReasonV1", "footprint reasons", reasons)?;
            for value in reasons {
                validate_text("PlanningReasonV1", "footprint reason", value)?;
            }
        }
        PlanningReasonV1::StaticRequirementSatisfied { requirement }
        | PlanningReasonV1::StaticRequirementUnsupported { requirement }
        | PlanningReasonV1::DynamicRequirementUndischarged { requirement } => {
            validate_text("PlanningReasonV1", "requirement", requirement)?;
        }
        _ => {}
    }
    Ok(())
}

fn is_rejection_cause(reason: &PlanningReasonV1) -> bool {
    matches!(
        reason,
        PlanningReasonV1::ExactFidelityMismatch
            | PlanningReasonV1::TargetRequirementsContentMismatch
            | PlanningReasonV1::RequirementFootprintIncomplete { .. }
            | PlanningReasonV1::StaticRequirementUnsupported { .. }
            | PlanningReasonV1::DynamicRequirementUndischarged { .. }
            | PlanningReasonV1::DescriptorOutsideRealizationSet
            | PlanningReasonV1::OfferNamesDifferentLogicalOperation
            | PlanningReasonV1::MissingInputRepresentation { .. }
            | PlanningReasonV1::MissingOutputRepresentation { .. }
            | PlanningReasonV1::UnexpectedInputRepresentation { .. }
            | PlanningReasonV1::UnexpectedOutputRepresentation { .. }
            | PlanningReasonV1::InputValueTypeMismatch { .. }
            | PlanningReasonV1::OutputValueTypeMismatch { .. }
            | PlanningReasonV1::InputRepresentationNotDeclared { .. }
            | PlanningReasonV1::OutputRepresentationNotDeclared { .. }
            | PlanningReasonV1::CostProfileDescriptorMismatch
            | PlanningReasonV1::CostProfileRealizationMismatch
            | PlanningReasonV1::CostProfileInterfaceMismatch
            | PlanningReasonV1::CostProfileContractMismatch
            | PlanningReasonV1::CostProfileTargetMismatch
            | PlanningReasonV1::CostProfileInputGeometryMismatch
            | PlanningReasonV1::CostProfileInputRepresentationsMismatch
            | PlanningReasonV1::CostProfileOutputRepresentationsMismatch
            | PlanningReasonV1::ObjectiveMaximumExceeded { .. }
    )
}

fn is_rank_selection_marker(reason: &PlanningReasonV1) -> bool {
    matches!(
        reason,
        PlanningReasonV1::StaticallyCompatibleForRanking
            | PlanningReasonV1::SelectedByObjective
            | PlanningReasonV1::NotSelectedHigherCost { .. }
            | PlanningReasonV1::NotSelectedCanonicalTieBreak
    )
}

fn is_nonselection_marker(reason: &PlanningReasonV1) -> bool {
    matches!(
        reason,
        PlanningReasonV1::NotSelectedHigherCost { .. }
            | PlanningReasonV1::NotSelectedCanonicalTieBreak
    )
}

fn is_operation_summary_reason(reason: &PlanningReasonV1) -> bool {
    matches!(
        reason,
        PlanningReasonV1::NoCandidateOffers | PlanningReasonV1::NoRankableCandidate
    )
}

fn is_cost_profile_mismatch(reason: &PlanningReasonV1) -> bool {
    matches!(
        reason,
        PlanningReasonV1::CostProfileDescriptorMismatch
            | PlanningReasonV1::CostProfileRealizationMismatch
            | PlanningReasonV1::CostProfileInterfaceMismatch
            | PlanningReasonV1::CostProfileContractMismatch
            | PlanningReasonV1::CostProfileTargetMismatch
            | PlanningReasonV1::CostProfileInputGeometryMismatch
            | PlanningReasonV1::CostProfileInputRepresentationsMismatch
            | PlanningReasonV1::CostProfileOutputRepresentationsMismatch
    )
}

fn objective_maximum(assessment: &CandidateAssessmentV1) -> Option<u64> {
    assessment.reasons.iter().find_map(|reason| match reason {
        PlanningReasonV1::ObjectiveMaximumSatisfied { maximum, .. }
        | PlanningReasonV1::ObjectiveMaximumExceeded { maximum, .. } => Some(*maximum),
        _ => None,
    })
}

fn validate_intrinsic_reason_consistency(
    reasons: &[PlanningReasonV1],
) -> Result<(), RealizationPlanErrorV1> {
    let contradictory_pair = [
        (
            PlanningReasonV1::ExactFidelityCompatible,
            PlanningReasonV1::ExactFidelityMismatch,
        ),
        (
            PlanningReasonV1::TargetRequirementsContentMatched,
            PlanningReasonV1::TargetRequirementsContentMismatch,
        ),
    ]
    .into_iter()
    .any(|(left, right)| reasons.contains(&left) && reasons.contains(&right));
    if contradictory_pair {
        return Err(invalid(
            "CandidateAssessmentV1",
            "causal explanation contains mutually exclusive compatibility facts",
        ));
    }

    if reasons.contains(&PlanningReasonV1::CostProfileCoordinatesMatched)
        && reasons.iter().any(is_cost_profile_mismatch)
    {
        return Err(invalid(
            "CandidateAssessmentV1",
            "cost profile cannot both match and mismatch its coordinates",
        ));
    }

    let descriptor_outside = reasons.contains(&PlanningReasonV1::DescriptorOutsideRealizationSet);
    let descriptor_resolved_fact = reasons.iter().any(|reason| {
        matches!(
            reason,
            PlanningReasonV1::StateRequirementsDeferred
                | PlanningReasonV1::ActorRequirementsDeferred
                | PlanningReasonV1::ExactFidelityCompatible
                | PlanningReasonV1::ExactFidelityMismatch
                | PlanningReasonV1::TargetRequirementsContentMatched
                | PlanningReasonV1::TargetRequirementsContentMismatch
                | PlanningReasonV1::CostProfileRealizationMismatch
        )
    });
    if descriptor_outside && descriptor_resolved_fact {
        return Err(invalid(
            "CandidateAssessmentV1",
            "descriptor-absent and descriptor-resolved facts are mutually exclusive",
        ));
    }

    let footprint_incomplete = reasons.iter().any(|reason| {
        matches!(
            reason,
            PlanningReasonV1::RequirementFootprintIncomplete { .. }
        )
    });
    let mut requirement_outcomes = BTreeMap::<&str, &'static str>::new();
    for reason in reasons {
        let outcome = match reason {
            PlanningReasonV1::StaticRequirementSatisfied { requirement } => {
                Some((requirement.as_str(), "satisfied"))
            }
            PlanningReasonV1::StaticRequirementUnsupported { requirement } => {
                Some((requirement.as_str(), "unsupported"))
            }
            PlanningReasonV1::DynamicRequirementUndischarged { requirement } => {
                Some((requirement.as_str(), "undischarged"))
            }
            _ => None,
        };
        let Some((requirement, outcome)) = outcome else {
            continue;
        };
        if requirement_outcomes
            .insert(requirement, outcome)
            .is_some_and(|previous| previous != outcome)
        {
            return Err(invalid(
                "CandidateAssessmentV1",
                format!("requirement `{requirement}` has mutually exclusive outcomes"),
            ));
        }
    }
    if footprint_incomplete && !requirement_outcomes.is_empty() {
        return Err(invalid(
            "CandidateAssessmentV1",
            "an incomplete footprint cannot also carry per-requirement outcomes",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAssessmentV1 {
    pub candidate: RealizationCandidateTupleV1,
    pub disposition: CandidateDispositionV1,
    /// Present whenever the candidate's structurally valid profile had a
    /// checked total, including when another compatibility check rejected it.
    pub predicted_total_ns: Option<u64>,
    pub reasons: Vec<PlanningReasonV1>,
}

fn canonicalize_assessment(assessment: &mut CandidateAssessmentV1) {
    canonicalize_candidate(&mut assessment.candidate);
    assessment.reasons.sort();
    assessment.reasons.dedup();
}

fn validate_assessment(assessment: &CandidateAssessmentV1) -> Result<(), RealizationPlanErrorV1> {
    validate_candidate("CandidateAssessmentV1", &assessment.candidate)?;
    if assessment.reasons.is_empty() {
        return Err(invalid(
            "CandidateAssessmentV1",
            "causal explanation is empty",
        ));
    }
    if assessment.reasons.len() > MAX_REALIZATION_PLAN_EXPLANATIONS_V1 {
        return Err(invalid(
            "CandidateAssessmentV1",
            "explanation count exceeds the limit",
        ));
    }
    ensure_strict_order(
        "CandidateAssessmentV1",
        "causal explanations",
        &assessment.reasons,
    )?;
    for reason in &assessment.reasons {
        validate_reason(reason)?;
    }
    validate_intrinsic_reason_consistency(&assessment.reasons)?;
    let Some(predicted_total) = assessment.predicted_total_ns else {
        return Err(invalid(
            "CandidateAssessmentV1",
            "candidate assessment has no checked cost",
        ));
    };
    let predicted_reasons = assessment
        .reasons
        .iter()
        .filter_map(|reason| match reason {
            PlanningReasonV1::PredictedTotalNanoseconds { value } => Some(*value),
            _ => None,
        })
        .collect::<Vec<_>>();
    if predicted_reasons != [predicted_total] {
        return Err(invalid(
            "CandidateAssessmentV1",
            "predicted cost reason does not exactly match the assessment score",
        ));
    }
    let satisfied = assessment
        .reasons
        .iter()
        .filter_map(|reason| match reason {
            PlanningReasonV1::ObjectiveMaximumSatisfied { maximum, actual } => {
                Some((*maximum, *actual))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let exceeded = assessment
        .reasons
        .iter()
        .filter_map(|reason| match reason {
            PlanningReasonV1::ObjectiveMaximumExceeded { maximum, actual } => {
                Some((*maximum, *actual))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if satisfied.len() > 1 || exceeded.len() > 1 || (!satisfied.is_empty() && !exceeded.is_empty())
    {
        return Err(invalid(
            "CandidateAssessmentV1",
            "objective maximum reasons are not mutually exclusive",
        ));
    }
    if satisfied
        .first()
        .is_some_and(|(maximum, actual)| *actual != predicted_total || *actual > *maximum)
        || exceeded
            .first()
            .is_some_and(|(maximum, actual)| *actual != predicted_total || *actual <= *maximum)
    {
        return Err(invalid(
            "CandidateAssessmentV1",
            "objective maximum reason arithmetic is incoherent",
        ));
    }
    if assessment.reasons.iter().any(is_operation_summary_reason) {
        return Err(invalid(
            "CandidateAssessmentV1",
            "candidate assessment carries an operation-summary reason",
        ));
    }
    let has_rejection_cause = assessment.reasons.iter().any(is_rejection_cause);
    let has_static_rank = assessment
        .reasons
        .contains(&PlanningReasonV1::StaticallyCompatibleForRanking);
    let has_selected = assessment
        .reasons
        .contains(&PlanningReasonV1::SelectedByObjective);
    let nonselection_reason_count = assessment
        .reasons
        .iter()
        .filter(|reason| is_nonselection_marker(reason))
        .count();
    match assessment.disposition {
        CandidateDispositionV1::Rejected
            if !has_rejection_cause || assessment.reasons.iter().any(is_rank_selection_marker) =>
        {
            return Err(invalid(
                "CandidateAssessmentV1",
                "rejected candidate reason set is incoherent",
            ));
        }
        CandidateDispositionV1::Rankable
            if has_rejection_cause
                || !has_static_rank
                || has_selected
                || nonselection_reason_count != 1 =>
        {
            return Err(invalid(
                "CandidateAssessmentV1",
                "rankable candidate reason set is incoherent",
            ));
        }
        CandidateDispositionV1::Selected
            if has_rejection_cause
                || !has_static_rank
                || !has_selected
                || nonselection_reason_count != 0 =>
        {
            return Err(invalid(
                "CandidateAssessmentV1",
                "selected candidate reason set is incoherent",
            ));
        }
        _ => {}
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentOperationV2 {
    pub logical_operation: LogicalOperationNodeIdV2,
    pub selection: Option<RealizationCandidateTupleV1>,
    pub candidates: Vec<CandidateAssessmentV1>,
    pub reasons: Vec<PlanningReasonV1>,
}

/// Pure descriptive result of the joint tuple planner.  Selection is not
/// admission, a lease, a dispatch request, or evidence that execution ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentPlanV2 {
    pub schema: String,
    pub logical_hgraph: LogicalHGraphIdV2,
    pub objective: ObjectiveIdV1,
    pub operations: Vec<DeploymentOperationV2>,
    /// Semantic execution order selected by the planner.  It binds order but
    /// conveys neither admission nor permission to execute.
    pub schedule: Vec<LogicalOperationNodeIdV2>,
    pub transfers: Vec<TransferPlanIdV1>,
}

impl DeploymentPlanV2 {
    pub fn new(
        logical_hgraph: LogicalHGraphIdV2,
        objective: ObjectiveIdV1,
        operations: Vec<DeploymentOperationV2>,
        schedule: Vec<LogicalOperationNodeIdV2>,
        transfers: Vec<TransferPlanIdV1>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: DEPLOYMENT_PLAN_SCHEMA_V2.to_owned(),
            logical_hgraph,
            objective,
            operations,
            schedule,
            transfers,
        })
    }

    /// Convenience for the first planner profile, which accepts exactly one
    /// logical operation.
    pub fn selected_candidate(&self) -> Option<&RealizationCandidateTupleV1> {
        (self.operations.len() == 1)
            .then(|| {
                self.operations
                    .first()
                    .and_then(|operation| operation.selection.as_ref())
            })
            .flatten()
    }

    pub fn selected_candidates(&self) -> impl Iterator<Item = &RealizationCandidateTupleV1> {
        self.operations
            .iter()
            .filter_map(|operation| operation.selection.as_ref())
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::Deployment)
    }

    /// Recompute the authority-free planner result from the complete request
    /// closure and require byte-semantic equality with this deployment.  This
    /// resolves facts that an intrinsically valid detached deployment cannot
    /// prove, such as whether a rejection reason was earned by the offer.
    pub fn verify_against(
        &self,
        request: &OperationPlanningRequestV1,
    ) -> Result<(), RealizationPlanErrorV1> {
        let actual = verify_record(self.clone())?;
        let expected = plan_operation_realization_v1(request)?;
        if actual != expected {
            return Err(invalid(
                Self::RECORD,
                "deployment does not exactly match the recomputed planning request",
            ));
        }
        Ok(())
    }
}

impl CanonicalRealizationPlanRecordV1 for DeploymentPlanV2 {
    const RECORD: &'static str = "DeploymentPlanV2";
    const SCHEMA: &'static str = DEPLOYMENT_PLAN_SCHEMA_V2;
    const DIGEST_DOMAIN: &'static [u8] = DEPLOYMENT_PLAN_DIGEST_DOMAIN_V2;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        for operation in &mut self.operations {
            if let Some(selection) = &mut operation.selection {
                canonicalize_candidate(selection);
            }
            for assessment in &mut operation.candidates {
                canonicalize_assessment(assessment);
            }
            operation
                .candidates
                .sort_by(|left, right| left.candidate.cmp(&right.candidate));
            operation.reasons.sort();
            operation.reasons.dedup();
        }
        self.operations
            .sort_by_key(|operation| operation.logical_operation);
        self.transfers.sort();
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_artifact(
            Self::RECORD,
            "logical HGraph",
            self.logical_hgraph.artifact(),
        )?;
        validate_artifact(Self::RECORD, "objective", self.objective.artifact())?;
        if self.operations.is_empty() {
            return Err(invalid(
                Self::RECORD,
                "deployment has no logical operations",
            ));
        }
        if self.operations.len() > MAX_REALIZATION_PLAN_OPERATIONS_V1 {
            return Err(invalid(
                Self::RECORD,
                "deployment operation count exceeds the limit",
            ));
        }
        if self.transfers.len() > MAX_REALIZATION_PLAN_EDGES_V1 {
            return Err(invalid(
                Self::RECORD,
                "deployment transfer count exceeds the limit",
            ));
        }
        if self.schedule.len() != self.operations.len() {
            return Err(invalid(
                Self::RECORD,
                "schedule must exactly cover deployment operations",
            ));
        }
        let scheduled = self.schedule.iter().copied().collect::<BTreeSet<_>>();
        let deployed = self
            .operations
            .iter()
            .map(|operation| operation.logical_operation)
            .collect::<BTreeSet<_>>();
        if scheduled.len() != self.schedule.len() || scheduled != deployed {
            return Err(invalid(
                Self::RECORD,
                "schedule must name every deployment operation exactly once",
            ));
        }
        ensure_strict_order(Self::RECORD, "deployment transfers", &self.transfers)?;
        for transfer in &self.transfers {
            validate_artifact(Self::RECORD, "transfer identity", transfer.artifact())?;
        }

        let mut previous_operation = None;
        let mut objective_maximum_policy: Option<Option<u64>> = None;
        let mut explanation_count = 0usize;
        for operation in &self.operations {
            if previous_operation.is_some_and(|previous| previous >= operation.logical_operation) {
                return Err(invalid(
                    Self::RECORD,
                    "deployment operations must be strictly ordered",
                ));
            }
            previous_operation = Some(operation.logical_operation);
            if operation.candidates.len() > MAX_REALIZATION_PLAN_CANDIDATES_V1 {
                return Err(invalid(Self::RECORD, "candidate count exceeds the limit"));
            }
            if operation.reasons.len() > MAX_REALIZATION_PLAN_EXPLANATIONS_V1 {
                return Err(invalid(
                    Self::RECORD,
                    "operation explanation count exceeds the limit",
                ));
            }
            ensure_strict_order(Self::RECORD, "operation explanations", &operation.reasons)?;
            for reason in &operation.reasons {
                validate_reason(reason)?;
            }
            explanation_count = explanation_count
                .checked_add(operation.reasons.len())
                .ok_or_else(|| invalid(Self::RECORD, "explanation count overflows usize"))?;

            let mut prior_candidate: Option<&RealizationCandidateTupleV1> = None;
            let mut conceptual_candidates = BTreeSet::new();
            let mut selected_assessment: Option<&CandidateAssessmentV1> = None;
            let mut rankable_count = 0usize;
            for assessment in &operation.candidates {
                if prior_candidate.is_some_and(|prior| prior >= &assessment.candidate) {
                    return Err(invalid(
                        Self::RECORD,
                        "candidate assessments must be strictly ordered",
                    ));
                }
                prior_candidate = Some(&assessment.candidate);
                if !conceptual_candidates.insert(conceptual_candidate_key(&assessment.candidate)) {
                    return Err(invalid(
                        Self::RECORD,
                        "candidate assessments repeat a conceptual tuple",
                    ));
                }
                let names_different_operation =
                    assessment.candidate.logical_operation != operation.logical_operation;
                let explains_different_operation = assessment
                    .reasons
                    .contains(&PlanningReasonV1::OfferNamesDifferentLogicalOperation);
                if names_different_operation != explains_different_operation {
                    return Err(invalid(
                        Self::RECORD,
                        "candidate operation mismatch and its causal reason must agree exactly",
                    ));
                }
                if names_different_operation
                    && assessment.disposition != CandidateDispositionV1::Rejected
                {
                    return Err(invalid(
                        Self::RECORD,
                        "a candidate for a different logical operation must be rejected",
                    ));
                }
                validate_assessment(assessment)?;
                let candidate_maximum = objective_maximum(assessment);
                match objective_maximum_policy {
                    None => objective_maximum_policy = Some(candidate_maximum),
                    Some(expected) if expected != candidate_maximum => {
                        return Err(invalid(
                            Self::RECORD,
                            "candidate assessments do not share one objective-maximum policy and value",
                        ));
                    }
                    Some(_) => {}
                }
                if assessment.disposition == CandidateDispositionV1::Rankable {
                    rankable_count += 1;
                }
                explanation_count = explanation_count
                    .checked_add(assessment.reasons.len())
                    .ok_or_else(|| invalid(Self::RECORD, "explanation count overflows usize"))?;
                if assessment.disposition == CandidateDispositionV1::Selected
                    && selected_assessment.replace(assessment).is_some()
                {
                    return Err(invalid(
                        Self::RECORD,
                        "operation selects multiple candidates",
                    ));
                }
            }
            match (&operation.selection, selected_assessment) {
                (Some(expected), Some(actual)) if expected == &actual.candidate => {
                    if expected.logical_operation != operation.logical_operation {
                        return Err(invalid(
                            Self::RECORD,
                            "selection names a different logical operation",
                        ));
                    }
                    validate_candidate(Self::RECORD, expected)?;
                }
                (None, None) => {}
                _ => {
                    return Err(invalid(
                        Self::RECORD,
                        "operation selection does not match its selected assessment",
                    ));
                }
            }
            if operation.selection.is_none() && rankable_count != 0 {
                return Err(invalid(
                    Self::RECORD,
                    "deployment leaves rankable candidates without selecting one",
                ));
            }

            let expected_summary = if operation.candidates.is_empty() {
                vec![PlanningReasonV1::NoCandidateOffers]
            } else if operation.selection.is_none() {
                vec![PlanningReasonV1::NoRankableCandidate]
            } else {
                Vec::new()
            };
            if operation.reasons != expected_summary {
                return Err(invalid(
                    Self::RECORD,
                    "operation summary reasons do not exactly describe its candidate closure",
                ));
            }

            if let (Some(selection), Some(selected_assessment)) =
                (&operation.selection, selected_assessment)
            {
                let expected_winner = operation
                    .candidates
                    .iter()
                    .filter(|assessment| assessment.disposition != CandidateDispositionV1::Rejected)
                    .min_by(|left, right| {
                        (
                            left.predicted_total_ns.expect("validated assessment cost"),
                            &left.candidate,
                        )
                            .cmp(&(
                                right.predicted_total_ns.expect("validated assessment cost"),
                                &right.candidate,
                            ))
                    })
                    .expect("selected assessment is a non-rejected candidate");
                if &expected_winner.candidate != selection {
                    return Err(invalid(
                        Self::RECORD,
                        "selection is not the lowest-cost canonical candidate tuple",
                    ));
                }
                let selected_total = selected_assessment
                    .predicted_total_ns
                    .expect("validated selected assessment cost");
                for assessment in &operation.candidates {
                    if assessment.disposition != CandidateDispositionV1::Rankable {
                        continue;
                    }
                    let candidate_total = assessment
                        .predicted_total_ns
                        .expect("validated rankable assessment cost");
                    let nonselection = assessment
                        .reasons
                        .iter()
                        .find(|reason| is_nonselection_marker(reason))
                        .expect("validated rankable assessment has one non-selection reason");
                    let coherent = match nonselection {
                        PlanningReasonV1::NotSelectedHigherCost {
                            selected,
                            candidate,
                        } => {
                            candidate_total > selected_total
                                && *selected == selected_total
                                && *candidate == candidate_total
                        }
                        PlanningReasonV1::NotSelectedCanonicalTieBreak => {
                            candidate_total == selected_total
                                && assessment
                                    .candidate
                                    .cmp(&selected_assessment.candidate)
                                    .is_gt()
                        }
                        _ => unreachable!("filtered to non-selection reasons"),
                    };
                    if !coherent {
                        return Err(invalid(
                            Self::RECORD,
                            "rankable candidate has the wrong non-selection reason or score",
                        ));
                    }
                }
            }
        }
        if explanation_count > MAX_REALIZATION_PLAN_EXPLANATIONS_V1 {
            return Err(invalid(
                Self::RECORD,
                "total explanation count exceeds the limit",
            ));
        }
        Ok(())
    }
}

record_api!(DeploymentPlanV2, DeploymentPlanIdV2);

/// Self-contained record closure and bounded input for the first
/// authority-free operation planner. Semantic record relationships are
/// verified exactly; referenced content remains intentionally opaque.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationPlanningRequestV1 {
    pub schema: String,
    pub graph: LogicalHGraphV2,
    pub contract: OperationContractV1,
    pub interface: OperationInterfaceV1,
    pub descriptors: Vec<RealizationDescriptorV1>,
    pub realization_set: RealizationSetV1,
    pub objective: ObjectiveV1,
    pub offers: Vec<CandidateTupleOfferV1>,
    pub transfer_plans: Vec<TransferPlanV1>,
}

impl OperationPlanningRequestV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        graph: LogicalHGraphV2,
        contract: OperationContractV1,
        interface: OperationInterfaceV1,
        descriptors: Vec<RealizationDescriptorV1>,
        realization_set: RealizationSetV1,
        objective: ObjectiveV1,
        offers: Vec<CandidateTupleOfferV1>,
        transfer_plans: Vec<TransferPlanV1>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: OPERATION_PLANNING_REQUEST_SCHEMA_V1.to_owned(),
            graph,
            contract,
            interface,
            descriptors,
            realization_set,
            objective,
            offers,
            transfer_plans,
        })
    }
}

impl CanonicalRealizationPlanRecordV1 for OperationPlanningRequestV1 {
    const RECORD: &'static str = "OperationPlanningRequestV1";
    const SCHEMA: &'static str = OPERATION_PLANNING_REQUEST_SCHEMA_V1;
    const DIGEST_DOMAIN: &'static [u8] = OPERATION_PLANNING_REQUEST_DIGEST_DOMAIN_V1;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        self.graph = verify_record(self.graph.clone())?;
        self.contract = self.contract.clone().verify()?;
        self.interface = self.interface.clone().verify()?;
        self.realization_set = self.realization_set.clone().verify()?;
        self.objective = verify_record(self.objective.clone())?;

        let mut descriptors = Vec::with_capacity(self.descriptors.len());
        for descriptor in self.descriptors.drain(..) {
            let descriptor = descriptor.verify()?;
            descriptors.push((descriptor.id()?, descriptor));
        }
        descriptors.sort_by(|left, right| left.0.cmp(&right.0));
        self.descriptors = descriptors
            .into_iter()
            .map(|(_, descriptor)| descriptor)
            .collect();

        let mut offers = Vec::with_capacity(self.offers.len());
        for mut offer in self.offers.drain(..) {
            canonicalize_representation_offers(&mut offer.inputs)?;
            canonicalize_representation_offers(&mut offer.outputs)?;
            offer.cost_profile = verify_record(offer.cost_profile)?;
            let candidate = offer.candidate()?;
            offers.push((candidate, offer));
        }
        offers.sort_by(|left, right| left.0.cmp(&right.0));
        self.offers = offers.into_iter().map(|(_, offer)| offer).collect();

        let mut transfers = Vec::with_capacity(self.transfer_plans.len());
        for transfer in self.transfer_plans.drain(..) {
            let transfer = verify_record(transfer)?;
            transfers.push((transfer.id()?, transfer));
        }
        transfers.sort_by(|left, right| left.0.cmp(&right.0));
        self.transfer_plans = transfers
            .into_iter()
            .map(|(_, transfer)| transfer)
            .collect();
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        verify_realization_set_v1(
            &self.contract,
            &self.interface,
            &self.descriptors,
            &self.realization_set,
        )?;
        self.graph.validate()?;
        self.objective.validate()?;
        if self.graph.operations.len() != 1 {
            return Err(invalid(
                Self::RECORD,
                "the v1 planner profile requires exactly one logical operation",
            ));
        }
        let operation = &self.graph.operations[0];
        if operation.interface != self.interface.id()? {
            return Err(invalid(
                Self::RECORD,
                "graph interface does not match the closure",
            ));
        }
        if operation.contract != self.contract.id()? {
            return Err(invalid(
                Self::RECORD,
                "graph contract does not match the closure",
            ));
        }
        if operation.realization_set != self.realization_set.id()? {
            return Err(invalid(
                Self::RECORD,
                "graph realization set does not match the closure",
            ));
        }
        if self.offers.len() > MAX_REALIZATION_PLAN_CANDIDATES_V1 {
            return Err(invalid(
                Self::RECORD,
                "candidate offer count exceeds the limit",
            ));
        }
        let mut previous_candidate = None;
        let mut conceptual_candidates = BTreeSet::new();
        for offer in &self.offers {
            offer.validate()?;
            let candidate = offer.candidate()?;
            if previous_candidate
                .as_ref()
                .is_some_and(|previous| previous >= &candidate)
            {
                return Err(invalid(
                    Self::RECORD,
                    "candidate tuple offers must be strictly ordered without duplicates",
                ));
            }
            previous_candidate = Some(candidate);
            if !conceptual_candidates.insert(offer.conceptual_key()?) {
                return Err(invalid(
                    Self::RECORD,
                    "multiple cost profiles were supplied for the same conceptual candidate tuple",
                ));
            }
        }

        if self.transfer_plans.len() > MAX_REALIZATION_PLAN_EDGES_V1 {
            return Err(invalid(
                Self::RECORD,
                "transfer plan count exceeds the limit",
            ));
        }
        if self.transfer_plans.len() != self.graph.edges.len() {
            return Err(invalid(
                Self::RECORD,
                "transfer plans must exactly cover the logical graph edges",
            ));
        }
        let graph_id = self.graph.id()?;
        let edge_ids = self
            .graph
            .edges
            .iter()
            .map(|edge| edge.id)
            .collect::<BTreeSet<_>>();
        let mut transferred_edges = BTreeSet::new();
        let mut previous_transfer = None;
        for transfer in &self.transfer_plans {
            transfer.validate()?;
            if transfer.logical_hgraph != graph_id {
                return Err(invalid(
                    Self::RECORD,
                    "transfer names a different logical graph",
                ));
            }
            if !edge_ids.contains(&transfer.edge) {
                return Err(invalid(
                    Self::RECORD,
                    "transfer names an unknown logical edge",
                ));
            }
            if !transferred_edges.insert(transfer.edge) {
                return Err(invalid(
                    Self::RECORD,
                    "multiple transfers name one logical edge",
                ));
            }
            let id = transfer.id()?;
            if previous_transfer
                .as_ref()
                .is_some_and(|previous| previous >= &id)
            {
                return Err(invalid(
                    Self::RECORD,
                    "transfer plans must be strictly ordered without duplicates",
                ));
            }
            previous_transfer = Some(id);
        }
        if transferred_edges != edge_ids {
            return Err(invalid(
                Self::RECORD,
                "transfer plans do not exactly cover the logical graph edges",
            ));
        }
        Ok(())
    }
}

record_api!(OperationPlanningRequestV1, OperationPlanningRequestIdV1);

fn footprint_content_ref(
    footprint: &RequirementFootprintV1,
) -> Result<SemanticArtifactRefV1, RealizationPlanErrorV1> {
    let bytes = footprint
        .canonical_bytes()
        .map_err(|error| RealizationPlanErrorV1::Placement(error.to_string()))?;
    Ok(SemanticArtifactRefV1::new(
        ComputationTokenV1::new(REQUIREMENT_FOOTPRINT_CONTENT_SCHEMA_V1)?,
        artifact_id_for_bytes(&bytes),
    )?)
}

fn push_reason(reasons: &mut Vec<PlanningReasonV1>, reason: PlanningReasonV1) {
    reasons.push(reason);
}

fn assess_representation_direction(
    direction: &'static str,
    interface_ports: &[crate::computation_core::OperationPortV1],
    descriptor: Option<&RealizationDescriptorV1>,
    offers: &[PortRepresentationOfferV1],
    reasons: &mut Vec<PlanningReasonV1>,
) -> Result<bool, RealizationPlanErrorV1> {
    let mut rejected = false;
    let bindings = descriptor.map(|descriptor| {
        if direction == "input" {
            descriptor.input_representations.as_slice()
        } else {
            descriptor.output_representations.as_slice()
        }
    });

    for port in interface_ports {
        let Some(offer) = offers.iter().find(|offer| offer.port == port.name) else {
            rejected = true;
            push_reason(
                reasons,
                if direction == "input" {
                    PlanningReasonV1::MissingInputRepresentation {
                        port: port.name.clone(),
                    }
                } else {
                    PlanningReasonV1::MissingOutputRepresentation {
                        port: port.name.clone(),
                    }
                },
            );
            continue;
        };
        if offer.representation.value_type != port.value_type {
            rejected = true;
            push_reason(
                reasons,
                if direction == "input" {
                    PlanningReasonV1::InputValueTypeMismatch {
                        port: port.name.clone(),
                    }
                } else {
                    PlanningReasonV1::OutputValueTypeMismatch {
                        port: port.name.clone(),
                    }
                },
            );
        }

        if let Some(bindings) = bindings {
            let declared = bindings
                .iter()
                .find(|binding| binding.port == port.name)
                .is_some_and(|binding| {
                    offer
                        .representation
                        .semantic_ref()
                        .ok()
                        .is_some_and(|reference| binding.representations.contains(&reference))
                });
            if !declared {
                rejected = true;
                push_reason(
                    reasons,
                    if direction == "input" {
                        PlanningReasonV1::InputRepresentationNotDeclared {
                            port: port.name.clone(),
                        }
                    } else {
                        PlanningReasonV1::OutputRepresentationNotDeclared {
                            port: port.name.clone(),
                        }
                    },
                );
            } else {
                let representation = offer.representation.id()?;
                push_reason(
                    reasons,
                    if direction == "input" {
                        PlanningReasonV1::InputRepresentationMatched {
                            port: port.name.clone(),
                            representation,
                        }
                    } else {
                        PlanningReasonV1::OutputRepresentationMatched {
                            port: port.name.clone(),
                            representation,
                        }
                    },
                );
            }
        }
    }

    for offer in offers {
        if !interface_ports.iter().any(|port| port.name == offer.port) {
            rejected = true;
            push_reason(
                reasons,
                if direction == "input" {
                    PlanningReasonV1::UnexpectedInputRepresentation {
                        port: offer.port.clone(),
                    }
                } else {
                    PlanningReasonV1::UnexpectedOutputRepresentation {
                        port: offer.port.clone(),
                    }
                },
            );
        }
    }
    Ok(rejected)
}

/// Rank the exact, caller-supplied tuple offers for the single logical
/// operation in `request`.  The result is deterministic and descriptive only:
/// this function cannot admit, reserve, dispatch, or execute work.
pub fn plan_operation_realization_v1(
    request: &OperationPlanningRequestV1,
) -> Result<DeploymentPlanV2, RealizationPlanErrorV1> {
    let request = verify_record(request.clone())?;
    let operation = &request.graph.operations[0];
    let interface_id = request.interface.id()?;
    let contract_id = request.contract.id()?;
    let descriptor_map = request
        .descriptors
        .iter()
        .map(|descriptor| Ok((descriptor.id()?, descriptor)))
        .collect::<Result<BTreeMap<_, _>, RealizationPlanErrorV1>>()?;

    let mut assessments = Vec::with_capacity(request.offers.len());
    for offer in &request.offers {
        let candidate = offer.candidate()?;
        let descriptor = descriptor_map.get(&offer.descriptor).copied();
        let mut reasons = Vec::new();
        let mut rejected = false;

        if offer.logical_operation != operation.id {
            rejected = true;
            push_reason(
                &mut reasons,
                PlanningReasonV1::OfferNamesDifferentLogicalOperation,
            );
        }
        if descriptor.is_none() {
            rejected = true;
            push_reason(
                &mut reasons,
                PlanningReasonV1::DescriptorOutsideRealizationSet,
            );
        }

        if let Some(descriptor) = descriptor {
            push_reason(&mut reasons, PlanningReasonV1::StateRequirementsDeferred);
            push_reason(&mut reasons, PlanningReasonV1::ActorRequirementsDeferred);
            if descriptor.supplied_fidelity == request.contract.required_fidelity {
                push_reason(&mut reasons, PlanningReasonV1::ExactFidelityCompatible);
            } else {
                rejected = true;
                push_reason(&mut reasons, PlanningReasonV1::ExactFidelityMismatch);
            }
            if descriptor.target_requirements == footprint_content_ref(&offer.target_requirements)?
            {
                push_reason(
                    &mut reasons,
                    PlanningReasonV1::TargetRequirementsContentMatched,
                );
            } else {
                rejected = true;
                push_reason(
                    &mut reasons,
                    PlanningReasonV1::TargetRequirementsContentMismatch,
                );
            }
        }

        match offer.target_requirements.require_complete() {
            Ok(atoms) => {
                for atom in atoms {
                    let requirement = atom.label();
                    match atom {
                        RequirementAtomV1::Environment(_)
                        | RequirementAtomV1::Effect(_)
                        | RequirementAtomV1::ResourceMinimum { .. } => {
                            rejected = true;
                            push_reason(
                                &mut reasons,
                                PlanningReasonV1::DynamicRequirementUndischarged { requirement },
                            );
                        }
                        _ => {
                            let supported =
                                offer.target.supports_requirement(atom).map_err(|error| {
                                    RealizationPlanErrorV1::Placement(error.to_string())
                                })?;
                            if supported {
                                push_reason(
                                    &mut reasons,
                                    PlanningReasonV1::StaticRequirementSatisfied { requirement },
                                );
                            } else {
                                rejected = true;
                                push_reason(
                                    &mut reasons,
                                    PlanningReasonV1::StaticRequirementUnsupported { requirement },
                                );
                            }
                        }
                    }
                }
            }
            Err(_) => {
                rejected = true;
                let state = if offer.target_requirements.is_unsatisfiable() {
                    RequirementFootprintIncompleteStateV1::Unsatisfiable
                } else {
                    RequirementFootprintIncompleteStateV1::ConservativeUnknown
                };
                let mut footprint_reasons = offer
                    .target_requirements
                    .reasons()
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>();
                footprint_reasons.sort();
                footprint_reasons.dedup();
                push_reason(
                    &mut reasons,
                    PlanningReasonV1::RequirementFootprintIncomplete {
                        state,
                        reasons: footprint_reasons,
                    },
                );
            }
        }

        rejected |= assess_representation_direction(
            "input",
            &request.interface.inputs,
            descriptor,
            &offer.inputs,
            &mut reasons,
        )?;
        rejected |= assess_representation_direction(
            "output",
            &request.interface.outputs,
            descriptor,
            &offer.outputs,
            &mut reasons,
        )?;

        let profile = &offer.cost_profile;
        let mut profile_mismatch = false;
        if profile.descriptor != offer.descriptor {
            rejected = true;
            profile_mismatch = true;
            push_reason(
                &mut reasons,
                PlanningReasonV1::CostProfileDescriptorMismatch,
            );
        }
        if descriptor.is_some_and(|descriptor| profile.realization != descriptor.realization) {
            rejected = true;
            profile_mismatch = true;
            push_reason(
                &mut reasons,
                PlanningReasonV1::CostProfileRealizationMismatch,
            );
        }
        if profile.interface != interface_id {
            rejected = true;
            profile_mismatch = true;
            push_reason(&mut reasons, PlanningReasonV1::CostProfileInterfaceMismatch);
        }
        if profile.contract != contract_id {
            rejected = true;
            profile_mismatch = true;
            push_reason(&mut reasons, PlanningReasonV1::CostProfileContractMismatch);
        }
        if profile.target != candidate.target {
            rejected = true;
            profile_mismatch = true;
            push_reason(&mut reasons, PlanningReasonV1::CostProfileTargetMismatch);
        }
        if profile.input_geometry != operation.input_geometry {
            rejected = true;
            profile_mismatch = true;
            push_reason(
                &mut reasons,
                PlanningReasonV1::CostProfileInputGeometryMismatch,
            );
        }
        if profile.inputs != candidate.inputs {
            rejected = true;
            profile_mismatch = true;
            push_reason(
                &mut reasons,
                PlanningReasonV1::CostProfileInputRepresentationsMismatch,
            );
        }
        if profile.outputs != candidate.outputs {
            rejected = true;
            profile_mismatch = true;
            push_reason(
                &mut reasons,
                PlanningReasonV1::CostProfileOutputRepresentationsMismatch,
            );
        }
        if !profile_mismatch {
            push_reason(
                &mut reasons,
                PlanningReasonV1::CostProfileCoordinatesMatched,
            );
        }

        let total = profile
            .checked_total_ns()
            .ok_or_else(|| invalid("CostProfileV1", "cost component sum overflows u64"))?;
        push_reason(
            &mut reasons,
            PlanningReasonV1::PredictedTotalNanoseconds { value: total },
        );
        match request.objective.maximum_total_ns {
            Some(maximum) if total > maximum => {
                rejected = true;
                push_reason(
                    &mut reasons,
                    PlanningReasonV1::ObjectiveMaximumExceeded {
                        maximum,
                        actual: total,
                    },
                );
            }
            Some(maximum) => push_reason(
                &mut reasons,
                PlanningReasonV1::ObjectiveMaximumSatisfied {
                    maximum,
                    actual: total,
                },
            ),
            _ => {}
        }

        let disposition = if rejected {
            CandidateDispositionV1::Rejected
        } else {
            push_reason(
                &mut reasons,
                PlanningReasonV1::StaticallyCompatibleForRanking,
            );
            CandidateDispositionV1::Rankable
        };
        reasons.sort();
        reasons.dedup();
        assessments.push(CandidateAssessmentV1 {
            candidate,
            disposition,
            predicted_total_ns: Some(total),
            reasons,
        });
    }
    assessments.sort_by(|left, right| left.candidate.cmp(&right.candidate));

    let mut selected_index = None;
    for (index, assessment) in assessments.iter().enumerate() {
        if assessment.disposition != CandidateDispositionV1::Rankable {
            continue;
        }
        let score = assessment
            .predicted_total_ns
            .expect("rankable assessment has a checked cost");
        let replace = selected_index.is_none_or(|best: usize| {
            let best_assessment = &assessments[best];
            let best_score = best_assessment
                .predicted_total_ns
                .expect("rankable assessment has a checked cost");
            (score, &assessment.candidate) < (best_score, &best_assessment.candidate)
        });
        if replace {
            selected_index = Some(index);
        }
    }

    let selection = selected_index.map(|index| assessments[index].candidate.clone());
    if let Some(index) = selected_index {
        let selected_total = assessments[index]
            .predicted_total_ns
            .expect("rankable assessment has a checked cost");
        for (candidate_index, assessment) in assessments.iter_mut().enumerate() {
            if assessment.disposition != CandidateDispositionV1::Rankable {
                continue;
            }
            if candidate_index == index {
                assessment.disposition = CandidateDispositionV1::Selected;
                push_reason(
                    &mut assessment.reasons,
                    PlanningReasonV1::SelectedByObjective,
                );
            } else {
                let candidate_total = assessment
                    .predicted_total_ns
                    .expect("rankable assessment has a checked cost");
                push_reason(
                    &mut assessment.reasons,
                    if candidate_total > selected_total {
                        PlanningReasonV1::NotSelectedHigherCost {
                            selected: selected_total,
                            candidate: candidate_total,
                        }
                    } else {
                        PlanningReasonV1::NotSelectedCanonicalTieBreak
                    },
                );
            }
            assessment.reasons.sort();
            assessment.reasons.dedup();
        }
    }

    let mut operation_reasons = Vec::new();
    if request.offers.is_empty() {
        operation_reasons.push(PlanningReasonV1::NoCandidateOffers);
    } else if selection.is_none() {
        operation_reasons.push(PlanningReasonV1::NoRankableCandidate);
    }
    let transfer_ids = request
        .transfer_plans
        .iter()
        .map(TransferPlanV1::id)
        .collect::<Result<Vec<_>, _>>()?;
    DeploymentPlanV2::new(
        request.graph.id()?,
        request.objective.id()?,
        vec![DeploymentOperationV2 {
            logical_operation: operation.id,
            selection,
            candidates: assessments,
            reasons: operation_reasons,
        }],
        vec![operation.id],
        transfer_ids,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeObservationStateV2 {
    Proposed,
    Started,
    Succeeded,
    Failed,
}

/// Optional measurements attached to one observation.  Absence means the
/// producer did not observe that metric; zero remains a distinct measurement.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMetricsV2 {
    pub bytes_transferred: Option<u64>,
    pub queue_ns: Option<u64>,
    pub startup_ns: Option<u64>,
    pub conversion_ns: Option<u64>,
    pub execution_ns: Option<u64>,
    pub checkpoint_ns: Option<u64>,
    pub elapsed_ns: Option<u64>,
    pub peak_memory_bytes: Option<u64>,
}

/// One caller-supplied runtime observation.  The record preserves a claim; it
/// neither authenticates that claim nor grants permission to perform the work.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeObservationV2 {
    pub ordinal: u64,
    pub logical_operation: LogicalOperationNodeIdV2,
    pub candidate: RealizationCandidateTupleV1,
    pub state: RuntimeObservationStateV2,
    pub metrics: RuntimeMetricsV2,
    pub observed_fidelity: Option<SemanticArtifactRefV1>,
    pub failure_classification: Option<SemanticArtifactRefV1>,
    /// Exact actor/process generation identity when the observer can bind it.
    /// A lifecycle may begin with `None` and acquire its first known generation
    /// later.  Once known, every subsequent observation for that candidate must
    /// repeat the same generation; reverting to `None` or changing it is invalid.
    pub actor_generation: Option<SemanticDigestV1>,
    pub evidence: Vec<SemanticArtifactRefV1>,
    pub detail: Option<String>,
}

/// Immutable observation graph over a descriptive deployment plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeGraphV2 {
    pub schema: String,
    pub logical_hgraph: LogicalHGraphIdV2,
    pub deployment_plan: DeploymentPlanIdV2,
    pub observations: Vec<RuntimeObservationV2>,
}

impl RuntimeGraphV2 {
    pub fn new(
        logical_hgraph: LogicalHGraphIdV2,
        deployment_plan: DeploymentPlanIdV2,
        observations: Vec<RuntimeObservationV2>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: RUNTIME_GRAPH_SCHEMA_V2.to_owned(),
            logical_hgraph,
            deployment_plan,
            observations,
        })
    }

    pub fn from_deployment(
        deployment: &DeploymentPlanV2,
        observations: Vec<RuntimeObservationV2>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        deployment.validate()?;
        let runtime = Self::new(
            deployment.logical_hgraph.clone(),
            deployment.id()?,
            observations,
        )?;
        runtime.verify_against(deployment)?;
        Ok(runtime)
    }

    /// Resolve this graph's otherwise detached IDs and candidate references
    /// against the supplied deployment plan.
    pub fn verify_against(
        &self,
        deployment: &DeploymentPlanV2,
    ) -> Result<(), RealizationPlanErrorV1> {
        self.validate()?;
        deployment.validate()?;
        if self.logical_hgraph != deployment.logical_hgraph {
            return Err(invalid(
                Self::RECORD,
                "runtime and deployment name different logical graphs",
            ));
        }
        if self.deployment_plan != deployment.id()? {
            return Err(invalid(
                Self::RECORD,
                "runtime deployment identity does not match the supplied plan",
            ));
        }
        let selected = deployment
            .operations
            .iter()
            .filter_map(|operation| {
                operation
                    .selection
                    .as_ref()
                    .map(|candidate| (operation.logical_operation, candidate))
            })
            .collect::<BTreeMap<_, _>>();
        for observation in &self.observations {
            if selected.get(&observation.logical_operation).copied() != Some(&observation.candidate)
            {
                return Err(invalid(
                    Self::RECORD,
                    "runtime observation does not name the deployment's selected candidate",
                ));
            }
        }
        Ok(())
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::RuntimeGraph)
    }
}

impl CanonicalRealizationPlanRecordV1 for RuntimeGraphV2 {
    const RECORD: &'static str = "RuntimeGraphV2";
    const SCHEMA: &'static str = RUNTIME_GRAPH_SCHEMA_V2;
    const DIGEST_DOMAIN: &'static [u8] = RUNTIME_GRAPH_DIGEST_DOMAIN_V2;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        for observation in &mut self.observations {
            canonicalize_candidate(&mut observation.candidate);
            observation.evidence.sort();
        }
        self.observations
            .sort_by_key(|observation| observation.ordinal);
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_artifact(
            Self::RECORD,
            "logical HGraph",
            self.logical_hgraph.artifact(),
        )?;
        validate_artifact(
            Self::RECORD,
            "deployment plan",
            self.deployment_plan.artifact(),
        )?;
        if self.observations.len() > MAX_REALIZATION_PLAN_EXPLANATIONS_V1 {
            return Err(invalid(Self::RECORD, "observation count exceeds the limit"));
        }
        let mut lifecycles = BTreeMap::<
            RealizationCandidateTupleV1,
            (RuntimeObservationStateV2, Option<SemanticDigestV1>),
        >::new();
        for (index, observation) in self.observations.iter().enumerate() {
            let expected = u64::try_from(index)
                .map_err(|_| invalid(Self::RECORD, "observation index does not fit u64"))?;
            if observation.ordinal != expected {
                return Err(invalid(
                    Self::RECORD,
                    format!("observation ordinals must be dense from zero; expected {expected}"),
                ));
            }
            if observation.logical_operation != observation.candidate.logical_operation {
                return Err(invalid(
                    Self::RECORD,
                    "observation operation does not match its candidate",
                ));
            }
            validate_candidate(Self::RECORD, &observation.candidate)?;
            match observation.state {
                RuntimeObservationStateV2::Failed
                    if observation.failure_classification.is_none() =>
                {
                    return Err(invalid(
                        Self::RECORD,
                        "failed observation omits failure classification",
                    ));
                }
                RuntimeObservationStateV2::Proposed
                | RuntimeObservationStateV2::Started
                | RuntimeObservationStateV2::Succeeded
                    if observation.failure_classification.is_some() =>
                {
                    return Err(invalid(
                        Self::RECORD,
                        "non-failed observation carries a failure classification",
                    ));
                }
                _ => {}
            }
            if let Some(fidelity) = &observation.observed_fidelity {
                validate_semantic_ref(Self::RECORD, "observed fidelity", fidelity)?;
            }
            if let Some(classification) = &observation.failure_classification {
                validate_semantic_ref(Self::RECORD, "failure classification", classification)?;
            }
            if let Some(generation) = &observation.actor_generation {
                validate_semantic_digest(Self::RECORD, "actor generation", generation)?;
            }
            if observation.evidence.len() > MAX_REALIZATION_PLAN_REFERENCES_V1 {
                return Err(invalid(
                    Self::RECORD,
                    "observation evidence exceeds the limit",
                ));
            }
            ensure_strict_order(Self::RECORD, "observation evidence", &observation.evidence)?;
            for evidence in &observation.evidence {
                validate_semantic_ref(Self::RECORD, "observation evidence", evidence)?;
            }
            if let Some(detail) = &observation.detail {
                validate_text(Self::RECORD, "observation detail", detail)?;
            }

            let previous = lifecycles.get(&observation.candidate);
            let previous_state = previous.map(|(state, _)| *state);
            if let Some(previous_generation) =
                previous.and_then(|(_, generation)| generation.as_ref())
            {
                if observation.actor_generation.as_ref() != Some(previous_generation) {
                    return Err(invalid(
                        Self::RECORD,
                        "known actor generation changed or became unknown within a candidate lifecycle",
                    ));
                }
            }
            let valid_transition = matches!(
                (previous_state, observation.state),
                (None, RuntimeObservationStateV2::Proposed)
                    | (None, RuntimeObservationStateV2::Succeeded)
                    | (None, RuntimeObservationStateV2::Failed)
                    | (
                        Some(RuntimeObservationStateV2::Proposed),
                        RuntimeObservationStateV2::Started
                    )
                    | (
                        Some(RuntimeObservationStateV2::Started),
                        RuntimeObservationStateV2::Succeeded | RuntimeObservationStateV2::Failed
                    )
            );
            if !valid_transition {
                return Err(invalid(
                    Self::RECORD,
                    "invalid runtime observation transition",
                ));
            }
            let known_generation = observation
                .actor_generation
                .clone()
                .or_else(|| previous.and_then(|(_, generation)| generation.clone()));
            lifecycles.insert(
                observation.candidate.clone(),
                (observation.state, known_generation),
            );
        }
        Ok(())
    }
}

record_api!(RuntimeGraphV2, RuntimeGraphIdV2);

/// A descriptive fallback tuple and the exact condition document that would
/// justify considering it.  No automatic retry or invocation is implied.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAlternativeV1 {
    pub candidate: RealizationCandidateTupleV1,
    pub condition: SemanticArtifactRefV1,
    pub checkpoint: Option<SemanticArtifactRefV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPlanV1 {
    pub schema: String,
    pub runtime_graph: RuntimeGraphIdV2,
    pub failed_operation: LogicalOperationNodeIdV2,
    pub failed_candidate: RealizationCandidateTupleV1,
    pub alternatives: Vec<RecoveryAlternativeV1>,
}

impl RecoveryPlanV1 {
    pub fn new(
        runtime_graph: RuntimeGraphIdV2,
        failed_operation: LogicalOperationNodeIdV2,
        failed_candidate: RealizationCandidateTupleV1,
        alternatives: Vec<RecoveryAlternativeV1>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        verify_record(Self {
            schema: RECOVERY_PLAN_SCHEMA_V1.to_owned(),
            runtime_graph,
            failed_operation,
            failed_candidate,
            alternatives,
        })
    }

    pub fn from_verified_runtime(
        runtime: &RuntimeGraphV2,
        deployment: &DeploymentPlanV2,
        failed_operation: LogicalOperationNodeIdV2,
        failed_candidate: RealizationCandidateTupleV1,
        alternatives: Vec<RecoveryAlternativeV1>,
    ) -> Result<Self, RealizationPlanErrorV1> {
        runtime.verify_against(deployment)?;
        let recovery = Self::new(
            runtime.id()?,
            failed_operation,
            failed_candidate,
            alternatives,
        )?;
        recovery.verify_against(runtime, deployment)?;
        Ok(recovery)
    }

    /// Resolve detached graph/plan references and confirm that the failed
    /// candidate was selected and observed failed, while each alternative was
    /// a non-rejected candidate in that same descriptive deployment.
    pub fn verify_against(
        &self,
        runtime: &RuntimeGraphV2,
        deployment: &DeploymentPlanV2,
    ) -> Result<(), RealizationPlanErrorV1> {
        self.validate()?;
        runtime.verify_against(deployment)?;
        if self.runtime_graph != runtime.id()? {
            return Err(invalid(
                Self::RECORD,
                "recovery runtime identity does not match the supplied runtime graph",
            ));
        }
        let selected = deployment
            .operations
            .iter()
            .find(|operation| operation.logical_operation == self.failed_operation)
            .and_then(|operation| operation.selection.as_ref());
        if selected != Some(&self.failed_candidate) {
            return Err(invalid(
                Self::RECORD,
                "failed candidate was not selected by the supplied deployment",
            ));
        }
        if !runtime.observations.iter().any(|observation| {
            observation.candidate == self.failed_candidate
                && observation.state == RuntimeObservationStateV2::Failed
        }) {
            return Err(invalid(
                Self::RECORD,
                "failed candidate has no failed runtime observation",
            ));
        }
        let assessments = deployment
            .operations
            .iter()
            .flat_map(|operation| operation.candidates.iter())
            .collect::<Vec<_>>();
        for alternative in &self.alternatives {
            if !assessments.iter().any(|assessment| {
                assessment.candidate == alternative.candidate
                    && assessment.disposition != CandidateDispositionV1::Rejected
            }) {
                return Err(invalid(
                    Self::RECORD,
                    "recovery alternative was not a non-rejected deployment candidate",
                ));
            }
        }
        Ok(())
    }

    pub fn facet_ref(&self, id: FacetIdV1) -> Result<FacetRefV1, RealizationPlanErrorV1> {
        record_facet_ref(self, id, FacetKindV1::RecoveryPlan)
    }
}

impl CanonicalRealizationPlanRecordV1 for RecoveryPlanV1 {
    const RECORD: &'static str = "RecoveryPlanV1";
    const SCHEMA: &'static str = RECOVERY_PLAN_SCHEMA_V1;
    const DIGEST_DOMAIN: &'static [u8] = RECOVERY_PLAN_DIGEST_DOMAIN_V1;

    fn schema(&self) -> &str {
        &self.schema
    }

    fn canonicalize(&mut self) -> Result<(), RealizationPlanErrorV1> {
        canonicalize_candidate(&mut self.failed_candidate);
        for alternative in &mut self.alternatives {
            canonicalize_candidate(&mut alternative.candidate);
        }
        Ok(())
    }

    fn validate_body(&self) -> Result<(), RealizationPlanErrorV1> {
        validate_artifact(Self::RECORD, "runtime graph", self.runtime_graph.artifact())?;
        validate_candidate(Self::RECORD, &self.failed_candidate)?;
        if self.failed_candidate.logical_operation != self.failed_operation {
            return Err(invalid(
                Self::RECORD,
                "failed operation does not match the failed candidate",
            ));
        }
        if self.alternatives.is_empty() {
            return Err(invalid(Self::RECORD, "recovery plan has no alternatives"));
        }
        if self.alternatives.len() > MAX_REALIZATION_PLAN_CANDIDATES_V1 {
            return Err(invalid(
                Self::RECORD,
                "recovery alternative count exceeds the limit",
            ));
        }
        let failed_key = conceptual_candidate_key(&self.failed_candidate);
        let mut alternatives = BTreeSet::new();
        for alternative in &self.alternatives {
            validate_candidate(Self::RECORD, &alternative.candidate)?;
            if alternative.candidate.logical_operation != self.failed_operation {
                return Err(invalid(
                    Self::RECORD,
                    "recovery alternative names a different logical operation",
                ));
            }
            let key = conceptual_candidate_key(&alternative.candidate);
            if key == failed_key {
                return Err(invalid(
                    Self::RECORD,
                    "recovery alternative repeats the failed conceptual tuple",
                ));
            }
            if !alternatives.insert(key) {
                return Err(invalid(
                    Self::RECORD,
                    "recovery alternatives repeat a conceptual tuple",
                ));
            }
            validate_semantic_ref(Self::RECORD, "recovery condition", &alternative.condition)?;
            if let Some(checkpoint) = &alternative.checkpoint {
                validate_semantic_ref(Self::RECORD, "recovery checkpoint", checkpoint)?;
            }
        }
        Ok(())
    }
}

record_api!(RecoveryPlanV1, RecoveryPlanIdV1);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computation_core::{
        OperationIdV1, OperationPortV1, RealizationPortRepresentationsV1,
    };
    use crate::placement::{
        CapabilityAtomV1, EndiannessV1, GenerationV1, PlatformDescriptorV1, ResourceKindV1,
        TargetCapabilityModelV1,
    };

    fn token(value: &str) -> ComputationTokenV1 {
        ComputationTokenV1::new(value).unwrap()
    }

    fn reference(value: &str) -> SemanticArtifactRefV1 {
        SemanticArtifactRefV1::new(
            token(&format!("test/{value}/v1")),
            artifact_id_for_bytes(value.as_bytes()),
        )
        .unwrap()
    }

    fn target() -> TargetDescriptorV1 {
        TargetDescriptorV1::new(
            "local:python",
            "Local Python",
            GenerationV1::new(1).unwrap(),
            TargetCapabilityModelV1::DownwardClosedIdeal,
            PlatformDescriptorV1::new("macos", "aarch64", "darwin", EndiannessV1::Little, 64)
                .unwrap(),
            Vec::<CapabilityAtomV1>::new(),
            Vec::<String>::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn representation(name: &str, value_type: &SemanticArtifactRefV1) -> PhysicalRepresentationV1 {
        PhysicalRepresentationV1::new(
            token(name),
            value_type.clone(),
            reference(&format!("{name}-format")),
            PhysicalStorageV1::HostMemory,
            PhysicalOwnershipV1::Owned,
            false,
        )
        .unwrap()
    }

    fn request_with_costs(scalar_cost: u64, chunked_cost: u64) -> OperationPlanningRequestV1 {
        let fidelity = reference("exact-fidelity");
        let contract = OperationContractV1::new(
            OperationIdV1::new("tensor/normalize").unwrap(),
            1,
            reference("preconditions"),
            reference("postconditions"),
            reference("state-model"),
            reference("effect-model"),
            reference("ordering"),
            reference("determinism"),
            fidelity.clone(),
        )
        .unwrap();
        let value_type = reference("f64-vector");
        let interface = OperationInterfaceV1::new(
            contract.operation.clone(),
            contract.semantic_version,
            contract.id().unwrap(),
            vec![],
            vec![OperationPortV1::new(token("input"), value_type.clone()).unwrap()],
            vec![OperationPortV1::new(token("output"), value_type.clone()).unwrap()],
        )
        .unwrap();
        let footprint =
            RequirementFootprintV1::complete([RequirementAtomV1::architecture("aarch64").unwrap()]);
        let footprint_ref = footprint_content_ref(&footprint).unwrap();
        let scalar_representation = representation("python-scalar-f64", &value_type);
        let chunked_representation = representation("python-chunked-f64", &value_type);

        let make_descriptor =
            |name: &str, implementation: &str, representation: &PhysicalRepresentationV1| {
                RealizationDescriptorV1::new(
                    RealizationIdV1::new(name).unwrap(),
                    interface.id().unwrap(),
                    contract.id().unwrap(),
                    artifact_id_for_bytes(implementation.as_bytes()),
                    reference("local-python-pipeline"),
                    vec![RealizationPortRepresentationsV1::new(
                        token("input"),
                        vec![representation.semantic_ref().unwrap()],
                    )
                    .unwrap()],
                    vec![RealizationPortRepresentationsV1::new(
                        token("output"),
                        vec![representation.semantic_ref().unwrap()],
                    )
                    .unwrap()],
                    footprint_ref.clone(),
                    reference("state-requirements-true"),
                    reference("actor-requirements-true"),
                    fidelity.clone(),
                    None,
                    vec![reference(&format!("{name}-validation"))],
                )
                .unwrap()
            };
        let scalar = make_descriptor(
            "python/scalar",
            "normalize-scalar.py",
            &scalar_representation,
        );
        let chunked = make_descriptor(
            "python/chunked",
            "normalize-chunked.py",
            &chunked_representation,
        );
        let descriptors = vec![scalar, chunked];
        let realization_set = RealizationSetV1::new(
            interface.id().unwrap(),
            contract.id().unwrap(),
            descriptors
                .iter()
                .map(|descriptor| descriptor.id().unwrap())
                .collect(),
        )
        .unwrap();
        let geometry = reference("geometry-4096-elements");
        let graph = LogicalHGraphV2::new(
            vec![LogicalOperationNodeV2 {
                id: LogicalOperationNodeIdV2(0),
                interface: interface.id().unwrap(),
                contract: contract.id().unwrap(),
                realization_set: realization_set.id().unwrap(),
                input_geometry: geometry.clone(),
            }],
            vec![],
            vec![LogicalOperationNodeIdV2(0)],
        )
        .unwrap();
        let objective =
            ObjectiveV1::new_minimize_predicted_total_ns(reference("objective-rules"), None)
                .unwrap();
        let target = target();
        let target_digest = target.semantic_digest().unwrap();

        let make_offer = |descriptor: &RealizationDescriptorV1,
                          representation: PhysicalRepresentationV1,
                          compute_ns: u64| {
            let residency = ValueResidencyV1::Portable;
            let selections = |port: &str| {
                vec![PortRepresentationSelectionV1 {
                    port: token(port),
                    representation: representation.id().unwrap(),
                    residency: residency.clone(),
                }]
            };
            let profile = CostProfileV1::new(
                descriptor.id().unwrap(),
                descriptor.realization.clone(),
                interface.id().unwrap(),
                contract.id().unwrap(),
                target_digest.clone(),
                geometry.clone(),
                selections("input"),
                selections("output"),
                CostComponentsV1 {
                    compute_ns,
                    startup_ns: 5,
                    conversion_ns: 0,
                    transfer_ns: 0,
                    queue_ns: 0,
                    checkpoint_ns: 0,
                },
                1,
                4,
                vec![reference("benchmark-receipt")],
            )
            .unwrap();
            CandidateTupleOfferV1::new(
                LogicalOperationNodeIdV2(0),
                descriptor.id().unwrap(),
                target.clone(),
                footprint.clone(),
                vec![PortRepresentationOfferV1 {
                    port: token("input"),
                    representation: representation.clone(),
                    residency: residency.clone(),
                }],
                vec![PortRepresentationOfferV1 {
                    port: token("output"),
                    representation,
                    residency,
                }],
                profile,
            )
            .unwrap()
        };
        let offers = vec![
            make_offer(&descriptors[0], scalar_representation, scalar_cost),
            make_offer(&descriptors[1], chunked_representation, chunked_cost),
        ];
        OperationPlanningRequestV1::new(
            graph,
            contract,
            interface,
            descriptors,
            realization_set,
            objective,
            offers,
            vec![],
        )
        .unwrap()
    }

    fn request_with_maximum(
        scalar_cost: u64,
        chunked_cost: u64,
        maximum: u64,
    ) -> OperationPlanningRequestV1 {
        let request = request_with_costs(scalar_cost, chunked_cost);
        let objective = ObjectiveV1::new_minimize_predicted_total_ns(
            request.objective.ruleset.clone(),
            Some(maximum),
        )
        .unwrap();
        let OperationPlanningRequestV1 {
            graph,
            contract,
            interface,
            descriptors,
            realization_set,
            offers,
            transfer_plans,
            ..
        } = request;
        OperationPlanningRequestV1::new(
            graph,
            contract,
            interface,
            descriptors,
            realization_set,
            objective,
            offers,
            transfer_plans,
        )
        .unwrap()
    }

    fn failed_runtime(plan: &DeploymentPlanV2) -> (RuntimeGraphV2, RealizationCandidateTupleV1) {
        let selected = plan.selected_candidate().unwrap().clone();
        let runtime = RuntimeGraphV2::from_deployment(
            plan,
            vec![RuntimeObservationV2 {
                ordinal: 0,
                logical_operation: selected.logical_operation,
                candidate: selected.clone(),
                state: RuntimeObservationStateV2::Failed,
                metrics: RuntimeMetricsV2 {
                    execution_ns: Some(41),
                    elapsed_ns: Some(47),
                    ..RuntimeMetricsV2::default()
                },
                observed_fidelity: None,
                failure_classification: Some(reference("route-nonzero-exit")),
                actor_generation: Some(SemanticDigestV1::hash_bytes(
                    "test/actor-generation/v1",
                    b"actor-1",
                )),
                evidence: vec![reference("run-record")],
                detail: Some("route returned a nonzero status".to_owned()),
            }],
        )
        .unwrap();
        (runtime, selected)
    }

    #[test]
    fn planner_selects_checked_minimum_and_binds_schedule_and_residency() {
        let request = request_with_costs(100, 50);
        let plan = plan_operation_realization_v1(&request).unwrap();
        let selected = plan.selected_candidate().unwrap();
        assert_eq!(selected.realization.as_str(), "python/chunked");
        assert_eq!(selected.target_node, "local:python");
        assert_eq!(selected.target_display_name, "Local Python");
        assert_eq!(selected.inputs[0].residency, ValueResidencyV1::Portable);
        assert_eq!(plan.schedule, [LogicalOperationNodeIdV2(0)]);
        assert!(plan.operations[0].candidates.iter().all(|assessment| {
            assessment.predicted_total_ns.is_some() && !assessment.reasons.is_empty()
        }));
        assert!(plan.operations[0]
            .candidates
            .iter()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Selected)
            .unwrap()
            .reasons
            .contains(&PlanningReasonV1::SelectedByObjective));
    }

    #[test]
    fn candidate_order_and_equal_cost_tie_break_are_deterministic() {
        let request = request_with_costs(50, 50);
        let first = plan_operation_realization_v1(&request).unwrap();
        let expected = request
            .offers
            .iter()
            .map(CandidateTupleOfferV1::candidate)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .min()
            .unwrap();
        assert_eq!(first.selected_candidate(), Some(&expected));

        let mut reversed = request.clone();
        reversed.offers.reverse();
        let second = plan_operation_realization_v1(&reversed).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            request.canonical_json().unwrap(),
            reversed.canonical_json().unwrap()
        );
    }

    #[test]
    fn wrong_operation_offers_are_preserved_as_rejected_assessments() {
        let mut request = request_with_costs(100, 50);
        let wrong_descriptor = request.offers[0].descriptor.clone();
        request.offers[0].logical_operation = LogicalOperationNodeIdV2(7);

        let plan = plan_operation_realization_v1(&request).unwrap();
        let wrong = plan.operations[0]
            .candidates
            .iter()
            .find(|assessment| assessment.candidate.descriptor == wrong_descriptor)
            .unwrap();
        assert_eq!(
            wrong.candidate.logical_operation,
            LogicalOperationNodeIdV2(7)
        );
        assert_eq!(wrong.disposition, CandidateDispositionV1::Rejected);
        assert!(wrong
            .reasons
            .contains(&PlanningReasonV1::OfferNamesDifferentLogicalOperation));
        assert!(plan.selected_candidate().is_some());
        plan.verify_against(&request).unwrap();

        let mut missing_reason = plan.clone();
        missing_reason.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| {
                assessment.candidate.logical_operation == LogicalOperationNodeIdV2(7)
            })
            .unwrap()
            .reasons
            .retain(|reason| *reason != PlanningReasonV1::OfferNamesDifferentLogicalOperation);
        assert!(missing_reason
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must agree exactly"));

        let mut spurious_reason = plan.clone();
        spurious_reason.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| {
                assessment.candidate.logical_operation == LogicalOperationNodeIdV2(0)
            })
            .unwrap()
            .reasons
            .push(PlanningReasonV1::OfferNamesDifferentLogicalOperation);
        assert!(spurious_reason
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must agree exactly"));

        let mut selected_mismatch = plan;
        let operation = &mut selected_mismatch.operations[0];
        let selected_candidate = {
            let selected = operation
                .candidates
                .iter_mut()
                .find(|assessment| assessment.disposition == CandidateDispositionV1::Selected)
                .unwrap();
            selected.candidate.logical_operation = LogicalOperationNodeIdV2(9);
            selected
                .reasons
                .push(PlanningReasonV1::OfferNamesDifferentLogicalOperation);
            selected.candidate.clone()
        };
        operation.selection = Some(selected_candidate);
        assert!(selected_mismatch
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must be rejected"));
    }

    #[test]
    fn assessment_dispositions_reject_cross_class_reason_injection() {
        let request = request_with_costs(100, 50);
        let plan = plan_operation_realization_v1(&request).unwrap();

        let mut rankable_with_rejection = plan.clone();
        rankable_with_rejection.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Rankable)
            .unwrap()
            .reasons
            .push(PlanningReasonV1::ExactFidelityMismatch);
        assert!(rankable_with_rejection.validate().is_err());

        let mut selected_with_rejection = plan;
        selected_with_rejection.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Selected)
            .unwrap()
            .reasons
            .push(PlanningReasonV1::CostProfileTargetMismatch);
        assert!(selected_with_rejection.validate().is_err());

        let rejected_plan =
            plan_operation_realization_v1(&request_with_maximum(100, 50, 1)).unwrap();
        let mut rejected_without_cause = rejected_plan.clone();
        rejected_without_cause.operations[0].candidates[0]
            .reasons
            .retain(|reason| !is_rejection_cause(reason));
        assert!(rejected_without_cause.validate().is_err());

        let mut rejected_with_rank_marker = rejected_plan;
        rejected_with_rank_marker.operations[0].candidates[0]
            .reasons
            .push(PlanningReasonV1::NotSelectedCanonicalTieBreak);
        assert!(rejected_with_rank_marker.validate().is_err());
    }

    #[test]
    fn deployment_rejects_forged_winner_reasons_duplicates_and_summaries() {
        let request = request_with_costs(100, 50);
        let plan = plan_operation_realization_v1(&request).unwrap();

        let mut higher_cost_winner = plan.clone();
        let operation = &mut higher_cost_winner.operations[0];
        let mut forged_selection = None;
        for assessment in &mut operation.candidates {
            if assessment.disposition == CandidateDispositionV1::Selected {
                assessment.disposition = CandidateDispositionV1::Rankable;
                assessment
                    .reasons
                    .retain(|reason| *reason != PlanningReasonV1::SelectedByObjective);
                assessment
                    .reasons
                    .push(PlanningReasonV1::NotSelectedCanonicalTieBreak);
            } else if assessment.disposition == CandidateDispositionV1::Rankable {
                assessment.disposition = CandidateDispositionV1::Selected;
                assessment
                    .reasons
                    .retain(|reason| !is_nonselection_marker(reason));
                assessment
                    .reasons
                    .push(PlanningReasonV1::SelectedByObjective);
                forged_selection = Some(assessment.candidate.clone());
            }
        }
        operation.selection = forged_selection;
        assert!(higher_cost_winner.validate().is_err());

        let mut wrong_cost_numbers = plan.clone();
        let rankable = wrong_cost_numbers.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Rankable)
            .unwrap();
        let reason = rankable
            .reasons
            .iter_mut()
            .find(|reason| is_nonselection_marker(reason))
            .unwrap();
        if let PlanningReasonV1::NotSelectedHigherCost { selected, .. } = reason {
            *selected = selected.checked_add(1).unwrap();
        } else {
            panic!("unequal fixture costs must produce a higher-cost reason");
        }
        assert!(wrong_cost_numbers.validate().is_err());

        let mut wrong_reason_choice = plan.clone();
        let rankable = wrong_reason_choice.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Rankable)
            .unwrap();
        rankable
            .reasons
            .retain(|reason| !is_nonselection_marker(reason));
        rankable
            .reasons
            .push(PlanningReasonV1::NotSelectedCanonicalTieBreak);
        assert!(wrong_reason_choice.validate().is_err());

        let mut conceptual_duplicate = plan.clone();
        let mut duplicate = conceptual_duplicate.operations[0]
            .candidates
            .iter()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Rankable)
            .unwrap()
            .clone();
        duplicate.candidate.cost_profile =
            CostProfileIdV1::from_artifact(artifact_id_for_bytes(b"alternate-profile")).unwrap();
        conceptual_duplicate.operations[0]
            .candidates
            .push(duplicate);
        assert!(conceptual_duplicate
            .validate()
            .unwrap_err()
            .to_string()
            .contains("conceptual tuple"));

        let mut false_summary = plan;
        false_summary.operations[0]
            .reasons
            .push(PlanningReasonV1::NoRankableCandidate);
        assert!(false_summary.validate().is_err());
    }

    #[test]
    fn deployment_request_closure_rejects_an_intrinsically_coherent_forgery() {
        let request = request_with_costs(100, 50);
        let mut forged = plan_operation_realization_v1(&request).unwrap();
        let operation = &mut forged.operations[0];
        let mut forged_selection = None;
        for assessment in &mut operation.candidates {
            if assessment.disposition == CandidateDispositionV1::Selected {
                assessment.disposition = CandidateDispositionV1::Rejected;
                assessment.reasons.retain(|reason| {
                    *reason != PlanningReasonV1::SelectedByObjective
                        && *reason != PlanningReasonV1::StaticallyCompatibleForRanking
                        && *reason != PlanningReasonV1::ExactFidelityCompatible
                });
                assessment
                    .reasons
                    .push(PlanningReasonV1::ExactFidelityMismatch);
            } else if assessment.disposition == CandidateDispositionV1::Rankable {
                assessment.disposition = CandidateDispositionV1::Selected;
                assessment
                    .reasons
                    .retain(|reason| !is_nonselection_marker(reason));
                assessment
                    .reasons
                    .push(PlanningReasonV1::SelectedByObjective);
                forged_selection = Some(assessment.candidate.clone());
            }
        }
        operation.selection = forged_selection;
        forged.validate().unwrap();
        assert!(forged.verify_against(&request).is_err());
        plan_operation_realization_v1(&request)
            .unwrap()
            .verify_against(&request)
            .unwrap();
    }

    #[test]
    fn objective_maximum_reason_arithmetic_and_exclusivity_are_enforced() {
        let plan = plan_operation_realization_v1(&request_with_maximum(100, 50, 75)).unwrap();

        let mut false_satisfied = plan.clone();
        let selected = false_satisfied.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Selected)
            .unwrap();
        let satisfied = selected
            .reasons
            .iter_mut()
            .find_map(|reason| match reason {
                PlanningReasonV1::ObjectiveMaximumSatisfied { maximum, actual } => {
                    Some((maximum, actual))
                }
                _ => None,
            })
            .unwrap();
        let actual = *satisfied.1;
        *satisfied.0 = actual.saturating_sub(1);
        assert!(false_satisfied.validate().is_err());

        let mut false_exceeded = plan.clone();
        let rejected = false_exceeded.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Rejected)
            .unwrap();
        let exceeded = rejected
            .reasons
            .iter_mut()
            .find_map(|reason| match reason {
                PlanningReasonV1::ObjectiveMaximumExceeded { maximum, actual } => {
                    Some((maximum, actual))
                }
                _ => None,
            })
            .unwrap();
        let actual = *exceeded.1;
        *exceeded.0 = actual;
        assert!(false_exceeded.validate().is_err());

        let mut both = plan;
        let selected = both.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Selected)
            .unwrap();
        let total = selected.predicted_total_ns.unwrap();
        selected
            .reasons
            .push(PlanningReasonV1::ObjectiveMaximumExceeded {
                maximum: total - 1,
                actual: total,
            });
        assert!(both.validate().is_err());
    }

    #[test]
    fn deployment_requires_one_global_objective_maximum_policy() {
        let plan = plan_operation_realization_v1(&request_with_maximum(100, 50, 75)).unwrap();

        let mut different_maximum = plan.clone();
        let exceeded = different_maximum.operations[0]
            .candidates
            .iter_mut()
            .find_map(|assessment| {
                assessment
                    .reasons
                    .iter_mut()
                    .find_map(|reason| match reason {
                        PlanningReasonV1::ObjectiveMaximumExceeded { maximum, actual } => {
                            Some((maximum, *actual))
                        }
                        _ => None,
                    })
            })
            .unwrap();
        *exceeded.0 = exceeded.1 - 1;
        assert!(different_maximum
            .validate()
            .unwrap_err()
            .to_string()
            .contains("one objective-maximum policy"));

        let mut missing_maximum = plan;
        missing_maximum.operations[0]
            .candidates
            .iter_mut()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Selected)
            .unwrap()
            .reasons
            .retain(|reason| !matches!(reason, PlanningReasonV1::ObjectiveMaximumSatisfied { .. }));
        assert!(missing_maximum
            .validate()
            .unwrap_err()
            .to_string()
            .contains("one objective-maximum policy"));
    }

    #[test]
    fn detached_assessments_reject_intrinsically_contradictory_facts() {
        let plan = plan_operation_realization_v1(&request_with_maximum(100, 50, 1)).unwrap();

        let rejects_added_reason = |reason: PlanningReasonV1| {
            let mut forged = plan.clone();
            forged.operations[0].candidates[0].reasons.push(reason);
            assert!(forged.validate().is_err());
        };
        rejects_added_reason(PlanningReasonV1::ExactFidelityMismatch);
        rejects_added_reason(PlanningReasonV1::TargetRequirementsContentMismatch);
        rejects_added_reason(PlanningReasonV1::CostProfileTargetMismatch);
        rejects_added_reason(PlanningReasonV1::DescriptorOutsideRealizationSet);

        let requirement = plan.operations[0].candidates[0]
            .reasons
            .iter()
            .find_map(|reason| match reason {
                PlanningReasonV1::StaticRequirementSatisfied { requirement } => {
                    Some(requirement.clone())
                }
                _ => None,
            })
            .unwrap();
        rejects_added_reason(PlanningReasonV1::StaticRequirementUnsupported { requirement });
        rejects_added_reason(PlanningReasonV1::RequirementFootprintIncomplete {
            state: RequirementFootprintIncompleteStateV1::ConservativeUnknown,
            reasons: vec!["analysis incomplete".to_owned()],
        });
    }

    #[test]
    fn footprint_failure_state_is_a_closed_wire_enum() {
        let reason = PlanningReasonV1::RequirementFootprintIncomplete {
            state: RequirementFootprintIncompleteStateV1::ConservativeUnknown,
            reasons: vec!["analysis incomplete".to_owned()],
        };
        assert_eq!(
            serde_json::to_value(&reason).unwrap()["state"],
            "conservative_unknown"
        );
        assert!(serde_json::from_str::<PlanningReasonV1>(
            r#"{"kind":"requirement_footprint_incomplete","state":"invented","reasons":["x"]}"#,
        )
        .is_err());
    }

    #[test]
    fn target_checks_fail_closed_and_preserve_all_rejection_causes() {
        let mut dynamic = request_with_costs(100, 50);
        let changed_descriptor = dynamic.offers[0].descriptor.clone();
        dynamic.offers[0].target_requirements =
            RequirementFootprintV1::complete([RequirementAtomV1::resource_minimum(
                ResourceKindV1::MemoryBytes,
                1,
            )
            .unwrap()]);
        let dynamic_plan = plan_operation_realization_v1(&dynamic).unwrap();
        let dynamic_assessment = dynamic_plan.operations[0]
            .candidates
            .iter()
            .find(|assessment| assessment.candidate.descriptor == changed_descriptor)
            .unwrap();
        assert_eq!(
            dynamic_assessment.disposition,
            CandidateDispositionV1::Rejected
        );
        assert!(dynamic_assessment.reasons.iter().any(|reason| matches!(
            reason,
            PlanningReasonV1::DynamicRequirementUndischarged { .. }
        )));
        assert!(dynamic_assessment
            .reasons
            .contains(&PlanningReasonV1::TargetRequirementsContentMismatch));

        let mut incomplete = request_with_costs(100, 50);
        incomplete.offers[0].target_requirements = RequirementFootprintV1::conservative_unknown(
            [],
            ["shape analysis incomplete".to_owned()],
        )
        .unwrap();
        let incomplete_plan = plan_operation_realization_v1(&incomplete).unwrap();
        assert!(incomplete_plan.operations[0]
            .candidates
            .iter()
            .any(
                |assessment| assessment.reasons.iter().any(|reason| matches!(
                    reason,
                    PlanningReasonV1::RequirementFootprintIncomplete { .. }
                ))
            ));

        let mut unsupported = request_with_costs(100, 50);
        unsupported.offers[0].target_requirements =
            RequirementFootprintV1::complete([RequirementAtomV1::architecture("x86_64").unwrap()]);
        let unsupported_plan = plan_operation_realization_v1(&unsupported).unwrap();
        assert!(unsupported_plan.operations[0]
            .candidates
            .iter()
            .any(
                |assessment| assessment.reasons.iter().any(|reason| matches!(
                    reason,
                    PlanningReasonV1::StaticRequirementUnsupported { .. }
                ))
            ));
    }

    #[test]
    fn representation_and_profile_content_are_matched_exactly() {
        let mut request = request_with_costs(100, 50);
        let descriptor = request.offers[0].descriptor.clone();
        request.offers[0].inputs[0].representation.format = reference("undeclared-format");
        request.offers[0].cost_profile.target =
            SemanticDigestV1::hash_bytes("test/different-target/v1", b"different-target");
        let plan = plan_operation_realization_v1(&request).unwrap();
        let assessment = plan.operations[0]
            .candidates
            .iter()
            .find(|assessment| assessment.candidate.descriptor == descriptor)
            .unwrap();
        assert_eq!(assessment.disposition, CandidateDispositionV1::Rejected);
        assert!(assessment
            .reasons
            .contains(&PlanningReasonV1::InputRepresentationNotDeclared {
                port: token("input")
            }));
        assert!(assessment
            .reasons
            .contains(&PlanningReasonV1::CostProfileTargetMismatch));
        assert!(assessment
            .reasons
            .contains(&PlanningReasonV1::CostProfileInputRepresentationsMismatch));
    }

    #[test]
    fn checked_costs_and_conceptual_tuple_uniqueness_reject_ambiguity() {
        let request = request_with_costs(100, 50);
        let profile = &request.offers[0].cost_profile;
        let error = CostProfileV1::new(
            profile.descriptor.clone(),
            profile.realization.clone(),
            profile.interface.clone(),
            profile.contract.clone(),
            profile.target.clone(),
            profile.input_geometry.clone(),
            profile.inputs.clone(),
            profile.outputs.clone(),
            CostComponentsV1 {
                compute_ns: u64::MAX,
                ..CostComponentsV1::default()
            },
            1,
            1,
            vec![],
        )
        .unwrap_err();
        assert!(error.to_string().contains("overflows"));

        let mut duplicate = request.clone();
        let mut second = duplicate.offers[0].clone();
        second.cost_profile.components.compute_ns += 1;
        duplicate.offers.push(second);
        assert!(OperationPlanningRequestV1::new(
            duplicate.graph,
            duplicate.contract,
            duplicate.interface,
            duplicate.descriptors,
            duplicate.realization_set,
            duplicate.objective,
            duplicate.offers,
            duplicate.transfer_plans,
        )
        .is_err());
    }

    #[test]
    fn canonical_round_trips_reject_unknown_fields_unsorted_bytes_and_zero_ids() {
        let request = request_with_costs(100, 50);
        let bytes = request.canonical_bytes().unwrap();
        assert_eq!(
            OperationPlanningRequestV1::decode_canonical(&bytes).unwrap(),
            request
        );
        assert_eq!(
            OperationPlanningRequestV1::decode_json(&request.canonical_json().unwrap()).unwrap(),
            request
        );

        let mut unsorted = request.clone();
        unsorted.offers.reverse();
        assert!(matches!(
            OperationPlanningRequestV1::decode_canonical(&encode(&unsorted).unwrap()).unwrap_err(),
            RealizationPlanErrorV1::NonCanonicalEncoding {
                record: "OperationPlanningRequestV1"
            }
        ));

        let mut json = serde_json::to_value(&request).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("authority".to_owned(), serde_json::Value::Bool(true));
        assert!(
            OperationPlanningRequestV1::decode_json(&serde_json::to_vec(&json).unwrap()).is_err()
        );
        let zero = format!("\"{}\"", "0".repeat(64));
        assert!(serde_json::from_str::<PhysicalRepresentationIdV1>(&zero).is_err());
        assert!(serde_json::from_str::<RuntimeGraphIdV2>(&zero).is_err());
    }

    #[test]
    fn logical_graph_accepts_general_dag_and_rejects_cycles_and_shared_consumers() {
        let request = request_with_costs(100, 50);
        let node = request.graph.operations[0].clone();
        let mut second = node.clone();
        second.id = LogicalOperationNodeIdV2(1);
        let edge = |id, producer, consumer, consumer_port: &str| LogicalEdgeV2 {
            id: LogicalEdgeIdV2(id),
            producer: LogicalEdgeEndpointV2 {
                operation: LogicalOperationNodeIdV2(producer),
                port: token("output"),
            },
            consumer: LogicalEdgeEndpointV2 {
                operation: LogicalOperationNodeIdV2(consumer),
                port: token(consumer_port),
            },
            value_type: reference("f64-vector"),
        };
        LogicalHGraphV2::new(
            vec![node.clone(), second.clone()],
            vec![edge(0, 0, 1, "input")],
            vec![LogicalOperationNodeIdV2(1)],
        )
        .unwrap();
        assert!(LogicalHGraphV2::new(
            vec![node.clone(), second.clone()],
            vec![edge(0, 0, 1, "input"), edge(1, 1, 0, "other-input")],
            vec![],
        )
        .unwrap_err()
        .to_string()
        .contains("cycle"));

        let mut third = node.clone();
        third.id = LogicalOperationNodeIdV2(2);
        assert!(LogicalHGraphV2::new(
            vec![node, second, third],
            vec![edge(0, 0, 2, "input"), edge(1, 1, 2, "input")],
            vec![LogicalOperationNodeIdV2(2)],
        )
        .is_err());
    }

    #[test]
    fn runtime_and_recovery_close_over_the_selected_plan_and_keep_priority_order() {
        let request = request_with_costs(100, 50);
        let plan = plan_operation_realization_v1(&request).unwrap();
        let (runtime, failed) = failed_runtime(&plan);
        runtime.verify_against(&plan).unwrap();
        let alternate = plan.operations[0]
            .candidates
            .iter()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Rankable)
            .unwrap()
            .candidate
            .clone();
        let recovery = RecoveryPlanV1::from_verified_runtime(
            &runtime,
            &plan,
            failed.logical_operation,
            failed.clone(),
            vec![RecoveryAlternativeV1 {
                candidate: alternate.clone(),
                condition: reference("retry-after-route-failure"),
                checkpoint: None,
            }],
        )
        .unwrap();
        recovery.verify_against(&runtime, &plan).unwrap();

        let mut other = alternate.clone();
        other.target = SemanticDigestV1::hash_bytes("test/alternate-target/v1", b"other");
        other.target_node = "other:node".to_owned();
        let ordered = vec![
            RecoveryAlternativeV1 {
                candidate: other,
                condition: reference("second-condition"),
                checkpoint: None,
            },
            RecoveryAlternativeV1 {
                candidate: alternate,
                condition: reference("first-condition"),
                checkpoint: None,
            },
        ];
        let priority = RecoveryPlanV1::new(
            runtime.id().unwrap(),
            failed.logical_operation,
            failed,
            ordered.clone(),
        )
        .unwrap();
        assert_eq!(priority.alternatives, ordered);
    }

    #[test]
    fn runtime_failure_fields_and_deployment_schedule_are_coherent() {
        let request = request_with_costs(100, 50);
        let mut plan = plan_operation_realization_v1(&request).unwrap();
        plan.schedule.clear();
        assert!(plan.validate().is_err());

        let plan = plan_operation_realization_v1(&request).unwrap();
        let selected = plan.selected_candidate().unwrap().clone();
        let bad_success = RuntimeObservationV2 {
            ordinal: 0,
            logical_operation: selected.logical_operation,
            candidate: selected,
            state: RuntimeObservationStateV2::Succeeded,
            metrics: RuntimeMetricsV2::default(),
            observed_fidelity: None,
            failure_classification: Some(reference("impossible-failure")),
            actor_generation: None,
            evidence: vec![],
            detail: None,
        };
        assert!(RuntimeGraphV2::from_deployment(&plan, vec![bad_success]).is_err());
    }

    #[test]
    fn runtime_actor_generation_may_become_known_once_but_never_drift() {
        let request = request_with_costs(100, 50);
        let plan = plan_operation_realization_v1(&request).unwrap();
        let candidate = plan.selected_candidate().unwrap().clone();
        let generation_one =
            SemanticDigestV1::hash_bytes("test/actor-generation/v1", b"generation-one");
        let generation_two =
            SemanticDigestV1::hash_bytes("test/actor-generation/v1", b"generation-two");
        let observation = |ordinal, state, actor_generation| RuntimeObservationV2 {
            ordinal,
            logical_operation: candidate.logical_operation,
            candidate: candidate.clone(),
            state,
            metrics: RuntimeMetricsV2::default(),
            observed_fidelity: None,
            failure_classification: None,
            actor_generation,
            evidence: vec![],
            detail: None,
        };

        let lifecycle = vec![
            observation(0, RuntimeObservationStateV2::Proposed, None),
            observation(
                1,
                RuntimeObservationStateV2::Started,
                Some(generation_one.clone()),
            ),
            observation(
                2,
                RuntimeObservationStateV2::Succeeded,
                Some(generation_one.clone()),
            ),
        ];
        RuntimeGraphV2::from_deployment(&plan, lifecycle.clone()).unwrap();

        let mut changed = lifecycle.clone();
        changed[2].actor_generation = Some(generation_two);
        assert!(RuntimeGraphV2::from_deployment(&plan, changed).is_err());

        let mut forgotten = lifecycle;
        forgotten[2].actor_generation = None;
        assert!(RuntimeGraphV2::from_deployment(&plan, forgotten).is_err());
    }

    fn complete_record_coordinate_fixture() -> (
        OperationPlanningRequestV1,
        DeploymentPlanV2,
        RuntimeGraphV2,
        RecoveryPlanV1,
        TransferPlanV1,
    ) {
        let request = request_with_costs(100, 50);
        let plan = plan_operation_realization_v1(&request).unwrap();
        let (runtime, failed) = failed_runtime(&plan);
        let alternate = plan.operations[0]
            .candidates
            .iter()
            .find(|assessment| assessment.disposition == CandidateDispositionV1::Rankable)
            .unwrap()
            .candidate
            .clone();
        let recovery = RecoveryPlanV1::from_verified_runtime(
            &runtime,
            &plan,
            failed.logical_operation,
            failed,
            vec![RecoveryAlternativeV1 {
                candidate: alternate,
                condition: reference("recover-condition"),
                checkpoint: None,
            }],
        )
        .unwrap();

        // TransferPlanV1 is a general-graph coordinate even though the V1
        // operation-planning request above is deliberately single-node. Bind
        // the golden transfer to a separate valid graph containing edge zero,
        // rather than freezing a syntactically valid but orphaned edge claim.
        let producer = request.graph.operations[0].clone();
        let mut consumer = producer.clone();
        consumer.id = LogicalOperationNodeIdV2(1);
        let transfer_graph = LogicalHGraphV2::new(
            vec![producer, consumer],
            vec![LogicalEdgeV2 {
                id: LogicalEdgeIdV2(0),
                producer: LogicalEdgeEndpointV2 {
                    operation: LogicalOperationNodeIdV2(0),
                    port: token("output"),
                },
                consumer: LogicalEdgeEndpointV2 {
                    operation: LogicalOperationNodeIdV2(1),
                    port: token("input"),
                },
                value_type: request.interface.outputs[0].value_type.clone(),
            }],
            vec![LogicalOperationNodeIdV2(1)],
        )
        .unwrap();
        let transfer = TransferPlanV1::new(
            transfer_graph.id().unwrap(),
            LogicalEdgeIdV2(0),
            request.offers[0].target_digest().unwrap(),
            request.offers[1].target_digest().unwrap(),
            request.offers[0].outputs[0].representation.id().unwrap(),
            request.offers[1].inputs[0].representation.id().unwrap(),
            artifact_id_for_bytes(b"transfer-adapter"),
            token("local-copy"),
            32,
            5,
        )
        .unwrap();
        (request, plan, runtime, recovery, transfer)
    }

    fn record_coordinate<T, D>(
        name: &str,
        record: &T,
        typed_id: D,
        facet_content: Option<&ArtifactId>,
    ) -> String
    where
        T: CanonicalRealizationPlanRecordV1,
        D: fmt::Display,
    {
        let bytes = canonical_bytes(record).unwrap();
        let raw_content = artifact_id_for_bytes(&bytes);
        if let Some(content) = facet_content {
            assert_eq!(content, &raw_content);
        }
        format!(
            "{name}|schema={}|canonical_len={}|canonical_sha256={}|typed_id={typed_id}|raw_facet_sha256={}",
            T::SCHEMA,
            bytes.len(),
            raw_content.as_sha256(),
            facet_content.map_or("none", ArtifactId::as_sha256),
        )
    }

    #[test]
    fn all_new_record_coordinates_match_frozen_golden_vector() {
        let (request, plan, runtime, recovery, transfer) = complete_record_coordinate_fixture();
        let representation = &request.offers[0].inputs[0].representation;
        let cost = &request.offers[0].cost_profile;
        let objective = &request.objective;
        let graph = &request.graph;

        let representation_facet = representation
            .facet_ref(FacetIdV1::new("golden-representation").unwrap())
            .unwrap();
        let transfer_facet = transfer
            .facet_ref(FacetIdV1::new("golden-transfer").unwrap())
            .unwrap();
        let cost_facet = cost
            .facet_ref(FacetIdV1::new("golden-cost").unwrap())
            .unwrap();
        let objective_facet = objective
            .facet_ref(FacetIdV1::new("golden-objective").unwrap())
            .unwrap();
        let graph_facet = graph
            .facet_ref(FacetIdV1::new("golden-graph").unwrap())
            .unwrap();
        let plan_facet = plan
            .facet_ref(FacetIdV1::new("golden-deployment").unwrap())
            .unwrap();
        let runtime_facet = runtime
            .facet_ref(FacetIdV1::new("golden-runtime").unwrap())
            .unwrap();
        let recovery_facet = recovery
            .facet_ref(FacetIdV1::new("golden-recovery").unwrap())
            .unwrap();

        // A raw canonical-content digest plus the byte length pins the exact
        // canonical bytes compactly, while the typed ID separately pins each
        // record's domain separation.  The request intentionally has no facet.
        let actual = [
            record_coordinate(
                "PhysicalRepresentationV1",
                representation,
                representation.id().unwrap(),
                Some(&representation_facet.content),
            ),
            record_coordinate(
                "TransferPlanV1",
                &transfer,
                transfer.id().unwrap(),
                Some(&transfer_facet.content),
            ),
            record_coordinate(
                "CostProfileV1",
                cost,
                cost.id().unwrap(),
                Some(&cost_facet.content),
            ),
            record_coordinate(
                "ObjectiveV1",
                objective,
                objective.id().unwrap(),
                Some(&objective_facet.content),
            ),
            record_coordinate(
                "LogicalHGraphV2",
                graph,
                graph.id().unwrap(),
                Some(&graph_facet.content),
            ),
            record_coordinate(
                "DeploymentPlanV2",
                &plan,
                plan.id().unwrap(),
                Some(&plan_facet.content),
            ),
            record_coordinate(
                "RuntimeGraphV2",
                &runtime,
                runtime.id().unwrap(),
                Some(&runtime_facet.content),
            ),
            record_coordinate(
                "RecoveryPlanV1",
                &recovery,
                recovery.id().unwrap(),
                Some(&recovery_facet.content),
            ),
            record_coordinate(
                "OperationPlanningRequestV1",
                &request,
                request.id().unwrap(),
                None,
            ),
        ]
        .join("\n");

        const EXPECTED: &str = r#"PhysicalRepresentationV1|schema=ostadix.physical-representation/v1|canonical_len=347|canonical_sha256=c944e8a991577c63d4cc5ee5529544ba228d04ab856d244e4aefd4c8090bd54d|typed_id=physical-representation:sha256:c1d94af6c26763585143e5039d7245a3c7b96fa71e3340da5ba4d87f58dc4ee6|raw_facet_sha256=c944e8a991577c63d4cc5ee5529544ba228d04ab856d244e4aefd4c8090bd54d
TransferPlanV1|schema=ostadix.transfer-plan/v1|canonical_len=600|canonical_sha256=fdb58c365e0db131f7fcc0b7f2ff698fe221b56e1d1f2ae3f2732cd4e156d353|typed_id=transfer-plan:sha256:9fb009fb9d236e91663d4413de48ecf7f4f0ee885d7f19b91f7fd6d02966eccc|raw_facet_sha256=fdb58c365e0db131f7fcc0b7f2ff698fe221b56e1d1f2ae3f2732cd4e156d353
CostProfileV1|schema=ostadix.cost-profile/v1|canonical_len=981|canonical_sha256=a59ddde3121219c6d84bdf656d5d2b6b46a277001b86de8d2d00ce9bce4f163e|typed_id=cost-profile:sha256:bb635c7b98b0cdfe83ad0846e79400fbd27535d5482766b3a3e40de2387b3d26|raw_facet_sha256=a59ddde3121219c6d84bdf656d5d2b6b46a277001b86de8d2d00ce9bce4f163e
ObjectiveV1|schema=ostadix.objective/v1|canonical_len=241|canonical_sha256=e9b1a84938fa8089887d2257a6a529d19c9dff370e66953e552f1cc1b1f35348|typed_id=objective:sha256:6f1725ff1815e11aa0d9332cd19890979a19d12b4ec789bdcc39c488373fd9da|raw_facet_sha256=e9b1a84938fa8089887d2257a6a529d19c9dff370e66953e552f1cc1b1f35348
LogicalHGraphV2|schema=ostadix.logical-hgraph/v2|canonical_len=429|canonical_sha256=78c1301b7be20c50f3af755d1a49b27b0ce1dea44bfe72b7bb11dd7d34e6fb4f|typed_id=logical-hgraph-v2:sha256:876c8b6cbf28217804ae3340b0a272e700ae174d81273f854eee6be05a084f34|raw_facet_sha256=78c1301b7be20c50f3af755d1a49b27b0ce1dea44bfe72b7bb11dd7d34e6fb4f
DeploymentPlanV2|schema=ostadix.deployment-plan/v2|canonical_len=3426|canonical_sha256=c3646a56e2c1b96d240ccebfe9c19b274ee1dbdde7b41a6f7908184f48f3d689|typed_id=deployment-plan-v2:sha256:c643fd2c70430af0e0ef76d92dee904b7f676a0a4c82b3e2f39b29dd7ffb4c86|raw_facet_sha256=c3646a56e2c1b96d240ccebfe9c19b274ee1dbdde7b41a6f7908184f48f3d689
RuntimeGraphV2|schema=ostadix.runtime-graph/v2|canonical_len=1364|canonical_sha256=5a0b67c9f3f0a0659134b92be7558438706558c7e1325f9071bbffd93eaf24a5|typed_id=runtime-graph-v2:sha256:f48852b8072a2d14190a5c262e862c60a228b5466b44593d4cda78957b0c3f3c|raw_facet_sha256=5a0b67c9f3f0a0659134b92be7558438706558c7e1325f9071bbffd93eaf24a5
RecoveryPlanV1|schema=ostadix.recovery-plan/v1|canonical_len=1480|canonical_sha256=e0800bc950ac91394965a5fa0a0e669d7cd226139839b4cdf56e656934f799b6|typed_id=recovery-plan:sha256:e2068734f4f8db59cef84899b3014e69a80942a11a833e2ad6f500ffa7272c74|raw_facet_sha256=e0800bc950ac91394965a5fa0a0e669d7cd226139839b4cdf56e656934f799b6
OperationPlanningRequestV1|schema=ostadix.operation-planning-request/v1|canonical_len=9908|canonical_sha256=3aed1b218840b42e64f48bd9d589946c024fb947a2b1fc03fe64ac1941b07a7f|typed_id=operation-planning-request:sha256:21562651412f869a6132fda871a0dad0bf870d107c5bbb0278e1606f5cc4cc15|raw_facet_sha256=none"#;
        assert_eq!(actual, EXPECTED, "captured golden vector:\n{actual}");
    }

    #[test]
    fn raw_content_facets_cover_new_records_and_existing_graph_kinds() {
        let (request, plan, runtime, recovery, transfer) = complete_record_coordinate_fixture();
        let representation = &request.offers[0].inputs[0].representation;
        let cost = &request.offers[0].cost_profile;
        let cases = vec![
            (
                request
                    .graph
                    .facet_ref(FacetIdV1::new("logical-graph").unwrap())
                    .unwrap(),
                FacetKindV1::LogicalHgraph,
                LOGICAL_HGRAPH_SCHEMA_V2,
                request.graph.canonical_bytes().unwrap(),
            ),
            (
                plan.facet_ref(FacetIdV1::new("deployment").unwrap())
                    .unwrap(),
                FacetKindV1::Deployment,
                DEPLOYMENT_PLAN_SCHEMA_V2,
                plan.canonical_bytes().unwrap(),
            ),
            (
                runtime
                    .facet_ref(FacetIdV1::new("runtime-graph").unwrap())
                    .unwrap(),
                FacetKindV1::RuntimeGraph,
                RUNTIME_GRAPH_SCHEMA_V2,
                runtime.canonical_bytes().unwrap(),
            ),
            (
                representation
                    .facet_ref(FacetIdV1::new("representation").unwrap())
                    .unwrap(),
                FacetKindV1::PhysicalRepresentation,
                PHYSICAL_REPRESENTATION_SCHEMA_V1,
                representation.canonical_bytes().unwrap(),
            ),
            (
                transfer
                    .facet_ref(FacetIdV1::new("transfer").unwrap())
                    .unwrap(),
                FacetKindV1::TransferPlan,
                TRANSFER_PLAN_SCHEMA_V1,
                transfer.canonical_bytes().unwrap(),
            ),
            (
                cost.facet_ref(FacetIdV1::new("cost").unwrap()).unwrap(),
                FacetKindV1::CostProfile,
                COST_PROFILE_SCHEMA_V1,
                cost.canonical_bytes().unwrap(),
            ),
            (
                request
                    .objective
                    .facet_ref(FacetIdV1::new("objective").unwrap())
                    .unwrap(),
                FacetKindV1::Objective,
                OBJECTIVE_SCHEMA_V1,
                request.objective.canonical_bytes().unwrap(),
            ),
            (
                recovery
                    .facet_ref(FacetIdV1::new("recovery").unwrap())
                    .unwrap(),
                FacetKindV1::RecoveryPlan,
                RECOVERY_PLAN_SCHEMA_V1,
                recovery.canonical_bytes().unwrap(),
            ),
        ];
        for (facet, kind, schema, bytes) in cases {
            assert_eq!(facet.kind, kind);
            assert_eq!(facet.schema.as_str(), schema);
            assert_eq!(facet.content, artifact_id_for_bytes(&bytes));
        }
    }
}
